//! Strict vs lenient timing checks of the simulated ECU, with a hand-driven tester.

mod common;

use common::*;
use kline_core::LinkError;
use kline_sim::ecu::{EcuProfile, SpecEcu};
use kline_sim::frame::checksum;
use kline_sim::journal::{Journal, ViolationKind};
use kline_sim::line::{Line, VirtualLine};
use kline_sim::timing::Tolerance;

fn line(tol: Tolerance) -> (VirtualLine, Journal) {
    let mut p = EcuProfile::sirius32();
    p.tolerance = tol;
    let ecu = SpecEcu::new(p, sirius(), sirius_scenario()).unwrap();
    let j = ecu.journal();
    (VirtualLine::with_ecu(ecu), j)
}

fn strict() -> Tolerance {
    Tolerance::strict(5)
}

/// The core's 5-baud sequence with a configurable bit time; returns the keyword read.
fn bitbang(l: &mut VirtualLine, byte: u8, idle_ms: u32, bit_ms: u32) -> Vec<u8> {
    l.sleep_ms(idle_ms);
    l.set_break(true).unwrap();
    l.sleep_ms(bit_ms);
    for i in 0..8 {
        l.set_break((byte >> i) & 1 == 0).unwrap();
        l.sleep_ms(bit_ms);
    }
    l.set_break(false).unwrap();
    l.sleep_ms(100);
    l.flush_rx().unwrap();
    l.read(3, 400).unwrap()
}

fn kinds(j: &Journal) -> Vec<ViolationKind> {
    j.violations().iter().map(|v| v.kind).collect()
}

#[test]
fn five_baud_bit_timing_strict_vs_lenient() {
    let (mut l, j) = line(strict());
    assert_eq!(bitbang(&mut l, 0x7A, 300, 200), [0x55, 0xD0, 0x8F]);
    assert!(j.violations().is_empty());
    // 5 % slow bits: the 2nd edge is already 20 ms off the 200 ms grid (strict: 10).
    let (mut l, j) = line(strict());
    assert_eq!(bitbang(&mut l, 0x7A, 300, 210), Vec::<u8>::new());
    assert_eq!(kinds(&j), [ViolationKind::FiveBaudTiming]);
    let (mut l, j) = line(Tolerance::lenient());
    // Lenient only samples the bit middles: 8 x 10 ms drift still lands inside each bit.
    assert_eq!(bitbang(&mut l, 0x7A, 300, 210), [0x55, 0xD0, 0x8F]);
    assert!(j.violations().is_empty());
}

#[test]
fn idle_before_wake_up() {
    let (mut l, j) = line(strict());
    assert_eq!(bitbang(&mut l, 0x7A, 50, 200), Vec::<u8>::new());
    assert_eq!(kinds(&j), [ViolationKind::IdleBeforeWake]);
    let (mut l, _) = line(Tolerance::lenient());
    assert_eq!(bitbang(&mut l, 0x7A, 50, 200), [0x55, 0xD0, 0x8F]);
}

#[test]
fn late_complement_of_kw2() {
    for (tol, ok) in [(strict(), false), (Tolerance::lenient(), true)] {
        let (mut l, j) = line(tol);
        assert_eq!(bitbang(&mut l, 0x7A, 300, 200), [0x55, 0xD0, 0x8F]);
        l.sleep_ms(60); // W4max is 50
        l.write(&[0x70], 0).unwrap();
        let rx = l.read(2, 400).unwrap();
        if ok {
            assert_eq!(rx, [0x70, 0x85]);
        } else {
            assert_eq!(rx, [0x70], "echo only");
            assert_eq!(kinds(&j), [ViolationKind::Handshake]);
        }
    }
}

fn frame(p: &[u8]) -> Vec<u8> {
    let mut f = vec![0x80 | p.len() as u8, 0x7A, 0xF1];
    f.extend_from_slice(p);
    f.push(checksum(&f));
    f
}

/// Init by hand, then return the line ready for frames.
fn ready(tol: Tolerance) -> (VirtualLine, Journal) {
    let (mut l, j) = line(tol);
    bitbang(&mut l, 0x7A, 300, 200);
    l.sleep_ms(25);
    l.write(&[0x70], 0).unwrap();
    assert_eq!(l.read(2, 400).unwrap(), [0x70, 0x85]);
    l.sleep_ms(20);
    (l, j)
}

#[test]
fn p3_min_between_reply_and_request() {
    for (tol, ok) in [(strict(), false), (Tolerance::lenient(), true)] {
        let (mut l, j) = ready(tol);
        let f = frame(&[0x21, 0xAA]);
        l.write(&f, 0).unwrap();
        // echo + reply of 3 header + 6 data + cs
        assert_eq!(l.read(f.len() + 10, 300).unwrap().len(), f.len() + 10);
        // Next request right after the reply's last byte: 0 ms < P3min 5.
        l.write(&f, 0).unwrap();
        let rx = l.read(f.len() + 10, 300).unwrap();
        if ok {
            assert_eq!(rx.len(), f.len() + 10);
        } else {
            assert_eq!(rx, f, "echo only");
            assert_eq!(kinds(&j), [ViolationKind::P3Min]);
        }
    }
}

#[test]
fn inter_byte_gap_p4() {
    for (tol, ok) in [(strict(), false), (Tolerance::lenient(), true)] {
        let (mut l, j) = ready(tol);
        let f = frame(&[0x21, 0xAA]);
        l.write(&f, 30).unwrap(); // P4max 20 strict, 100 lenient
        let rx = l.read(f.len() + 10, 300).unwrap();
        if ok {
            assert_eq!(rx.len(), f.len() + 10);
        } else {
            assert_eq!(rx, f, "echo only");
            assert!(
                kinds(&j).contains(&ViolationKind::InterByte),
                "{:?}",
                j.violations()
            );
        }
    }
}

#[test]
fn bad_request_checksum_is_ignored() {
    let (mut l, j) = ready(strict());
    let mut f = frame(&[0x21, 0xAA]);
    *f.last_mut().unwrap() ^= 0x01;
    l.write(&f, 0).unwrap();
    assert_eq!(l.read(f.len() + 10, 300).unwrap(), f, "echo only");
    assert_eq!(kinds(&j), [ViolationKind::BadFrame]);
}

#[test]
fn frame_for_another_address_is_ignored() {
    // The core never XORs the header target; a frame to 0xFA must not be answered by 0x7A.
    let (mut l, j) = ready(strict());
    let mut f = vec![0x82, 0xFA, 0xF1, 0x21, 0xAA];
    f.push(checksum(&f));
    l.write(&f, 0).unwrap();
    assert_eq!(l.read(f.len() + 10, 300).unwrap(), f);
    assert!(j
        .notes()
        .iter()
        .any(|(_, n)| n.contains("frame for 0xfa ignored")));
}

#[test]
fn echo_and_break_residue_on_the_bus() {
    let (mut l, _) = line(Tolerance::lenient());
    l.sleep_ms(300);
    l.set_break(true).unwrap();
    assert!(l.break_loopback_seen().unwrap());
    l.sleep_ms(25);
    l.set_break(false).unwrap();
    // Residue 0x00 from the released break, then the echo of what we send.
    l.write(&[0x12, 0x34], 0).unwrap();
    assert_eq!(l.read(3, 10).unwrap(), [0x00, 0x12, 0x34]);
}

#[test]
fn late_w1_overruns_the_core_keyword_window() {
    // Finding: the core reads 55 KW1 KW2 within KW2Timeout + 100 = 400 ms from the middle of
    // the stop bit, i.e. up to 300 ms after it ends. ISO W1 alone may be 300 ms; an ECU near
    // that limit is lost (DDT2000 has the same window, RE: IOTBD32V3).
    let mut p = EcuProfile::sirius32();
    p.timing.w1_ms = 290;
    let (mut d, _) = drive(SpecEcu::new(p, sirius(), sirius_scenario()).unwrap());
    match d.init(&cfg(&sirius())).unwrap_err().init() {
        Some(LinkError::NoKeywords { received }) => assert_eq!(received, &[0x55]),
        other => panic!("{other:?}"),
    }
}
