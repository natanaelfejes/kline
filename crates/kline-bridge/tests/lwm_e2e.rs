//! Lock-when-moving end to end (feature `standing-lock`, decision D37): arm with two owner
//! codes, drive off, the bridge switches to the BMT, sends `31 A0 00 01`, goes back to the
//! Sirius, and sends `31 A1 00 01` when the engine stops.
#![cfg(feature = "standing-lock")]

mod common;

use common::*;
use kline_bridge::gate::Role;
use kline_bridge::gatt::{live_flags, Channel, LiveSample, Op, Response, ResultCode};
use kline_spec::Value;

fn challenge(r: &Response) -> (u32, String) {
    assert_eq!(r.result, ResultCode::Challenge, "{r:?}");
    let id = u32::from_le_bytes(r.data[..4].try_into().unwrap());
    (id, String::from_utf8(r.data[5..].to_vec()).unwrap())
}

/// The car drives off 2 s after the first `21 A9`, at 40 km/h, and the engine stops 30 s
/// after it.
fn driving_sirius() -> kline_sim::ecu::SpecEcu {
    sirius_ecu(sirius_scenario())
        .with_change_after(&[0x21, 0xA9], 2_000, "Vehicle speed", Value::Number(40.0))
        .with_change_after(&[0x21, 0xA9], 2_000, "Engine speed", Value::Number(2_200.0))
        .with_change_after(&[0x21, 0xA9], 30_000, "Vehicle speed", Value::Number(0.0))
        .with_change_after(&[0x21, 0xA9], 30_000, "Engine speed", Value::Number(0.0))
}

fn arm(rig: &mut Rig) {
    let (id, code) = challenge(&rig.call(OWNER, Op::LwmArm, 15_000));
    let (id2, code2) = challenge(&rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        10_000,
    ));
    assert_ne!(id, id2);
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id2,
            code: code2,
        },
        10_000,
    );
    assert_eq!(r.result, ResultCode::Ok);
    assert!(rig.bridge.lwm().is_armed());
    rig.car.bmt_journal.assert_no_writes();
}

#[test]
fn locks_when_moving_and_unlocks_when_the_engine_stops() {
    let mut rig = Rig::new(driving_sirius(), bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    arm(&mut rig);
    rig.run_for(60_000);

    let w: Vec<Vec<u8>> = rig
        .car
        .bmt_journal
        .writes()
        .into_iter()
        .map(|w| w.payload)
        .collect();
    assert_eq!(
        w,
        vec![vec![0x31, 0xA0, 0x00, 0x01], vec![0x31, 0xA1, 0x00, 0x01]]
    );
    rig.car.sirius_journal.assert_no_writes();
    rig.assert_wire_tier0_except(&[
        (0x26, vec![0x31, 0xA0, 0x00, 0x01]),
        (0x26, vec![0x31, 0xA1, 0x00, 0x01]),
    ]);
    // Lock 3 s (hold) after 40 km/h was first seen, not before.
    let lock_t = rig.car.bmt_journal.writes()[0].t_ms;
    let first_fast = rig
        .car
        .channel(Channel::Live)
        .iter()
        .map(|f| LiveSample::decode(f).unwrap())
        .find(|s| s.speed_kmh == 40)
        .unwrap();
    assert!(lock_t >= u64::from(first_fast.t_ms) + 3_000);
    // The live stream showed "locked by the bridge" between the two.
    assert!(rig
        .car
        .channel(Channel::Live)
        .iter()
        .any(|f| LiveSample::decode(f).unwrap().flags & live_flags::LWM_LOCKED != 0));
    assert!(!rig.bridge.lwm().locked_by_us());
    // Never two sessions at once: every BMT frame comes after a Sirius Stop (82) and vice
    // versa.
    let mut cur: Option<u8> = None;
    for (t, p) in rig.car.written_payloads() {
        match cur {
            Some(c) if c != t => {
                panic!("frame to {t:#04x} while the session with {c:#04x} is open")
            }
            _ => {}
        }
        cur = if p == [0x82] { None } else { Some(t) };
    }
    assert!(
        rig.pin_highs().is_empty(),
        "the K-line path never touches the fob"
    );
}

#[test]
fn not_armed_means_no_writes_while_driving() {
    let mut rig = Rig::new(driving_sirius(), bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.run_for(60_000);
    rig.car.bmt_journal.assert_no_writes();
    rig.car.sirius_journal.assert_no_writes();
    rig.assert_wire_tier0_except(&[]);
}

#[test]
fn a_wrong_arming_code_leaves_it_disarmed() {
    let mut rig = Rig::new(driving_sirius(), bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    let (id, _) = challenge(&rig.call(OWNER, Op::LwmArm, 15_000));
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code: "AAAAAA".into(),
        },
        10_000,
    );
    assert_eq!(r.result, ResultCode::ApprovalMismatch);
    assert!(!rig.bridge.lwm().is_armed());
    rig.run_for(60_000);
    rig.car.bmt_journal.assert_no_writes();
}

#[test]
fn disarm_after_the_lock_still_unlocks_at_engine_stop() {
    let mut rig = Rig::new(driving_sirius(), bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    arm(&mut rig);
    let locked = rig.run_until_pred(30_000, |r| r.bridge.lwm().locked_by_us());
    assert!(locked);
    rig.run_for(8_000);
    assert_eq!(rig.call(OWNER, Op::LwmDisarm, 1_000).result, ResultCode::Ok);
    rig.run_for(50_000);
    let w = rig.car.bmt_journal.writes();
    assert_eq!(w.len(), 2, "{w:?}");
    assert_eq!(w[1].payload, vec![0x31, 0xA1, 0x00, 0x01]);
}
