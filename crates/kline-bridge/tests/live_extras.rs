//! Live extras (`LIVE_EXTRAS`, the "advanced" drive profile) against kline-sim: the drive
//! schedule `21AA, 21A9, 21AA, 21AA` stays, one extra Sirius read follows every `n` cycles, the
//! raw replies come as Extra frames, frames the ECU refuses are skipped, and everything on the
//! wire stays Tier 0.

mod common;

use common::*;
use kline_bridge::gate::Role;
use kline_bridge::gatt::{live_flags, Channel, ExtraFrame, LiveSample, Op, ResultCode};
use kline_bridge::profile::{Profile, EXTRA_SCHEDULE};

fn extras(rig: &Rig) -> Vec<ExtraFrame> {
    rig.car
        .channel(Channel::Extra)
        .iter()
        .map(|f| ExtraFrame::decode(f).unwrap())
        .collect()
}

fn sirius_lids(rig: &Rig) -> Vec<u8> {
    rig.car
        .written_payloads()
        .iter()
        .filter(|(t, p)| *t == 0x7A && p.len() == 2 && p[0] == 0x21)
        .map(|(_, p)| p[1])
        .collect()
}

#[test]
fn off_by_default_no_extra_reads_no_extra_frames() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.run_until(15_000);
    let lids = sirius_lids(&rig);
    assert!(lids.iter().all(|l| *l == 0xAA || *l == 0xA9), "{lids:02X?}");
    assert!(rig.car.channel(Channel::Extra).is_empty());
    let last = LiveSample::decode(rig.car.channel(Channel::Live).last().unwrap()).unwrap();
    assert_eq!(last.flags & live_flags::EXTRAS_ON, 0);
}

#[test]
fn every_cycle_interleaves_one_extra_read_and_forwards_raw_replies() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.run_until(5_000);
    let r = rig.call(OWNER, Op::LiveExtras(1), 1_000);
    assert_eq!(r.result, ResultCode::Ok);
    let before = sirius_lids(&rig).len();
    rig.run_for(30_000);
    let lids: Vec<u8> = sirius_lids(&rig)[before..].to_vec();

    // The drive schedule is intact: between two extra reads exactly one AA, A9, AA, AA cycle.
    let base: Vec<u8> = lids
        .iter()
        .copied()
        .filter(|l| *l == 0xAA || *l == 0xA9)
        .collect();
    let a9 = base.iter().filter(|l| **l == 0xA9).count();
    let aa = base.len() - a9;
    assert!(a9 > 20, "{a9} 21A9 reads");
    assert!(aa >= 3 * a9 - 3 && aa <= 3 * a9 + 3, "{aa} vs {a9}");
    let extra_reads: Vec<u8> = lids
        .iter()
        .copied()
        .filter(|l| EXTRA_SCHEDULE.contains(l))
        .collect();
    assert!(
        extra_reads.len() >= a9 - 2 && extra_reads.len() <= a9 + 2,
        "one extra per cycle: {} extras, {a9} cycles",
        extra_reads.len()
    );

    // The fixture knows 21A0 and 21AB; every other extra frame is answered negatively once in
    // the session and then skipped.
    for lid in EXTRA_SCHEDULE {
        let n = extra_reads.iter().filter(|l| **l == lid).count();
        if lid == 0xA0 || lid == 0xAB {
            assert!(n > 5, "{lid:02X} read {n} times");
        } else {
            assert_eq!(n, 1, "{lid:02X} refused once, then skipped");
        }
    }

    let x = extras(&rig);
    assert!(x.iter().all(|f| f.ecu == 0x7A && f.reply.len() >= 2));
    assert!(x.iter().all(|f| f.request[0] == 0x21 && f.reply[0] == 0x61));
    assert!(x.iter().all(|f| f.reply[1] == f.request[1]));
    assert!(x.windows(2).all(|w| w[1].seq == w[0].seq.wrapping_add(1)));
    let count = |lid: u8| x.iter().filter(|f| f.request == [0x21, lid]).count();
    // Every 21A9 and one 21AA per cycle are forwarded raw; refused frames never are.
    assert!(count(0xA9) + 2 >= a9, "{} of {a9}", count(0xA9));
    assert!(count(0xAA) + 2 >= a9 && count(0xAA) <= a9 + 2);
    assert!(count(0xA0) > 5 && count(0xAB) > 5);
    for lid in [0xA1, 0xA2, 0xA4, 0xA8, 0xAD] {
        assert_eq!(count(lid), 0, "{lid:02X}");
    }
    // The raw 21A9 decodes to what Live says (1 byte rpm high, 850 rpm).
    let a9f = x.iter().rfind(|f| f.request == [0x21, 0xA9]).unwrap();
    assert_eq!(u16::from_be_bytes([a9f.reply[2], a9f.reply[3]]), 850);

    let last = LiveSample::decode(rig.car.channel(Channel::Live).last().unwrap()).unwrap();
    assert_ne!(last.flags & live_flags::EXTRAS_ON, 0);
    // A forwarded 21A9 comes right before the Live notification it belongs to.
    let order: Vec<(Channel, Vec<u8>)> = rig
        .car
        .notes
        .iter()
        .filter(|(_, _, c, _)| matches!(c, Channel::Live | Channel::Extra))
        .map(|(_, _, c, f)| (*c, f.clone()))
        .collect();
    let extra_a9_positions: Vec<usize> = rig
        .car
        .notes
        .iter()
        .filter(|(_, _, c, _)| matches!(c, Channel::Live | Channel::Extra))
        .enumerate()
        .filter(|(_, (_, _, c, f))| {
            *c == Channel::Extra && ExtraFrame::decode(f).unwrap().request == [0x21, 0xA9]
        })
        .map(|(i, _)| i)
        .collect();
    assert!(!extra_a9_positions.is_empty());
    for i in extra_a9_positions {
        let x = ExtraFrame::decode(&order[i].1).unwrap();
        let (c, f) = &order[i + 1];
        assert_eq!(*c, Channel::Live, "Live follows the Extra 21A9");
        assert_eq!(
            LiveSample::decode(f).unwrap().t_ms,
            x.t_ms,
            "the same reply"
        );
    }
    rig.assert_wire_tier0_except(&[]);
    rig.car.sirius_journal.assert_no_writes();
    rig.car.sirius_journal.assert_no_violations();

    // Off again: no more extra reads or frames.
    let r = rig.call(OWNER, Op::LiveExtras(0), 1_000);
    assert_eq!(r.result, ResultCode::Ok);
    rig.run_for(500);
    let (n_lids, n_x) = (sirius_lids(&rig).len(), extras(&rig).len());
    rig.run_for(10_000);
    let after = &sirius_lids(&rig)[n_lids..];
    assert!(
        after.iter().all(|l| *l == 0xAA || *l == 0xA9),
        "{after:02X?}"
    );
    assert_eq!(extras(&rig).len(), n_x);
}

#[test]
fn every_n_cycles_reads_one_extra_per_n_cycles() {
    let mut rig = Rig::default_car();
    rig.connect(AGENT, Role::Agent);
    rig.battery(14_100);
    rig.run_until(5_000);
    // Reads only: the agent may set it too.
    assert_eq!(
        rig.call(AGENT, Op::LiveExtras(5), 1_000).result,
        ResultCode::Ok
    );
    let before = sirius_lids(&rig).len();
    rig.run_for(40_000);
    let lids = &sirius_lids(&rig)[before..];
    let a9 = lids.iter().filter(|l| **l == 0xA9).count();
    let n = lids.iter().filter(|l| EXTRA_SCHEDULE.contains(l)).count();
    assert!(a9 > 30);
    assert!(
        n * 5 >= a9 - 6 && n * 5 <= a9 + 6,
        "{n} extras in {a9} cycles"
    );
}

#[test]
fn parked_profile_never_reads_sirius_extras() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    assert_eq!(
        rig.call(OWNER, Op::LiveExtras(1), 1_000).result,
        ResultCode::Ok
    );
    assert_eq!(
        rig.call(OWNER, Op::StartLive(Profile::Parked), 1_000)
            .result,
        ResultCode::Ok
    );
    rig.run_for(10_000);
    let p = rig.car.written_payloads();
    assert!(p.iter().all(|(t, _)| *t == 0x26), "only the BMT: {p:02X?}");
    assert!(rig.car.channel(Channel::Extra).is_empty());
}

#[test]
fn unbonded_peer_cannot_set_extras() {
    let mut rig = Rig::default_car();
    rig.battery(14_100);
    let r = rig.call(STRANGER, Op::LiveExtras(1), 1_000);
    assert_eq!(r.result, ResultCode::NotAllowedForRole);
}

/// A frame refused in one session is tried again in the next one (per-session reset).
#[test]
fn a_refused_frame_is_tried_again_in_a_new_session() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.run_until(5_000);
    assert_eq!(
        rig.call(OWNER, Op::LiveExtras(1), 1_000).result,
        ResultCode::Ok
    );
    rig.run_for(15_000);
    let count = |rig: &Rig| sirius_lids(rig).iter().filter(|l| **l == 0xA1).count();
    assert_eq!(count(&rig), 1);
    // A new session: yield the line and take it back.
    assert_eq!(
        rig.call(OWNER, Op::YieldKline, 5_000).result,
        ResultCode::Ok
    );
    assert_eq!(
        rig.call(OWNER, Op::ResumeKline, 1_000).result,
        ResultCode::Ok
    );
    rig.run_for(20_000);
    assert_eq!(count(&rig), 2, "tried again once in the new session");
}

/// LIVE_EXTRAS restarts the cycle count: sent again just before an extra read was due, the
/// next extra read comes a full interval later.
#[test]
fn live_extras_restarts_the_cycle_count() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.run_until(5_000);
    let a9 = |rig: &Rig| sirius_lids(rig).iter().filter(|l| **l == 0xA9).count();
    let extras = |rig: &Rig| {
        sirius_lids(rig)
            .iter()
            .filter(|l| EXTRA_SCHEDULE.contains(l))
            .count()
    };
    rig.send(OWNER, Op::LiveExtras(16));
    let start = a9(&rig);
    assert!(rig.run_until_pred(30_000, |r| a9(r) >= start + 12));
    assert_eq!(extras(&rig), 0);
    rig.send(OWNER, Op::LiveExtras(16));
    let again = a9(&rig);
    assert!(rig.run_until_pred(30_000, |r| extras(r) > 0));
    assert!(
        a9(&rig) >= again + 15,
        "a full 16 cycles after the second LIVE_EXTRAS: {} vs {again}",
        a9(&rig)
    );
}
