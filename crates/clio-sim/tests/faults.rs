//! Each failure mode of the simulated ECU, as clio-core sees it.

mod common;

use clio_core::frame::{FrameError, Reply};
use clio_core::session::{Session, SessionError, SessionOutcome};
use clio_core::{LinkConfig, LinkError};
use clio_sim::driver::{DriveError, Driver};
use clio_sim::ecu::{FaultRule, InitFault, ReplyFault, SpecEcu};
use clio_sim::journal::Journal;
use clio_sim::line::{Line, VirtualLine};
use common::*;

const A9: [u8; 2] = [0x21, 0xA9];

fn session_with(ecu: SpecEcu) -> (Driver<VirtualLine>, Session, Journal) {
    let (mut d, j) = drive(ecu);
    let cfg = cfg(&sirius());
    let init = d.init(&cfg).unwrap();
    let s = d.session(&cfg, &init);
    (d, s, j)
}

fn link_err(e: &DriveError) -> &LinkError {
    match e.session() {
        Some(SessionError::Link(l)) => l,
        other => panic!("expected a link error, got {other:?} ({e:?})"),
    }
}

#[test]
fn no_answer_to_init() {
    let (mut d, _) = drive(sirius_ecu().with_init_fault(InitFault::Silent));
    let e = d.init(&cfg(&sirius())).unwrap_err();
    assert_eq!(e.init(), Some(&LinkError::NoKeywords { received: vec![] }));
}

#[test]
fn wrong_keywords_reported_or_rejected() {
    let fault = InitFault::WrongKeywords(0xE9, 0x8F);
    let (mut d, _) = drive(sirius_ecu().with_init_fault(fault));
    let init = d.init(&cfg(&sirius())).unwrap();
    assert_eq!(init.keywords(), (0xE9, 0x8F));
    assert_eq!(init.keywords_match, Some(false));

    let (mut d, _) = drive(sirius_ecu().with_init_fault(fault));
    let mut c: LinkConfig = cfg(&sirius());
    c.reject_keyword_mismatch = true;
    assert_eq!(
        d.init(&c).unwrap_err().init(),
        Some(&LinkError::KeywordMismatch {
            expected: (0xD0, 0x8F),
            received: (0xE9, 0x8F)
        })
    );
}

#[test]
fn missing_address_complement() {
    let (mut d, _) = drive(sirius_ecu().with_init_fault(InitFault::NoAddressComplement));
    assert_eq!(
        d.init(&cfg(&sirius())).unwrap_err().init(),
        Some(&LinkError::NoAddressComplement {
            received: vec![0x70]
        })
    );
}

#[test]
fn checksum_error() {
    let ecu = sirius_ecu().with_fault(FaultRule::on(&A9, ReplyFault::BadChecksum).once());
    let (mut d, mut s, _) = session_with(ecu);
    let e = d.request(&mut s, &A9).unwrap_err();
    // Reply decode errors surface as SessionError::Frame (see report: API request).
    assert!(
        matches!(
            e.session(),
            Some(SessionError::Frame(FrameError::Checksum { .. }))
        ),
        "{e:?}"
    );
    // once(): the next one is fine.
    positive(&d.request(&mut s, &A9).unwrap());
}

#[test]
fn response_pending_then_reply() {
    let ecu = sirius_ecu().with_fault(FaultRule::on(&A9, ReplyFault::ResponsePending { count: 3 }));
    let (mut d, mut s, j) = session_with(ecu);
    let r = positive(&d.request(&mut s, &A9).unwrap());
    assert_eq!(
        decode(&sirius(), "Live A9", &r),
        expected(&sirius(), "Live A9", &sirius_scenario())
    );
    j.assert_no_violations();
}

#[test]
fn response_pending_beyond_the_core_bound() {
    let ecu = sirius_ecu().with_fault(FaultRule::on(
        &A9,
        ReplyFault::ResponsePending { count: 11 },
    ));
    let (mut d, mut s, _) = session_with(ecu);
    let e = d.request(&mut s, &A9).unwrap_err();
    assert_eq!(link_err(&e), &LinkError::TooManyPending { count: 11 });
}

#[test]
fn negative_response() {
    let ecu = sirius_ecu().with_fault(FaultRule::on(&A9, ReplyFault::Negative { nrc: 0x22 }));
    let (mut d, mut s, _) = session_with(ecu);
    assert_eq!(
        d.request(&mut s, &A9).unwrap(),
        SessionOutcome::Reply(Reply::Negative {
            sid: 0x21,
            nrc: 0x22
        })
    );
}

#[test]
fn dropped_bytes() {
    let ecu = sirius_ecu().with_fault(FaultRule::on(&A9, ReplyFault::DropBytes { count: 2 }));
    let (mut d, mut s, _) = session_with(ecu);
    let e = d.request(&mut s, &A9).unwrap_err();
    match link_err(&e) {
        // Header 3 + 12 payload + checksum = 16 bytes, 2 dropped.
        LinkError::IncompleteReply { received } => assert_eq!(received.len(), 14),
        other => panic!("{other:?}"),
    }
}

#[test]
fn slow_reply_beyond_p2_then_recovery() {
    let ecu =
        sirius_ecu().with_fault(FaultRule::on(&A9, ReplyFault::Slow { delay_ms: 400 }).once());
    let (mut d, mut s, _) = session_with(ecu);
    let t0 = d.line.now_ms();
    let e = d.request(&mut s, &A9).unwrap_err();
    assert_eq!(link_err(&e), &LinkError::NoReply);
    // The core gave up after its 300 ms RequestToReply timeout.
    assert!(d.line.now_ms() - t0 < 400);
    // The late reply is flushed before the next request (OnError gap), which succeeds.
    let r = positive(&d.request(&mut s, &A9).unwrap());
    assert_eq!(r[..2], [0x61, 0xA9]);
}

#[test]
fn no_reply_to_tester_present_is_tolerated() {
    let ecu = sirius_ecu().with_fault(FaultRule::on(&[0x3E], ReplyFault::NoReply));
    let (mut d, mut s, _) = session_with(ecu);
    assert_eq!(
        d.tester_present(&mut s).unwrap(),
        SessionOutcome::TesterPresent(None)
    );
    positive(&d.request(&mut s, &A9).unwrap());
}
