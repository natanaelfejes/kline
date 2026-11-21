//! Full Action/Event traces of the init machines, including error paths.

mod common;

use clio_core::init::{FastInit, FiveBaudInit, InitOutcome};
use clio_core::io::Action;
use clio_core::{Event, InitKind, InitVariant, KTiming, LinkConfig, LinkError, Step};
use common::*;

fn sirius32() -> LinkConfig {
    // SIRIUS32 XML <K>: KWP Multipoint 5-baud, KW1 0xD0 KW2 0x8F, Timeout 300, FirstRequest 20,
    // KW2_ 25, at address 0x7A.
    let mut cfg = LinkConfig::new(
        0x7A,
        KTiming {
            init: InitKind::KwpFiveBaud { multipoint: true },
            ..KTiming::default()
        },
    );
    cfg.expected_keywords = Some((0xD0, 0x8F));
    cfg
}

fn bmt() -> LinkConfig {
    let mut cfg = sirius32();
    cfg.ecu_address = 0x26;
    cfg
}

fn safir2() -> LinkConfig {
    let mut cfg = LinkConfig::new(
        0x7A,
        KTiming {
            init: InitKind::Iso8FiveBaud,
            kw2_timeout_ms: 500,
            ..KTiming::default()
        },
    );
    cfg.expected_keywords = Some((0xC2, 0x85));
    cfg
}

/// Idle, start bit and loopback check.
fn start_bit(seen: bool) -> Script {
    vec![
        (sleep(300), done()),
        (dtr(false), done()),
        (brk(true), done()),
        (sleep(200), done()),
        (Action::CheckBreakLoopback, Event::BreakLoopbackSeen(seen)),
    ]
}

/// Start bit, address bits, stop bit, flush and the keyword read.
fn up_to_keywords(addr: u8, window: u32, reply: Event) -> Script {
    let mut s = start_bit(true);
    s.extend(address_bits(addr));
    s.extend([
        (brk(false), done()),
        (sleep(100), done()),
        (dtr(true), done()),
        (flush(), done()),
        (read(3, window), reply),
    ]);
    s
}

fn run_five_baud(cfg: &LinkConfig, script: &Script) -> Step<InitOutcome, LinkError> {
    let mut m = FiveBaudInit::new(cfg).unwrap();
    let first = m.start();
    drive(first, script, |ev| m.on_event(ev))
}

fn multi(mut cfg: LinkConfig) -> LinkConfig {
    cfg.variant = InitVariant::MultiEcu;
    cfg
}

/// KWP 5-baud trace: address bits, `55 D0 8F`, 25 ms, `70`, `[70, ~addr]`, FirstRequest 20.
fn kwp_five_baud(addr: u8, complement: u8) -> Script {
    let mut s = up_to_keywords(addr, 400, ok(&[0x55, 0xD0, 0x8F]));
    s.extend([
        (sleep(25), done()),
        (write(&[0x70], 0), done()),
        (read(2, 400), ok(&[0x70, complement])),
        (sleep(20), done()),
    ]);
    s
}

fn kwp_outcome(multipoint: bool, addr: u8, complement: u8) -> InitOutcome {
    InitOutcome {
        kind: InitKind::KwpFiveBaud { multipoint },
        address_byte: Some(addr),
        sync: Some(0x55),
        kw1: 0xD0,
        kw2: 0x8F,
        address_complement: Some(complement),
        keywords_match: Some(true),
    }
}

#[test]
fn sirius32_single_ecu_init_is_default() {
    // DDT2000 with one ECU: Multipoint cleared, plain address 0x7A.
    let cfg = sirius32();
    assert_eq!(cfg.variant, InitVariant::SingleEcu);
    assert_eq!(cfg.five_baud_address_byte(), 0x7A);
    assert_eq!(cfg.effective_timing().inter_byte_ms, 0);
    assert_eq!(
        run_five_baud(&cfg, &kwp_five_baud(0x7A, 0x85)),
        Step::Done(kwp_outcome(false, 0x7A, 0x85))
    );
}

#[test]
fn sirius32_multi_ecu_init() {
    // DDT2000 with several ECUs: Multipoint, 0x7A ^ 0x80 = 0xFA, ~address 0x05.
    let cfg = multi(sirius32());
    assert_eq!(cfg.five_baud_address_byte(), 0xFA);
    assert_eq!(cfg.effective_timing().inter_byte_ms, 7);
    assert_eq!(
        run_five_baud(&cfg, &kwp_five_baud(0xFA, 0x05)),
        Step::Done(kwp_outcome(true, 0xFA, 0x05))
    );
}

#[test]
fn complement_is_logged_not_validated() {
    // Any ~address value passes.
    let Step::Done(o) = run_five_baud(&sirius32(), &kwp_five_baud(0x7A, 0x00)) else {
        panic!("init failed")
    };
    assert_eq!(o.address_complement, Some(0x00));
}

#[test]
fn bmt_single_ecu_init() {
    let cfg = bmt();
    assert_eq!(
        run_five_baud(&cfg, &kwp_five_baud(0x26, 0xD9)),
        Step::Done(kwp_outcome(false, 0x26, 0xD9))
    );
}

#[test]
fn bmt_multi_ecu_init() {
    let cfg = multi(bmt());
    assert_eq!(
        run_five_baud(&cfg, &kwp_five_baud(0xA6, 0x59)),
        Step::Done(kwp_outcome(true, 0xA6, 0x59))
    );
}

#[test]
fn multi_ecu_forces_multipoint_timing() {
    // Declared values differ from DDT2000's fixed Multipoint values; MultiEcu uses the fixed ones.
    let mut cfg = sirius32();
    cfg.timing.kw2_delay_ms = 40;
    cfg.timing.first_request_ms = 90;
    cfg.timing.kw2_timeout_ms = 900;
    let mut s = up_to_keywords(0x7A, 1000, ok(&[0x55, 0xD0, 0x8F]));
    s.extend([
        (sleep(40), done()),
        (write(&[0x70], 0), done()),
        (read(2, 1000), ok(&[0x70, 0x85])),
        (sleep(90), done()),
    ]);
    assert!(matches!(run_five_baud(&cfg, &s), Step::Done(_)));
    assert_eq!(
        run_five_baud(&multi(cfg), &kwp_five_baud(0xFA, 0x05)),
        Step::Done(kwp_outcome(true, 0xFA, 0x05))
    );
}

#[test]
fn multi_ecu_without_xml_multipoint_is_single() {
    let mut cfg = multi(sirius32());
    cfg.timing.init = InitKind::KwpFiveBaud { multipoint: false };
    assert_eq!(cfg.five_baud_address_byte(), 0x7A);
    assert_eq!(cfg.effective_timing().inter_byte_ms, 0);
}

#[test]
fn safir_iso8_init_has_no_handshake() {
    let cfg = safir2();
    let mut s = up_to_keywords(0x7A, 600, ok(&[0x55, 0xC2, 0x85]));
    s.push((sleep(20), done()));
    assert_eq!(
        run_five_baud(&cfg, &s),
        Step::Done(InitOutcome {
            kind: InitKind::Iso8FiveBaud,
            address_byte: Some(0x7A),
            sync: Some(0x55),
            kw1: 0xC2,
            kw2: 0x85,
            address_complement: None,
            keywords_match: Some(true),
        })
    );
}

#[test]
fn safir1_keywords_are_reported_not_rejected() {
    // Safir_2 definition expected, Safir_1 key bytes received: logged as a mismatch only.
    let cfg = safir2();
    let mut s = up_to_keywords(0x7A, 600, ok(&[0x55, 0x1C, 0x86]));
    s.push((sleep(20), done()));
    let Step::Done(o) = run_five_baud(&cfg, &s) else {
        panic!("init failed")
    };
    assert_eq!(o.keywords(), (0x1C, 0x86));
    assert_eq!(o.keywords_match, Some(false));
}

#[test]
fn keyword_mismatch_rejected_when_configured() {
    let mut cfg = safir2();
    cfg.reject_keyword_mismatch = true;
    let s = up_to_keywords(0x7A, 600, ok(&[0x55, 0x1C, 0x86]));
    assert_eq!(
        run_five_baud(&cfg, &s),
        Step::Failed(LinkError::KeywordMismatch {
            expected: (0xC2, 0x85),
            received: (0x1C, 0x86)
        })
    );
}

#[test]
fn k_line_inactive_releases_the_line() {
    let mut s = start_bit(false);
    s.extend([(brk(false), done()), (dtr(true), done())]);
    assert_eq!(
        run_five_baud(&sirius32(), &s),
        Step::Failed(LinkError::KLineInactive)
    );
}

#[test]
fn no_keywords() {
    let s = up_to_keywords(0x7A, 400, timeout(&[0x55]));
    assert_eq!(
        run_five_baud(&sirius32(), &s),
        Step::Failed(LinkError::NoKeywords {
            received: vec![0x55]
        })
    );
}

#[test]
fn kw2_complement_echo_collision() {
    let mut s = up_to_keywords(0x7A, 400, ok(&[0x55, 0xD0, 0x8F]));
    s.extend([
        (sleep(25), done()),
        (write(&[0x70], 0), done()),
        (read(2, 400), ok(&[0x71, 0x05])),
    ]);
    assert_eq!(
        run_five_baud(&sirius32(), &s),
        Step::Failed(LinkError::Collision {
            sent: vec![0x70],
            echo: vec![0x71, 0x05]
        })
    );
}

#[test]
fn no_address_complement() {
    let mut s = up_to_keywords(0x7A, 400, ok(&[0x55, 0xD0, 0x8F]));
    s.extend([
        (sleep(25), done()),
        (write(&[0x70], 0), done()),
        (read(2, 400), timeout(&[0x70])),
    ]);
    assert_eq!(
        run_five_baud(&sirius32(), &s),
        Step::Failed(LinkError::NoAddressComplement {
            received: vec![0x70]
        })
    );
}

#[test]
fn wrong_event_fails() {
    let mut m = FiveBaudInit::new(&sirius32()).unwrap();
    assert_eq!(m.start(), sleep(300));
    assert_eq!(
        m.on_event(ok(&[0x55])),
        Step::Failed(LinkError::UnexpectedEvent)
    );
    // Finished machines refuse further events.
    assert_eq!(m.on_event(done()), Step::Failed(LinkError::UnexpectedEvent));
}

#[test]
fn short_read_ok_is_rejected() {
    // ReadOk must carry exactly `count` bytes.
    let s = up_to_keywords(0x7A, 400, ok(&[0x55, 0xD0]));
    assert_eq!(
        run_five_baud(&sirius32(), &s),
        Step::Failed(LinkError::UnexpectedEvent)
    );
}

#[test]
fn init_kind_checked() {
    let fast = LinkConfig::new(
        0x7A,
        KTiming {
            init: InitKind::KwpFast { multipoint: false },
            ..KTiming::default()
        },
    );
    assert!(matches!(
        FiveBaudInit::new(&fast),
        Err(LinkError::WrongInitKind)
    ));
    assert!(matches!(
        FastInit::new(&sirius32()),
        Err(LinkError::WrongInitKind)
    ));
}

fn fast_cfg(addr: u8) -> LinkConfig {
    LinkConfig::new(
        addr,
        KTiming {
            init: InitKind::KwpFast { multipoint: false },
            ..KTiming::default()
        },
    )
}

fn wake_up() -> Script {
    vec![
        (sleep(300), done()),
        (dtr(true), done()),
        (sleep(1), done()),
        (brk(true), done()),
        (sleep(25), done()),
        (brk(false), done()),
        (sleep(25), done()),
    ]
}

const START_COMM_7A: [u8; 5] = [0x81, 0x7A, 0xF1, 0x81, 0x6D];

fn run_fast(cfg: &LinkConfig, script: &Script) -> Step<InitOutcome, LinkError> {
    let mut m = FastInit::new(cfg).unwrap();
    let first = m.start();
    drive(first, script, |ev| m.on_event(ev))
}

#[test]
fn fast_init_sirius32_fallback() {
    // x65 addressing lists FastInit MonoPoint for 0x7A as the second protocol.
    let mut s = wake_up();
    let reply = [0x83, 0xF1, 0x7A, 0xC1, 0xD0, 0x8F];
    let cs = clio_core::frame::checksum(&reply);
    s.extend([
        (flush(), done()),
        (write(&START_COMM_7A, 0), done()),
        (read(5, 300), ok(&START_COMM_7A)),
        (read(1, 300), ok(&reply[..1])),
        (read(6, 300), ok(&[0xF1, 0x7A, 0xC1, 0xD0, 0x8F, cs])),
    ]);
    assert_eq!(
        run_fast(&fast_cfg(0x7A), &s),
        Step::Done(InitOutcome {
            kind: InitKind::KwpFast { multipoint: false },
            address_byte: None,
            sync: None,
            kw1: 0xD0,
            kw2: 0x8F,
            address_complement: None,
            keywords_match: None,
        })
    );
}

#[test]
fn fast_init_separate_length_byte_reply() {
    let mut s = wake_up();
    let body = [0xF1, 0x7A, 0x03, 0xC1, 0xD0, 0x8F];
    let mut full = vec![0x80];
    full.extend_from_slice(&body);
    let cs = clio_core::frame::checksum(&full);
    s.extend([
        (flush(), done()),
        (write(&START_COMM_7A, 0), done()),
        (read(5, 300), ok(&START_COMM_7A)),
        (read(1, 300), ok(&[0x80])),
        (read(3, 300), ok(&body[..3])),
        (read(4, 300), ok(&[0xC1, 0xD0, 0x8F, cs])),
    ]);
    let Step::Done(o) = run_fast(&fast_cfg(0x7A), &s) else {
        panic!("init failed")
    };
    assert_eq!(o.keywords(), (0xD0, 0x8F));
}

#[test]
fn fast_init_no_reply() {
    let mut s = wake_up();
    s.extend([
        (flush(), done()),
        (write(&START_COMM_7A, 0), done()),
        (read(5, 300), ok(&START_COMM_7A)),
        (read(1, 300), timeout(&[])),
    ]);
    assert_eq!(
        run_fast(&fast_cfg(0x7A), &s),
        Step::Failed(LinkError::NoReply)
    );
}

#[test]
fn fast_init_no_echo_is_inactive_line() {
    let mut s = wake_up();
    s.extend([
        (flush(), done()),
        (write(&START_COMM_7A, 0), done()),
        (read(5, 300), timeout(&[])),
    ]);
    assert_eq!(
        run_fast(&fast_cfg(0x7A), &s),
        Step::Failed(LinkError::KLineInactive)
    );
}

#[test]
fn fast_init_negative_reply() {
    let mut s = wake_up();
    let mut reply = vec![0x83, 0xF1, 0x7A, 0x7F, 0x81, 0x10];
    reply.push(clio_core::frame::checksum(&reply));
    s.extend([
        (flush(), done()),
        (write(&START_COMM_7A, 0), done()),
        (read(5, 300), ok(&START_COMM_7A)),
        (read(1, 300), ok(&reply[..1])),
        (read(6, 300), ok(&reply[1..])),
    ]);
    assert_eq!(
        run_fast(&fast_cfg(0x7A), &s),
        Step::Failed(LinkError::UnexpectedStartReply(
            clio_core::frame::Reply::Negative {
                sid: 0x81,
                nrc: 0x10
            }
        ))
    );
}

#[test]
fn fast_init_fallback_for_five_baud_ecu() {
    // Sirius 32 definition (KWP 5-baud), opened with a fast init instead.
    let mut cfg = sirius32();
    cfg.variant = InitVariant::FastInit;
    assert!(FiveBaudInit::new(&cfg).is_err());
    let mut s = wake_up();
    let reply = [0x83, 0xF1, 0x7A, 0xC1, 0xD0, 0x8F];
    let cs = clio_core::frame::checksum(&reply);
    s.extend([
        (flush(), done()),
        (write(&START_COMM_7A, 0), done()),
        (read(5, 300), ok(&START_COMM_7A)),
        (read(1, 300), ok(&reply[..1])),
        (read(6, 300), ok(&[0xF1, 0x7A, 0xC1, 0xD0, 0x8F, cs])),
    ]);
    let Step::Done(o) = run_fast(&cfg, &s) else {
        panic!("init failed")
    };
    assert_eq!(o.kind, InitKind::KwpFast { multipoint: false });
    assert_eq!(o.keywords_match, Some(true));
}

#[test]
fn fast_init_positive_but_not_c1_fails() {
    let mut s = wake_up();
    let mut reply = vec![0x83, 0xF1, 0x7A, 0xC2, 0xD0, 0x8F];
    reply.push(clio_core::frame::checksum(&reply));
    s.extend([
        (flush(), done()),
        (write(&START_COMM_7A, 0), done()),
        (read(5, 300), ok(&START_COMM_7A)),
        (read(1, 300), ok(&reply[..1])),
        (read(6, 300), ok(&reply[1..])),
    ]);
    assert_eq!(
        run_fast(&fast_cfg(0x7A), &s),
        Step::Failed(LinkError::UnexpectedStartReply(
            clio_core::frame::Reply::Positive(vec![0xC2, 0xD0, 0x8F])
        ))
    );
}

#[test]
fn fast_init_multipoint_uses_7ms_inter_byte_only_multi_ecu() {
    let mut cfg = LinkConfig::new(
        0x7A,
        KTiming {
            init: InitKind::KwpFast { multipoint: true },
            ..KTiming::default()
        },
    );
    let script = |ib| {
        let mut s = wake_up();
        s.extend([
            (flush(), done()),
            (write(&START_COMM_7A, ib), done()),
            (read(5, 300), timeout(&[])),
        ]);
        s
    };
    assert_eq!(
        run_fast(&cfg, &script(0)),
        Step::Failed(LinkError::KLineInactive)
    );
    cfg.variant = InitVariant::MultiEcu;
    assert_eq!(
        run_fast(&cfg, &script(7)),
        Step::Failed(LinkError::KLineInactive)
    );
}
