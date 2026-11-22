//! Full sessions through kline-core's machines against the in-process simulator.

mod common;

use common::*;
use kline_core::frame::Reply;
use kline_core::safety::DenyReason;
use kline_core::session::{SessionError, SessionOutcome};
use kline_sim::ecu::{EcuProfile, SpecEcu};
use kline_sim::journal::WriteKind;
use kline_sim::line::Line;
use kline_sim::scenario::Scenario;
use kline_spec::Value;

#[test]
fn sirius_full_session_round_trips_scenario() {
    let def = sirius();
    let sc = sirius_scenario();
    let (mut d, journal) = drive(sirius_ecu());
    let cfg = cfg(&def);

    let init = d.init(&cfg).unwrap();
    assert_eq!(init.sync(), Some(0x55));
    assert_eq!(init.keywords(), (0xD0, 0x8F));
    assert_eq!(init.keywords_match(), Some(true));
    assert_eq!(init.address_byte(), Some(0x7A));
    assert_eq!(init.address_complement(), Some(0x85));
    let mut s = d.session(init);

    assert_eq!(
        positive(&d.request(&mut s, &[0x10, 0xA0]).unwrap()),
        [0x50, 0xA0]
    );

    let ident = positive(&d.request(&mut s, &[0x21, 0x80]).unwrap());
    let got = decode(&def, "Identification", &ident);
    assert_eq!(got["Calibration"], Value::Text("SIM-0001".into()));
    assert_eq!(got["Supplier code"], Value::Hex("0A1B".into()));

    for name in ["Live A9", "Live AA"] {
        let lid = def.request(name).unwrap().sent_bytes.0.clone();
        let reply = positive(&d.request(&mut s, &lid).unwrap());
        assert_eq!(
            decode(&def, name, &reply),
            expected(&def, name, &sc),
            "{name}"
        );
    }

    assert_eq!(
        d.tester_present(&mut s).unwrap(),
        SessionOutcome::TesterPresent(Some(Reply::Positive(vec![0x7E])))
    );
    assert_eq!(
        d.stop(&mut s).unwrap(),
        SessionOutcome::Stopped(Some(Reply::Positive(vec![0xC2])))
    );
    assert!(s.is_closed());
    journal.assert_no_writes();
    // Strict tolerance: the core met every ISO window the simulator checks.
    journal.assert_no_violations();
}

#[test]
fn ecu_sleeps_after_stop() {
    use kline_sim::frame::checksum;
    let def = sirius();
    let (mut d, journal) = drive(sirius_ecu());
    let cfg = cfg(&def);
    let init = d.init(&cfg).unwrap();
    let mut s = d.session(init);
    d.stop(&mut s).unwrap();
    // The closed session refuses further requests (one init, one session) ...
    assert!(
        matches!(
            d.request(&mut s, &[0x21, 0xA9]).unwrap_err().session(),
            Some(SessionError::Closed)
        ),
        "a stopped session must refuse requests"
    );
    // ... and the ECU itself is asleep: a well-formed 21 A9 frame written directly on the line
    // gets only the cable echo back, no reply.
    d.line.sleep_ms(100); // well past P3min, so silence is not a timing rejection
    let mut f = vec![0x82, 0x7A, 0xF1, 0x21, 0xA9];
    f.push(checksum(&f));
    d.line.flush_rx().unwrap();
    d.line.write(&f, 0).unwrap();
    assert_eq!(d.line.read(64, 1_000).unwrap(), f, "only the echo");
    journal.assert_no_writes();
    journal.assert_no_violations();
}

#[test]
fn tester_present_keeps_session_and_silence_ends_it() {
    let def = sirius();
    let cfg = cfg(&def);
    // Alive: TesterPresent every 3 s for 12 s.
    let (mut d, _j) = drive(sirius_ecu());
    let init = d.init(&cfg).unwrap();
    let mut s = d.session(init);
    for _ in 0..4 {
        d.line.sleep_ms(3_000);
        assert!(s.tester_present_due(d.line.now_ms()));
        d.tester_present(&mut s).unwrap();
    }
    positive(&d.request(&mut s, &[0x21, 0xA9]).unwrap());
    // Silent for 6 s > P3max 5000: the ECU has gone to sleep.
    d.line.sleep_ms(6_000);
    let e = d.request(&mut s, &[0x21, 0xA9]).unwrap_err();
    assert!(
        matches!(
            e.session(),
            Some(SessionError::Link(kline_core::LinkError::NoReply))
        ),
        "{e:?}"
    );
}

#[test]
fn bmt_21a3_read_and_shared_2180() {
    let def = bmt();
    let sc = Scenario::new()
        .text("Remote frame received", "yes")
        .text("Remote frame valid", "yes")
        .text("Remote code accepted", "no")
        .number("Remote count", 2.0)
        .text("Part number", "770012")
        .text("Board type", "type C");
    let ecu = SpecEcu::new(EcuProfile::bmt(), def.clone(), sc.clone()).unwrap();
    let (mut d, journal) = drive(ecu);
    let cfg = cfg(&def);
    let init = d.init(&cfg).unwrap();
    assert_eq!(init.address_complement(), Some(0xD9));
    let mut s = d.session(init);
    let r = positive(&d.request(&mut s, &[0x21, 0xA3]).unwrap());
    assert_eq!(r[..2], [0x61, 0xA3]);
    assert_eq!(
        decode(&def, "Remote status", &r),
        expected(&def, "Remote status", &sc)
    );
    // Two definitions share 2180; the one reply carries both.
    let r = positive(&d.request(&mut s, &[0x21, 0x80]).unwrap());
    assert_eq!(
        decode(&def, "Identification", &r),
        expected(&def, "Identification", &sc)
    );
    assert_eq!(decode(&def, "Board", &r), expected(&def, "Board", &sc));
    d.stop(&mut s).unwrap();
    journal.assert_no_writes();
    journal.assert_no_violations();
}

#[test]
fn bmt_transponder_write_is_denied_before_the_wire() {
    let def = bmt();
    let ecu = SpecEcu::new(EcuProfile::bmt(), def.clone(), Scenario::new()).unwrap();
    let (mut d, journal) = drive(ecu);
    let cfg = cfg(&def);
    let init = d.init(&cfg).unwrap();
    let mut s = d.session(init);
    // Transponder *status* (exactly 21 A4) is a Tier 0 read: answered, not in the write log.
    let r = positive(&d.request(&mut s, &[0x21, 0xA4]).unwrap());
    assert_eq!(r[..2], [0x61, 0xA4]);
    // Anything after 21 A4 is refused as an immobilizer request before the wire.
    let e = d.request(&mut s, &[0x21, 0xA4, 0x01]).unwrap_err();
    assert!(
        matches!(e.session(), Some(SessionError::Denied(dn)) if dn.reason == DenyReason::Immobilizer),
        "{e:?}"
    );
    // Non-read tiers, too, never reach the ECU through `request` (no approval).
    for p in [
        &[0x14, 0xFF, 0x00][..],
        &[0x31, 0xC7, 0, 0, 0],
        &[0x3B, 0xD0, 0x10, 0x00],
    ] {
        assert!(matches!(
            d.request(&mut s, p).unwrap_err().session(),
            Some(SessionError::Denied(_))
        ));
    }
    journal.assert_no_writes();
}

#[cfg(feature = "approved-routines")]
#[test]
fn approved_routine_reaches_ecu_and_is_logged() {
    use kline_core::safety::{Approval, Tier};
    let def = bmt();
    let ecu = SpecEcu::new(EcuProfile::bmt(), def.clone(), Scenario::new()).unwrap();
    let (mut d, journal) = drive(ecu);
    let cfg = cfg(&def);
    let init = d.init(&cfg).unwrap();
    let mut s = d.session(init);
    let payload = [0x31, 0xC8, 0x00, 0x00, 0x00];
    // A wrong code approves nothing and nothing reaches the ECU.
    let c = s.service_challenge(&payload).unwrap();
    assert_eq!(c.tier(), Tier::Service);
    assert!(Approval::confirm(c, "WRONG!").is_err());
    assert!(journal.writes().is_empty());
    // The right code: exactly this routine goes out once.
    let c = s.service_challenge(&payload).unwrap();
    let code = c.code().to_string();
    let a = Approval::confirm(c, &code).unwrap();
    let r = positive(&d.request_approved(&mut s, a).unwrap());
    assert_eq!(r, [0x71, 0xC8, 0x00, 0x00, 0x00]);
    let w = journal.writes();
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0].kind, WriteKind::Routine);
    assert_eq!(w[0].payload, payload);
    assert_eq!(w[0].request_names, ["Learn remote 2"]);
}

#[test]
fn write_log_catches_raw_frames_that_bypass_the_core() {
    use kline_sim::frame::checksum;
    let def = bmt();
    let ecu = SpecEcu::new(EcuProfile::bmt(), def.clone(), Scenario::new()).unwrap();
    let (mut d, journal) = drive(ecu);
    let cfg = cfg(&def);
    d.init(&cfg).unwrap();
    // A buggy tester writes 21 A4 01 and 14 FF 00 directly on the line.
    for p in [&[0x21, 0xA4, 0x01][..], &[0x14, 0xFF, 0x00]] {
        let mut f = vec![0x80 | p.len() as u8, 0x26, 0xF1];
        f.extend_from_slice(p);
        f.push(checksum(&f));
        d.line.sleep_ms(10);
        d.line.write(&f, 0).unwrap();
        let _ = d.line.read(64, 300).unwrap();
    }
    let kinds: Vec<WriteKind> = journal.writes().iter().map(|w| w.kind).collect();
    assert_eq!(kinds, [WriteKind::Immobilizer, WriteKind::ClearDtc]);
    let r = std::panic::catch_unwind(|| journal.assert_no_writes());
    assert!(r.is_err(), "assert_no_writes must panic on a non-empty log");
}

#[test]
fn unknown_requests_get_7f_11_and_7f_12() {
    let def = sirius();
    let (mut d, journal) = drive(sirius_ecu());
    let cfg = cfg(&def);
    let init = d.init(&cfg).unwrap();
    let mut s = d.session(init);
    // 1A is allowed by the core but not in the definition: serviceNotSupported.
    assert_eq!(
        d.request(&mut s, &[0x1A, 0x80]).unwrap(),
        SessionOutcome::Reply(Reply::Negative {
            sid: 0x1A,
            nrc: 0x11
        })
    );
    // 21 is known, 21 B0 is not: subFunctionNotSupported.
    assert_eq!(
        d.request(&mut s, &[0x21, 0xB0]).unwrap(),
        SessionOutcome::Reply(Reply::Negative {
            sid: 0x21,
            nrc: 0x12
        })
    );
    journal.assert_no_writes();
}

#[test]
fn safir_iso8_session() {
    let def = safir();
    let sc = Scenario::new()
        .number("Engine speed", 850.0)
        .number("Coolant temperature", 88.0);
    let ecu = SpecEcu::new(EcuProfile::safir_iso8(), def.clone(), sc.clone()).unwrap();
    let (mut d, journal) = drive(ecu);
    let cfg = cfg(&def);
    let init = d.init(&cfg).unwrap();
    assert_eq!(init.keywords(), (0xC2, 0x85));
    assert_eq!(init.address_complement(), None);
    let mut s = d.session(init);
    let r = positive(&d.request(&mut s, &[0x33, 0x50]).unwrap());
    assert_eq!(
        decode(&def, "Block 50", &r),
        expected(&def, "Block 50", &sc)
    );
    // ISO8 TesterPresent: the profile stays silent (unknown on the car), the core tolerates it.
    assert_eq!(
        d.tester_present(&mut s).unwrap(),
        SessionOutcome::TesterPresent(None)
    );
    assert_eq!(d.stop(&mut s).unwrap(), SessionOutcome::Stopped(None));
    journal.assert_no_writes();
    journal.assert_no_violations();
}

#[test]
fn scenario_is_checked_up_front() {
    let off_grid = Scenario::new().number("Engine speed", 850.06);
    assert!(SpecEcu::new(EcuProfile::sirius32(), sirius(), off_grid).is_err());
    let unknown = Scenario::new().number("No such data", 1.0);
    assert!(SpecEcu::new(EcuProfile::sirius32(), sirius(), unknown).is_err());
}
