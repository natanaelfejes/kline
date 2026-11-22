//! Human approvals (feature `approved-routines`): binding to session, target, exact bytes, tier,
//! risk; single use; Tier 2 preconditions; expert mode. Security review M2 finding 1.
#![cfg(feature = "approved-routines")]

mod common;

use common::*;
use kline_core::frame::{checksum, Reply};
use kline_core::safety::{assess, Approval, ConfirmationMismatch, DenyReason, Tier, Warning};
use kline_core::session::{Session, SessionError, SessionOutcome};
use kline_core::{InitKind, KTiming, LinkConfig, Step};

const ERASE_AND_LEARN_1: [u8; 5] = [0x31, 0xC7, 0x00, 0x00, 0x00];
const LEARN_2: [u8; 5] = [0x31, 0xC8, 0x00, 0x00, 0x00];

fn cfg(addr: u8) -> LinkConfig {
    LinkConfig::new(
        addr,
        KTiming {
            init: InitKind::KwpFiveBaud { multipoint: false },
            ..KTiming::default()
        },
    )
}

fn bmt() -> Session {
    Session::new(open(&cfg(0x26), 0xD0, 0x8F), 0)
}

fn sirius() -> Session {
    Session::new(open(&cfg(0x7A), 0xD0, 0x8F), 0)
}

fn kwp(src: u8, dst: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = vec![0x80 | payload.len() as u8, dst, src];
    f.extend_from_slice(payload);
    f.push(checksum(&f));
    f
}

/// Run one exchange to `addr` with `reply` and return the outcome.
fn exchange(
    s: &mut Session,
    first: kline_core::Action,
    addr: u8,
    req: &[u8],
    reply: &[u8],
    now: u64,
) -> Step<SessionOutcome, SessionError> {
    let tx = kwp(0xF1, addr, req);
    let rx = kwp(addr, 0xF1, reply);
    let script = vec![
        (flush(), done()),
        (write(&tx, 0), done()),
        (read(tx.len(), 300), ok(&tx)),
        (read(1, 300), ok(&rx[..1])),
        (read(rx.len() - 1, 300), ok(&rx[1..])),
    ];
    drive(first, &script, |ev| s.on_event(ev, now))
}

fn approve(s: &mut Session, payload: &[u8]) -> Approval {
    let ch = s.service_challenge(payload).unwrap();
    let code = ch.code().to_string();
    Approval::confirm(ch, &code).unwrap()
}

#[test]
fn approved_remote_learning_full_trace() {
    let mut s = bmt();
    let ch = s.service_challenge(&ERASE_AND_LEARN_1).unwrap();
    assert_eq!(ch.payload(), ERASE_AND_LEARN_1);
    assert_eq!(ch.tier(), Tier::Service);
    assert_eq!(ch.target().ecu_address, 0x26);
    assert_eq!(ch.risk().warnings, &[Warning::ErasesRemotes]);
    assert_eq!(
        *ch.risk(),
        assess(&ch.target(), &ERASE_AND_LEARN_1).unwrap()
    );
    assert_eq!(ch.code().len(), 6);
    let code = ch.code().to_string();
    let a = Approval::confirm(ch, &format!("  {code}\n")).unwrap();
    let first = s.request_approved(&ERASE_AND_LEARN_1, a, 0).unwrap();
    assert_eq!(
        exchange(&mut s, first, 0x26, &ERASE_AND_LEARN_1, &[0x71, 0xC7], 0),
        Step::Done(SessionOutcome::Reply(Reply::Positive(vec![0x71, 0xC7])))
    );
}

#[test]
fn codes_are_random_per_challenge() {
    let mut s = bmt();
    let codes: Vec<String> = (0..8)
        .map(|_| s.service_challenge(&LEARN_2).unwrap().code().to_string())
        .collect();
    for (i, a) in codes.iter().enumerate() {
        for b in &codes[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

#[test]
fn wrong_code_is_refused() {
    let mut s = bmt();
    for typed in [
        "",
        "yes",
        "y",
        "LEARN REMOTE 2",
        "ERASE ALL REMOTES AND LEARN REMOTE 1",
    ] {
        let ch = s.service_challenge(&LEARN_2).unwrap();
        assert_eq!(
            Approval::confirm(ch, typed).unwrap_err(),
            ConfirmationMismatch
        );
    }
    // One character off, and a lowercase letter, are both refused.
    let ch = s.service_challenge(&LEARN_2).unwrap();
    let mut wrong: Vec<char> = ch.code().chars().collect();
    wrong[0] = if wrong[0] == 'A' { 'C' } else { 'A' };
    let wrong: String = wrong.into_iter().collect();
    assert_eq!(
        Approval::confirm(ch, &wrong).unwrap_err(),
        ConfirmationMismatch
    );
    let ch = s.service_challenge(&LEARN_2).unwrap();
    let lower: String = ch
        .code()
        .chars()
        .map(|c| {
            if c.is_ascii_digit() {
                c
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect::<String>()
        + "";
    if lower != ch.code() {
        assert_eq!(
            Approval::confirm(ch, &lower).unwrap_err(),
            ConfirmationMismatch
        );
    }
}

#[test]
fn approval_is_bound_to_its_session() {
    // Security review M2 F1: an approval minted for one session cannot drive another one, even
    // to the same ECU.
    let mut s1 = bmt();
    let mut s2 = bmt();
    let a = approve(&mut s1, &ERASE_AND_LEARN_1);
    let _ = s2.service_challenge(&ERASE_AND_LEARN_1).unwrap();
    assert!(matches!(
        s2.request_approved(&ERASE_AND_LEARN_1, a, 0),
        Err(SessionError::Denied(ref d)) if d.reason == DenyReason::ApprovalMismatch
    ));
    assert!(!s2.is_busy());
}

#[test]
fn approval_is_bound_to_the_exact_bytes() {
    let mut s = bmt();
    let a = approve(&mut s, &ERASE_AND_LEARN_1);
    assert!(matches!(
        s.request_approved(&LEARN_2, a, 0),
        Err(SessionError::Denied(ref d)) if d.reason == DenyReason::ApprovalMismatch
    ));
    assert!(!s.is_busy());
    // Same tier and identical RiskInfo, different output byte: only the byte binding stops it.
    let (window, lock) = ([0x31, 0xA0, 0x00, 0x01], [0x31, 0xA1, 0x00, 0x01]);
    let bmt = kline_core::safety::Target {
        protocol: kline_core::safety::Protocol::Kwp,
        ecu_address: 0x26,
    };
    assert_eq!(assess(&bmt, &window).unwrap(), assess(&bmt, &lock).unwrap());
    let a = approve(&mut s, &window);
    assert!(matches!(
        s.request_approved(&lock, a, 0),
        Err(SessionError::Denied(ref d)) if d.reason == DenyReason::ApprovalMismatch
    ));
    assert!(!s.is_busy());
}

#[test]
fn approval_is_single_use_and_latest_challenge_only() {
    let mut s = bmt();
    // A newer challenge invalidates the older one.
    let old = s.service_challenge(&LEARN_2).unwrap();
    let old_code = old.code().to_string();
    let _newer = s.service_challenge(&LEARN_2).unwrap();
    let a = Approval::confirm(old, &old_code).unwrap();
    assert!(matches!(
        s.request_approved(&LEARN_2, a, 0),
        Err(SessionError::Denied(ref d)) if d.reason == DenyReason::ApprovalMismatch
    ));
    // A failed attempt (busy) also closes the challenge.
    let a1 = approve(&mut s, &LEARN_2);
    let _ = s.request(&[0x21, 0xA0], 0).unwrap();
    assert_eq!(s.request_approved(&LEARN_2, a1, 0), Err(SessionError::Busy));
}

#[test]
fn approval_kind_must_match_tier() {
    let mut s = sirius();
    let mismatch = |r: Result<_, SessionError>| matches!(r, Err(SessionError::Denied(ref d)) if d.reason == DenyReason::ApprovalMismatch);
    // Reads need no approval; config writes and undocumented requests use their own path.
    assert!(mismatch(s.service_challenge(&[0x21, 0xA0])));
    assert!(mismatch(s.service_challenge(&[0x3B, 0xD0, 0x10])));
    assert!(mismatch(s.service_challenge(&[0x31, 0xA0, 0xA1])));
    assert!(mismatch(s.expert_challenge(&[0x14, 0x00, 0x00])));
    assert!(mismatch(s.expert_challenge(&[0x21, 0xA0])));
    // BMT remote learning is not Tier 1 on the Sirius.
    assert!(mismatch(s.service_challenge(&ERASE_AND_LEARN_1)));
    // Engine stop carries its warning.
    let ch = s.service_challenge(&[0x31, 0xA0, 0xA0]).unwrap();
    assert_eq!(ch.risk().warnings, &[Warning::StopsEngine]);
}

#[test]
fn forbidden_requests_get_no_challenge() {
    let mut s = bmt();
    for p in [
        &[0x31, 0xC0, 0x00, 0x00, 0x00][..],
        &[0x31, 0xC5, 0x00, 0x00, 0x00],
        &[0x3B, 0xD2, 0x02, 0x01],
        &[0x2E, 0x01, 0x00],
        &[0x27, 0x01],
        &[0x34, 0x00],
        &[0x10, 0x85],
        &[0x82],
    ] {
        for r in [s.service_challenge(p), s.expert_challenge(p)] {
            match r {
                Err(SessionError::Denied(d)) => assert!(d.reason.is_forbidden(), "{p:02X?}"),
                other => panic!("{p:02X?}: {other:?}"),
            }
        }
    }
    let mut s = sirius();
    for p in [&[0x31, 0xA0, 0xA3][..], &[0x3B, 0xAE, 0x01]] {
        assert!(matches!(
            s.expert_challenge(p),
            Err(SessionError::Denied(ref d)) if d.reason == DenyReason::Immobilizer
        ));
    }
}

#[test]
fn expert_request_shows_unknown_effect() {
    let mut s = sirius();
    let p = [0x31, 0xA0, 0xA7, 0x00];
    let ch = s.expert_challenge(&p).unwrap();
    assert_eq!(ch.tier(), Tier::Expert);
    assert_eq!(ch.risk().warnings, &[Warning::UnknownEffect]);
    assert!(!ch.risk().reversible);
    assert_eq!(ch.code().len(), 10);
    let code = ch.code().to_string();
    let a = Approval::confirm(ch, &code).unwrap();
    let first = s.request_approved(&p, a, 0).unwrap();
    assert!(matches!(
        exchange(&mut s, first, 0x7A, &p, &[0x71, 0xA0], 0),
        Step::Done(SessionOutcome::Reply(_))
    ));
}

/// Read `21 lid` on `s` (to `addr`) and answer positively with `data`.
fn read_ok(s: &mut Session, addr: u8, lid: u8, data: &[u8], now: u64) {
    let first = s.request(&[0x21, lid], now).unwrap();
    let mut reply = vec![0x61, lid];
    reply.extend_from_slice(data);
    assert!(matches!(
        exchange(s, first, addr, &[0x21, lid], &reply, now),
        Step::Done(_)
    ));
}

#[test]
fn config_write_needs_identification_and_backup() {
    let mut s = bmt();
    let write = [0x3B, 0xD2, 0x07, 0x01];
    let ident = [0x61, 0x80, 0x01, 0x02, 0x03];
    let r = |s: &mut Session, id: &[u8]| match s.config_write_challenge(&write, id) {
        Err(SessionError::Denied(d)) => Some(d.reason),
        Ok(_) => None,
        Err(e) => panic!("{e:?}"),
    };
    // No 21 80 in this session.
    assert_eq!(r(&mut s, &ident), Some(DenyReason::IdentificationMismatch));
    read_ok(&mut s, 0x26, 0x80, &ident[2..], 0);
    // Definition chosen for another identification reply.
    assert_eq!(
        r(&mut s, &[0x61, 0x80, 0x09]),
        Some(DenyReason::IdentificationMismatch)
    );
    // No 21 A1 backup yet.
    assert_eq!(
        r(&mut s, &ident),
        Some(DenyReason::PreReadRequired { identifier: 0xA1 })
    );
    read_ok(&mut s, 0x26, 0xA1, &[0x12, 0x34], 1000);
    assert_eq!(s.config_read(0xA1), Some(&[0x61, 0xA1, 0x12, 0x34][..]));
    // Not through the service path.
    assert!(matches!(
        s.service_challenge(&write),
        Err(SessionError::Denied(ref d)) if d.reason == DenyReason::ApprovalMismatch
    ));
    let ch = s.config_write_challenge(&write, &ident).unwrap();
    assert_eq!(ch.tier(), Tier::ConfigWrite);
    assert_eq!(ch.risk().warnings, &[Warning::EepromWrite]);
    let code = ch.code().to_string();
    let a = Approval::confirm(ch, &code).unwrap();
    let first = s.request_approved(&write, a, 2000).unwrap();
    assert!(matches!(
        exchange(&mut s, first, 0x26, &write, &[0x7B, 0xD2], 2000),
        Step::Done(_)
    ));
}

#[test]
fn config_write_refused_when_identification_changes_after_challenge() {
    let mut s = bmt();
    let write = [0x3B, 0xD2, 0x07, 0x01];
    let ident = [0x61, 0x80, 0x01];
    read_ok(&mut s, 0x26, 0x80, &ident[2..], 0);
    read_ok(&mut s, 0x26, 0xA1, &[0x12, 0x34], 100);
    let ch = s.config_write_challenge(&write, &ident).unwrap();
    let code = ch.code().to_string();
    let a = Approval::confirm(ch, &code).unwrap();
    // Another 21 80 reply arrives before the write (another ECU / definition).
    read_ok(&mut s, 0x26, 0x80, &[0x02], 200);
    assert!(matches!(
        s.request_approved(&write, a, 300),
        Err(SessionError::Denied(ref d)) if d.reason == DenyReason::IdentificationMismatch
    ));
}

#[test]
fn unframeable_request_is_a_frame_error() {
    // Encode errors (request) stay SessionError::Frame; reply decode errors are Link(Frame).
    let mut s = sirius();
    let mut p = vec![0x23];
    p.resize(300, 0x00);
    let ch = s.expert_challenge(&p).unwrap();
    let code = ch.code().to_string();
    let a = Approval::confirm(ch, &code).unwrap();
    assert!(matches!(
        s.request_approved(&p, a, 0),
        Err(SessionError::Frame(
            kline_core::frame::FrameError::PayloadTooLong { .. }
        ))
    ));
    assert!(!s.is_busy());
}

/// Re-review test gap: a failed or negative `21 A1` after the challenge removes the backup, so
/// the approved write is refused.
#[test]
fn config_write_refused_when_backup_is_invalidated_after_challenge() {
    let mut s = bmt();
    let write = [0x3B, 0xD2, 0x07, 0x01];
    let ident = [0x61, 0x80, 0x01];
    read_ok(&mut s, 0x26, 0x80, &ident[2..], 0);
    read_ok(&mut s, 0x26, 0xA1, &[0x12, 0x34], 100);
    let ch = s.config_write_challenge(&write, &ident).unwrap();
    let code = ch.code().to_string();
    let a = Approval::confirm(ch, &code).unwrap();
    let first = s.request(&[0x21, 0xA1], 200).unwrap();
    assert!(matches!(
        exchange(&mut s, first, 0x26, &[0x21, 0xA1], &[0x7F, 0x21, 0x22], 200),
        Step::Done(SessionOutcome::Reply(Reply::Negative { .. }))
    ));
    assert_eq!(s.config_read(0xA1), None);
    assert!(matches!(
        s.request_approved(&write, a, 300),
        Err(SessionError::Denied(ref d)) if d.reason == DenyReason::PreReadRequired { identifier: 0xA1 }
    ));
}
