//! Session traces: request/response, gaps, 0x78, TesterPresent, Stop, policy before Write.

mod common;

use clio_core::frame::{checksum, FrameError, Reply};
use clio_core::init::InitOutcome;
use clio_core::safety::DenyReason;
use clio_core::session::{Session, SessionError, SessionOutcome};
use clio_core::{InitKind, InitVariant, KTiming, LinkConfig, LinkError, Step};
use common::*;

fn sirius_cfg() -> LinkConfig {
    LinkConfig::new(
        0x7A,
        KTiming {
            init: InitKind::KwpFiveBaud { multipoint: true },
            ..KTiming::default()
        },
    )
}

fn outcome(kind: InitKind, kw1: u8, kw2: u8) -> InitOutcome {
    InitOutcome {
        kind,
        address_byte: None,
        sync: Some(0x55),
        kw1,
        kw2,
        address_complement: None,
        keywords_match: None,
    }
}

fn sirius_session(now: u64) -> Session {
    let cfg = sirius_cfg();
    Session::new(&cfg, &outcome(cfg.timing.init, 0xD0, 0x8F), now)
}

/// ECU -> tool KWP frame from 0x7A.
fn from_ecu(payload: &[u8]) -> Vec<u8> {
    let mut f = vec![0x80 | payload.len() as u8, 0xF1, 0x7A];
    f.extend_from_slice(payload);
    f.push(checksum(&f));
    f
}

/// Reply read: first byte, then the rest.
fn reply_reads(frame: &[u8]) -> Script {
    vec![
        (read(1, 300), ok(&frame[..1])),
        (read(frame.len() - 1, 300), ok(&frame[1..])),
    ]
}

/// Feed `script` and record `now` for every event (constant here; the gap tests vary it).
fn run(
    s: &mut Session,
    first: clio_core::Action,
    script: &Script,
    now: u64,
) -> Step<SessionOutcome, SessionError> {
    drive(first, script, |ev| s.on_event(ev, now))
}

const REQ_21A0: [u8; 6] = [0x82, 0x7A, 0xF1, 0x21, 0xA0, 0xAE];

#[test]
fn sirius_read_request() {
    let mut s = sirius_session(1000);
    let first = s.request(&[0x21, 0xA0], 1000).unwrap();
    let reply = from_ecu(&[0x61, 0xA0, 0x12, 0x34]);
    let mut script = vec![
        (flush(), done()),
        // Single ECU: XML inter-byte 0 -> one write.
        (write(&REQ_21A0, 0), done()),
        (read(6, 300), ok(&REQ_21A0)),
    ];
    script.extend(reply_reads(&reply));
    assert_eq!(
        run(&mut s, first, &script, 1100),
        Step::Done(SessionOutcome::Reply(Reply::Positive(vec![
            0x61, 0xA0, 0x12, 0x34
        ])))
    );
    assert!(!s.is_busy());
}

#[test]
fn p3_gap_is_owed_from_last_reply() {
    let mut s = sirius_session(0);
    let first = s.request(&[0x21, 0xA0], 0).unwrap();
    let mut script = vec![
        (flush(), done()),
        (write(&REQ_21A0, 0), done()),
        (read(6, 300), ok(&REQ_21A0)),
    ];
    script.extend(reply_reads(&from_ecu(&[0x61, 0xA0])));
    assert!(matches!(run(&mut s, first, &script, 100), Step::Done(_)));
    // 2 ms after the reply: 3 ms of the 5 ms ReplyToRequest gap are still owed.
    assert_eq!(s.request(&[0x21, 0xA1], 102).unwrap(), sleep(3));
}

#[test]
fn no_gap_when_already_idle_long_enough() {
    let mut s = sirius_session(0);
    let first = s.request(&[0x21, 0xA0], 0).unwrap();
    let mut script = vec![
        (flush(), done()),
        (write(&REQ_21A0, 0), done()),
        (read(6, 300), ok(&REQ_21A0)),
    ];
    script.extend(reply_reads(&from_ecu(&[0x61, 0xA0])));
    assert!(matches!(run(&mut s, first, &script, 100), Step::Done(_)));
    assert_eq!(s.request(&[0x21, 0xA1], 200).unwrap(), flush());
}

#[test]
fn on_error_gap_after_no_reply() {
    let mut s = sirius_session(0);
    let first = s.request(&[0x21, 0xA0], 0).unwrap();
    let script = vec![
        (flush(), done()),
        (write(&REQ_21A0, 0), done()),
        (read(6, 300), ok(&REQ_21A0)),
        (read(1, 300), timeout(&[])),
    ];
    assert_eq!(
        run(&mut s, first, &script, 400),
        Step::Failed(SessionError::Link(LinkError::NoReply))
    );
    // OnError 300 ms from the failure.
    assert_eq!(s.request(&[0x21, 0xA0], 400).unwrap(), sleep(300));
}

#[test]
fn negative_reply_and_on_error_gap() {
    let mut s = sirius_session(0);
    let first = s.request(&[0x21, 0xA0], 0).unwrap();
    let mut script = vec![
        (flush(), done()),
        (write(&REQ_21A0, 0), done()),
        (read(6, 300), ok(&REQ_21A0)),
    ];
    script.extend(reply_reads(&from_ecu(&[0x7F, 0x21, 0x12])));
    assert_eq!(
        run(&mut s, first, &script, 50),
        Step::Done(SessionOutcome::Reply(Reply::Negative {
            sid: 0x21,
            nrc: 0x12
        }))
    );
    assert_eq!(s.request(&[0x21, 0xA0], 50).unwrap(), sleep(300));
}

#[test]
fn response_pending_keeps_reading() {
    let mut s = sirius_session(0);
    let first = s.request(&[0x21, 0xA0], 0).unwrap();
    let pending = from_ecu(&[0x7F, 0x21, 0x78]);
    let fin = from_ecu(&[0x61, 0xA0, 0x01]);
    let mut script = vec![
        (flush(), done()),
        (write(&REQ_21A0, 0), done()),
        (read(6, 300), ok(&REQ_21A0)),
    ];
    script.extend(reply_reads(&pending));
    // No new write; first byte of the next frame within the P2* timeout.
    script.push((read(1, 5000), ok(&fin[..1])));
    script.push((read(fin.len() - 1, 300), ok(&fin[1..])));
    assert_eq!(
        run(&mut s, first, &script, 10),
        Step::Done(SessionOutcome::Reply(Reply::Positive(vec![
            0x61, 0xA0, 0x01
        ])))
    );
}

#[test]
fn response_pending_is_bounded() {
    let mut cfg = sirius_cfg();
    cfg.max_response_pending = 2;
    let mut s = Session::new(&cfg, &outcome(cfg.timing.init, 0xD0, 0x8F), 0);
    let first = s.request(&[0x21, 0xA0], 0).unwrap();
    let pending = from_ecu(&[0x7F, 0x21, 0x78]);
    let mut script = vec![
        (flush(), done()),
        (write(&REQ_21A0, 0), done()),
        (read(6, 300), ok(&REQ_21A0)),
    ];
    script.extend(reply_reads(&pending));
    for _ in 0..2 {
        script.push((read(1, 5000), ok(&pending[..1])));
        script.push((read(pending.len() - 1, 300), ok(&pending[1..])));
    }
    assert_eq!(
        run(&mut s, first, &script, 10),
        Step::Failed(SessionError::Link(LinkError::TooManyPending { count: 3 }))
    );
}

#[test]
fn echo_with_break_residue() {
    let mut s = sirius_session(0);
    let first = s.request(&[0x21, 0xA0], 0).unwrap();
    let mut rx = vec![0x00];
    rx.extend_from_slice(&REQ_21A0[..5]);
    let mut script = vec![
        (flush(), done()),
        (write(&REQ_21A0, 0), done()),
        (read(6, 300), ok(&rx)),
        (read(1, 300), ok(&REQ_21A0[5..])),
    ];
    script.extend(reply_reads(&from_ecu(&[0x61, 0xA0])));
    assert!(matches!(
        run(&mut s, first, &script, 10),
        Step::Done(SessionOutcome::Reply(Reply::Positive(_)))
    ));
}

#[test]
fn echo_collision() {
    let mut s = sirius_session(0);
    let first = s.request(&[0x21, 0xA0], 0).unwrap();
    let bad = [0x82, 0x7A, 0xF1, 0x21, 0xA1, 0xAE];
    let script = vec![
        (flush(), done()),
        (write(&REQ_21A0, 0), done()),
        (read(6, 300), ok(&bad)),
    ];
    assert_eq!(
        run(&mut s, first, &script, 10),
        Step::Failed(SessionError::Link(LinkError::Collision {
            sent: REQ_21A0.to_vec(),
            echo: bad.to_vec()
        }))
    );
}

#[test]
fn incomplete_and_bad_replies() {
    let mut s = sirius_session(0);
    let first = s.request(&[0x21, 0xA0], 0).unwrap();
    let reply = from_ecu(&[0x61, 0xA0, 0x01]);
    let script = vec![
        (flush(), done()),
        (write(&REQ_21A0, 0), done()),
        (read(6, 300), ok(&REQ_21A0)),
        (read(1, 300), ok(&reply[..1])),
        (read(reply.len() - 1, 300), timeout(&reply[1..3])),
    ];
    assert_eq!(
        run(&mut s, first, &script, 10),
        Step::Failed(SessionError::Link(LinkError::IncompleteReply {
            received: reply[..3].to_vec()
        }))
    );

    let first = s.request(&[0x21, 0xA0], 1000).unwrap();
    let mut bad = reply.clone();
    *bad.last_mut().unwrap() ^= 0xFF;
    let mut script = vec![
        (flush(), done()),
        (write(&REQ_21A0, 0), done()),
        (read(6, 300), ok(&REQ_21A0)),
    ];
    script.extend(reply_reads(&bad));
    assert!(matches!(
        run(&mut s, first, &script, 1100),
        Step::Failed(SessionError::Frame(FrameError::Checksum { .. }))
    ));
}

#[test]
fn separate_length_byte_reply() {
    let mut s = sirius_session(0);
    let first = s.request(&[0x21, 0xA0], 0).unwrap();
    let mut reply = vec![0x80, 0xF1, 0x7A, 0x02, 0x61, 0xA0];
    reply.push(checksum(&reply));
    let script = vec![
        (flush(), done()),
        (write(&REQ_21A0, 0), done()),
        (read(6, 300), ok(&REQ_21A0)),
        (read(1, 300), ok(&reply[..1])),
        (read(3, 300), ok(&reply[1..4])),
        (read(3, 300), ok(&reply[4..])),
    ];
    assert_eq!(
        run(&mut s, first, &script, 10),
        Step::Done(SessionOutcome::Reply(Reply::Positive(vec![0x61, 0xA0])))
    );
}

#[test]
fn denied_request_writes_nothing() {
    let mut s = sirius_session(0);
    for payload in [
        &[0x14, 0x00, 0x00][..],
        &[0x11, 0xFF],
        &[0x30, 0xD0, 0x00, 0x0A],
        &[0x31, 0xA2, 0x01, 0x00, 0x00],
        &[0x10, 0x85],
        &[0x3B, 0xD0, 0x10],
    ] {
        let e = s.request(payload, 0).unwrap_err();
        assert!(
            matches!(e, SessionError::Denied(ref d) if d.payload == payload),
            "{payload:02X?}: {e:?}"
        );
        // Nothing queued: the session is free and owes no event.
        assert!(!s.is_busy());
    }
    // An approved-routine payload without Approval is refused too.
    assert!(matches!(
        s.request(&[0x31, 0xC7, 0, 0, 0], 0),
        Err(SessionError::Denied(ref d)) if d.reason == DenyReason::ApprovalRequired
    ));
}

#[test]
fn busy_and_closed() {
    let mut s = sirius_session(0);
    let first = s.request(&[0x21, 0xA0], 0).unwrap();
    assert_eq!(first, flush());
    assert_eq!(s.request(&[0x21, 0xA0], 0), Err(SessionError::Busy));
    assert_eq!(s.tester_present(0), Err(SessionError::Busy));
    assert!(!s.tester_present_due(10_000));
}

#[test]
fn tester_present_due_at_once_after_five_baud_then_every_3s() {
    let mut s = sirius_session(1000);
    // RE: IOTBD32V3: after a 5-baud init the TP timer is 0.
    assert!(s.tester_present_due(1000));
    let first = s.tester_present(1000).unwrap();
    let tp = [0x81, 0x7A, 0xF1, 0x3E, 0x2A];
    let script = vec![
        (flush(), done()),
        (write(&tp, 0), done()),
        (read(5, 300), ok(&tp)),
        // No reply is fine for TesterPresent.
        (read(1, 300), timeout(&[])),
    ];
    assert_eq!(
        run(&mut s, first, &script, 1300),
        Step::Done(SessionOutcome::TesterPresent(None))
    );
    assert!(!s.tester_present_due(4299));
    assert!(s.tester_present_due(4300));
}

#[test]
fn tester_present_reply_is_read_when_present() {
    let mut s = sirius_session(0);
    let first = s.tester_present(0).unwrap();
    let tp = [0x81, 0x7A, 0xF1, 0x3E, 0x2A];
    let mut script = vec![
        (flush(), done()),
        (write(&tp, 0), done()),
        (read(5, 300), ok(&tp)),
    ];
    script.extend(reply_reads(&from_ecu(&[0x7E])));
    assert_eq!(
        run(&mut s, first, &script, 10),
        Step::Done(SessionOutcome::TesterPresent(Some(Reply::Positive(vec![
            0x7E
        ]))))
    );
}

#[test]
fn tester_present_after_fast_init_waits_tp_minus_first_request() {
    let cfg = LinkConfig::new(
        0x7A,
        KTiming {
            init: InitKind::KwpFast { multipoint: false },
            ..KTiming::default()
        },
    );
    let s = Session::new(&cfg, &outcome(cfg.timing.init, 0xD0, 0x8F), 0);
    assert!(!s.tester_present_due(2979));
    assert!(s.tester_present_due(2980));
}

#[test]
fn stop_closes_session() {
    let mut s = sirius_session(0);
    let first = s.stop(0).unwrap();
    let stop = [0x81, 0x7A, 0xF1, 0x82, 0x6E];
    let mut script = vec![
        (flush(), done()),
        (write(&stop, 0), done()),
        (read(5, 300), ok(&stop)),
    ];
    script.extend(reply_reads(&from_ecu(&[0xC2])));
    assert_eq!(
        run(&mut s, first, &script, 10),
        Step::Done(SessionOutcome::Stopped(Some(Reply::Positive(vec![0xC2]))))
    );
    assert!(s.is_closed());
    assert_eq!(s.request(&[0x21, 0xA0], 100), Err(SessionError::Closed));
    assert!(!s.tester_present_due(100_000));
}

#[test]
fn safir_iso8_session() {
    let cfg = LinkConfig::new(
        0x7A,
        KTiming {
            init: InitKind::Iso8FiveBaud,
            ..KTiming::default()
        },
    );
    let mut s = Session::new(&cfg, &outcome(InitKind::Iso8FiveBaud, 0xC2, 0x85), 0);
    let first = s.request(&[0x33, 0x50], 0).unwrap();
    let req = [0x03, 0x33, 0x50, 0x86];
    let reply = [0x03, 0x73, 0x50, 0xC6];
    let script = vec![
        (flush(), done()),
        // Not Multipoint: InterByte from the XML (0) -> one write.
        (write(&req, 0), done()),
        (read(4, 300), ok(&req)),
        (read(1, 300), ok(&reply[..1])),
        (read(3, 300), ok(&reply[1..])),
    ];
    assert_eq!(
        run(&mut s, first, &script, 10),
        Step::Done(SessionOutcome::Reply(Reply::Positive(vec![0x73, 0x50])))
    );
    // ISO8 keep-alive is {36}.
    let first = s.tester_present(100).unwrap();
    let script = vec![
        (flush(), done()),
        (write(&[0x02, 0x36, 0x38], 0), done()),
        (read(3, 300), ok(&[0x02, 0x36, 0x38])),
        (read(1, 300), timeout(&[])),
    ];
    assert_eq!(
        run(&mut s, first, &script, 400),
        Step::Done(SessionOutcome::TesterPresent(None))
    );
    // KWP services are not on the ISO8 allowlist.
    assert!(matches!(
        s.request(&[0x10, 0x81], 500),
        Err(SessionError::Denied(_))
    ));
}

#[test]
fn length_byte_framing_when_kw1_requires_it() {
    // KW1 0xC2 (bit 0 clear, not 0xD0): KWP length goes in a separate byte.
    let cfg = sirius_cfg();
    let mut s = Session::new(&cfg, &outcome(cfg.timing.init, 0xC2, 0x8F), 0);
    let req = [0x80, 0x7A, 0xF1, 0x02, 0x21, 0xA0, 0xAE];
    assert_eq!(s.request(&[0x21, 0xA0], 0).unwrap(), flush());
    assert_eq!(s.on_event(done(), 0), Step::Act(write(&req, 0)));
}

#[test]
fn multi_ecu_session_uses_7ms_inter_byte() {
    let mut cfg = sirius_cfg();
    cfg.variant = InitVariant::MultiEcu;
    let mut s = Session::new(
        &cfg,
        &outcome(InitKind::KwpFiveBaud { multipoint: true }, 0xD0, 0x8F),
        0,
    );
    assert_eq!(s.request(&[0x21, 0xA0], 0).unwrap(), flush());
    assert_eq!(s.on_event(done(), 0), Step::Act(write(&REQ_21A0, 7)));
}
