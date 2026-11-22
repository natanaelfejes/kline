//! Capture a session on the spec-driven ECU, then replay it as the ECU.

mod common;

use clio_core::session::{SessionError, SessionOutcome};
use clio_core::{LinkConfig, LinkError};
use clio_sim::bus::BusConfig;
use clio_sim::driver::Driver;
use clio_sim::journal::ViolationKind;
use clio_sim::line::{Line, VirtualLine};
use clio_sim::replay::{read_jsonl, write_jsonl, Dir, EntryKind, LogEntry, ReplayEcu};
use clio_sim::timing::Tolerance;
use common::*;

const SCRIPT: [&[u8]; 4] = [&[0x10, 0xA0], &[0x21, 0x80], &[0x21, 0xA9], &[0x21, 0xAA]];

/// Init, the script, TesterPresent, Stop; all outcomes.
fn session<L: Line>(d: &mut Driver<L>, cfg: &LinkConfig) -> Vec<SessionOutcome> {
    let init = d.init(cfg).unwrap();
    let mut s = d.session(cfg, &init);
    let mut out: Vec<SessionOutcome> = SCRIPT
        .iter()
        .map(|p| d.request(&mut s, p).unwrap())
        .collect();
    out.push(d.tester_present(&mut s).unwrap());
    out.push(d.stop(&mut s).unwrap());
    out
}

fn replay_line(entries: Vec<LogEntry>) -> (VirtualLine, clio_sim::journal::Journal) {
    let ecu = ReplayEcu::new(entries);
    let j = ecu.journal();
    (
        VirtualLine::new(ecu, BusConfig::default(), Tolerance::lenient(), j.clone()),
        j,
    )
}

fn captured() -> (Vec<LogEntry>, Vec<SessionOutcome>) {
    let cfg = cfg(&sirius());
    let mut d = Driver::capturing(VirtualLine::with_ecu(sirius_ecu()));
    let out = session(&mut d, &cfg);
    (d.captured().to_vec(), out)
}

#[test]
fn capture_jsonl_replay_gives_the_same_session() {
    let (log, original) = captured();
    assert_eq!(log[0].kind, Some(EntryKind::FiveBaud));
    assert_eq!(log[0].hex, [0x7A]);
    assert_eq!(log[1].dir, Dir::Rx);
    assert_eq!(log[1].hex, [0x55, 0xD0, 0x8F]);

    let mut text = Vec::new();
    write_jsonl(&mut text, &log).unwrap();
    let back = read_jsonl(text.as_slice()).unwrap();
    assert_eq!(back, log);

    let (l, j) = replay_line(back);
    let mut d = Driver::new(l);
    let replayed = session(&mut d, &cfg(&sirius()));
    assert_eq!(replayed, original);
    assert!(j.violations().is_empty(), "{:?}", j.violations());
    // The replayed ident decodes to the scenario, too.
    let ident = positive(&replayed[1]);
    assert_eq!(
        decode(&sirius(), "Identification", &ident),
        expected(&sirius(), "Identification", &sirius_scenario())
    );
}

#[test]
fn replay_mismatch_goes_silent() {
    let (log, _) = captured();
    let (l, j) = replay_line(log);
    let mut d = Driver::new(l);
    let cfg = cfg(&sirius());
    let init = d.init(&cfg).unwrap();
    let mut s = d.session(&cfg, &init);
    // The log expects 10 A0 first.
    let e = d.request(&mut s, &[0x21, 0xAA]).unwrap_err();
    assert!(
        matches!(e.session(), Some(SessionError::Link(LinkError::NoReply))),
        "{e:?}"
    );
    let v = j.violations();
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].kind, ViolationKind::ReplayMismatch);
}

#[test]
fn hand_written_log_with_echo_in_rx() {
    // A log as a tester on the real cable would record it: rx includes the echo.
    let text = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bmt_21a3.jsonl"),
    )
    .unwrap();
    let log = read_jsonl(text.as_slice()).unwrap();
    let (l, j) = replay_line(log);
    let mut d = Driver::new(l);
    let cfg = cfg(&bmt());
    let init = d.init(&cfg).unwrap();
    assert_eq!(init.address_complement, Some(0xD9));
    let mut s = d.session(&cfg, &init);
    let r = positive(&d.request(&mut s, &[0x21, 0xA3]).unwrap());
    assert_eq!(r, [0x61, 0xA3, 0x05, 0x02]);
    assert!(j.violations().is_empty(), "{:?}", j.violations());
}

#[test]
fn bad_jsonl_reports_the_line() {
    let text =
        "{\"t_ms\":0,\"dir\":\"tx\",\"hex\":\"21\"}\n\n{\"t_ms\":1,\"dir\":\"rx\",\"hex\":\"6\"}\n";
    let e = read_jsonl(text.as_bytes()).unwrap_err();
    assert!(e.to_string().starts_with("line 3:"), "{e}");
}
