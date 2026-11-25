//! Lock-when-moving end to end (feature `standing-lock`, decision D37): arm with two owner
//! codes, drive off, the bridge switches to the BMT, sends `31 A0 00 01`, goes back to the
//! Sirius, and sends `31 A1 00 01` when the engine stops.
#![cfg(feature = "standing-lock")]

mod common;

use common::*;
use kline_bridge::gate::Role;
use kline_bridge::gatt::{live_flags, Channel, LiveSample, Op, Response, ResultCode};
use kline_bridge::profile::{EcuId, Profile};
use kline_sim::ecu::{FaultRule, ReplyFault};
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

/// Arm while parked (engine off, 12.6 V), then the engine starts (14.1 V). Arming is refused
/// while the car may be moving (review B-2).
fn arm(rig: &mut Rig) {
    rig.battery(12_600);
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
    rig.battery(14_100);
    rig.car.bmt_journal.assert_no_writes();
}

#[test]
fn locks_when_moving_and_unlocks_when_the_engine_stops() {
    for following in [false, true] {
        locks_and_unlocks(following);
    }
}

fn locks_and_unlocks(following: bool) {
    let mut rig = Rig::new(driving_sirius(), bmt_model(following, bmt_scenario(true)));
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
    rig.battery(12_600);
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

/// The BMT lock-memory models: (follows the K-line actuators, reads unlocked at the start).
/// A stale memory that keeps reading "unlocked" after our lock is the hard case (review B4-1).
const MODELS: [(bool, bool); 3] = [(false, true), (false, false), (true, true)];

#[test]
fn disarm_after_the_lock_still_unlocks_at_engine_stop() {
    for (following, unlocked) in MODELS {
        let mut rig = Rig::new(
            driving_sirius(),
            bmt_model(following, bmt_scenario(unlocked)),
        );
        rig.connect(OWNER, Role::Owner);
        rig.battery(14_100);
        arm(&mut rig);
        let locked = rig.run_until_pred(30_000, |r| r.bridge.lwm().locked_by_us());
        assert!(locked);
        rig.run_for(8_000);
        assert_eq!(rig.call(OWNER, Op::LwmDisarm, 1_000).result, ResultCode::Ok);
        rig.run_for(50_000);
        let w = rig.car.bmt_journal.writes();
        assert_eq!(w.len(), 2, "{following} {unlocked}: {w:?}");
        assert_eq!(w[1].payload, vec![0x31, 0xA1, 0x00, 0x01]);
    }
}

/// Review B4-1 (reviewer's break tests): the lock memory keeps reading "unlocked" after the
/// K-line lock. A disarm while driving (later, or right after the lock) reads it, and the owed
/// unlock must still be sent at engine stop.
#[test]
fn rev4_stale_lock_memory_disarm_keeps_unlock() {
    for wait_ms in [3_000, 0] {
        for (following, unlocked) in MODELS {
            let mut rig = Rig::new(
                driving_sirius(),
                bmt_model(following, bmt_scenario(unlocked)),
            );
            rig.connect(OWNER, Role::Owner);
            rig.battery(14_100);
            arm(&mut rig);
            assert!(rig.run_until_pred(30_000, |r| r.bridge.lwm().locked_by_us()));
            rig.run_for(wait_ms);
            assert_eq!(rig.call(OWNER, Op::LwmDisarm, 1_000).result, ResultCode::Ok);
            rig.run_for(60_000);
            let w = bmt_payloads(&rig);
            assert!(
                w.contains(&vec![0x31, 0xA1, 0x00, 0x01]),
                "no unlock after disarm ({wait_ms} ms, {following} {unlocked}): {w:02X?}"
            );
        }
    }
}

/// Review B4-1 (reviewer's break test): stale memory reading "unlocked", K-line unlock refused:
/// the app UNLOCK finds "already unlocked" and pulses nothing; that must not end the duty.
#[test]
fn rev4_stale_memory_app_unlock_ends_duty_without_pulse() {
    let bmt = bmt_ecu(bmt_scenario(true)).with_fault(FaultRule::on(
        &[0x31, 0xA1],
        ReplyFault::Negative { nrc: 0x22 },
    ));
    let mut rig = Rig::new(driving_sirius(), bmt);
    rig.connect(OWNER, Role::Owner);
    arm(&mut rig);
    assert!(rig.run_until_pred(60_000, |r| count(&bmt_payloads(r), 0xA1) == 1));
    assert!(rig.bridge.lwm().locked_by_us());
    rig.battery(12_600);
    let pulses = rig.pin_highs().len();
    let _ = rig.call(OWNER, Op::Unlock, 30_000);
    assert!(
        rig.bridge.lwm().locked_by_us() || rig.pin_highs().len() > pulses,
        "duty ended with no pulse and no K-line unlock"
    );
}

#[test]
fn the_k_line_cannot_be_yielded_while_an_unlock_is_owed() {
    let mut rig = Rig::new(driving_sirius(), bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.connect(AGENT, Role::Agent);
    rig.battery(14_100);
    arm(&mut rig);
    assert!(rig.run_until_pred(30_000, |r| r.bridge.lwm().locked_by_us()));
    assert_eq!(
        rig.call(AGENT, Op::YieldKline, 1_000).result,
        ResultCode::LwmActive
    );
    rig.run_for(50_000);
    let w = rig.car.bmt_journal.writes();
    assert_eq!(w.last().unwrap().payload, vec![0x31, 0xA1, 0x00, 0x01]);
    assert_eq!(
        rig.call(AGENT, Op::YieldKline, 1_000).result,
        ResultCode::Ok
    );
}

/// Driving at 40 km/h for good (no engine stop).
fn cruising_sirius() -> kline_sim::ecu::SpecEcu {
    sirius_ecu(sirius_scenario())
        .with_change_after(&[0x21, 0xA9], 1_000, "Vehicle speed", Value::Number(40.0))
        .with_change_after(&[0x21, 0xA9], 1_000, "Engine speed", Value::Number(2_200.0))
}

/// Review B1: neither role can move the bridge off the Sirius while lock-when-moving needs it,
/// so nobody can make it unlock and relock the car while driving.
#[test]
fn clients_cannot_toggle_the_locks_while_driving() {
    let mut rig = Rig::new(cruising_sirius(), bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.connect(AGENT, Role::Agent);
    rig.battery(14_100);
    arm(&mut rig);
    assert!(rig.run_until_pred(30_000, |r| r.bridge.lwm().locked_by_us()));
    rig.run_for(10_000);
    for _ in 0..4 {
        for client in [AGENT, OWNER] {
            let r = rig.call(client, Op::StartLive(Profile::Parked), 1_000);
            assert_eq!(r.result, ResultCode::LwmActive);
            let r = rig.call(
                client,
                Op::Request {
                    ecu: EcuId::Bmt,
                    payload: vec![0x21, 0xA3],
                },
                1_000,
            );
            assert_eq!(r.result, ResultCode::LwmActive);
            assert_eq!(
                rig.call(client, Op::YieldKline, 1_000).result,
                ResultCode::LwmActive
            );
            // Review r3 minor 3: PREPARE_WRITE is held off the BMT as well.
            let r = rig.call(
                client,
                Op::PrepareWrite {
                    ecu: EcuId::Bmt,
                    identification: vec![0x61, 0x80, 0x01],
                    payload: vec![0x3B, 0xD2, 0x03, 0x00],
                },
                1_000,
            );
            assert_eq!(r.result, ResultCode::LwmActive);
        }
        rig.run_for(12_000);
    }
    let w = rig.car.bmt_journal.writes();
    assert_eq!(w.len(), 1, "only the one lock: {w:?}");
    assert!(rig.bridge.lwm().locked_by_us());
}

/// Review M4: the lock is written but its reply is lost: the unlock at engine stop still
/// comes.
#[test]
fn a_lost_lock_reply_still_unlocks_at_engine_stop() {
    let bmt =
        bmt_ecu(bmt_scenario(true)).with_fault(FaultRule::on(&[0x31, 0xA0], ReplyFault::NoReply));
    let mut rig = Rig::new(driving_sirius(), bmt);
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
    assert_eq!(w.first(), Some(&vec![0x31, 0xA0, 0x00, 0x01]));
    assert!(w.contains(&vec![0x31, 0xA1, 0x00, 0x01]), "{w:02X?}");
}

/// Review M9 and B1: no charging voltage (12.6 V with the engine running), and after the lock
/// the Sirius stops answering (ignition off): the bridge keeps trying the Sirius, sees it gone
/// after 3 s of failed attempts, and unlocks.
#[test]
fn the_sirius_going_silent_unlocks_without_a_charging_voltage() {
    // About 10 reads per second: the first 21-requests are answered, later ones get nothing.
    let sirius =
        cruising_sirius().with_fault(FaultRule::on(&[0x21], ReplyFault::NoReply).after(400));
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    arm(&mut rig);
    assert!(rig.run_until_pred(60_000, |r| !r.car.bmt_journal.writes().is_empty()));
    rig.battery(12_600);
    rig.run_for(120_000);
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
    assert!(rig.car.sleeps.is_empty() || !rig.bridge.lwm().locked_by_us());
}

/// Review M5 (surviving mutation): below 12.0 V with the engine stopped, the owed unlock is
/// sent before the bridge sleeps.
#[test]
fn a_low_battery_never_sleeps_over_an_owed_unlock() {
    let sirius =
        cruising_sirius().with_fault(FaultRule::on(&[0x21], ReplyFault::NoReply).after(400));
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    arm(&mut rig);
    assert!(rig.run_until_pred(60_000, |r| !r.car.bmt_journal.writes().is_empty()));
    rig.battery(11_900);
    rig.run_for(120_000);
    let w: Vec<Vec<u8>> = rig
        .car
        .bmt_journal
        .writes()
        .into_iter()
        .map(|w| w.payload)
        .collect();
    assert_eq!(w.last(), Some(&vec![0x31, 0xA1, 0x00, 0x01]), "{w:02X?}");
    let unlock_t = rig.car.bmt_journal.writes().last().unwrap().t_ms;
    assert!(
        rig.car.sleeps.iter().all(|s| s.0 > unlock_t),
        "{:?}",
        rig.car.sleeps
    );
    assert!(!rig.car.sleeps.is_empty(), "then it sleeps");
}

fn cruise_then_stop() -> kline_sim::ecu::SpecEcu {
    sirius_ecu(sirius_scenario())
        .with_change_after(&[0x21, 0xA9], 1_000, "Vehicle speed", Value::Number(40.0))
        .with_change_after(&[0x21, 0xA9], 1_000, "Engine speed", Value::Number(2_200.0))
        .with_change_after(&[0x21, 0xA9], 60_000, "Vehicle speed", Value::Number(0.0))
        .with_change_after(&[0x21, 0xA9], 60_000, "Engine speed", Value::Number(0.0))
}

fn bmt_payloads(rig: &Rig) -> Vec<Vec<u8>> {
    rig.car
        .bmt_journal
        .writes()
        .into_iter()
        .map(|w| w.payload)
        .collect()
}

/// Review B-1: arming again while locked is refused, so the unlock duty and its risks stay.
#[test]
fn r2_rearm_while_locked_keeps_the_unlock_duty() {
    let mut rig = Rig::new(cruise_then_stop(), bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    arm(&mut rig);
    assert!(rig.run_until_pred(30_000, |r| r.bridge.lwm().locked_by_us()));
    rig.run_for(6_000);
    assert_eq!(
        rig.call(OWNER, Op::LwmArm, 15_000).result,
        ResultCode::LwmActive
    );
    rig.run_for(200_000);
    assert_eq!(
        bmt_payloads(&rig).last(),
        Some(&vec![0x31, 0xA1, 0x00, 0x01]),
        "unlock duty lost"
    );
}

/// Review B-2: arming never takes the bridge off the Sirius while driving.
#[test]
fn r2_rearm_while_driving_leaves_the_sirius() {
    let mut rig = Rig::new(cruising_sirius(), bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    arm(&mut rig);
    assert!(rig.run_until_pred(30_000, |r| r.bridge.lwm().locked_by_us()));
    rig.run_for(6_000);
    assert_eq!(
        rig.call(OWNER, Op::LwmArm, 15_000).result,
        ResultCode::LwmActive
    );
    // Disarmed but still locked: still refused (an unlock is owed).
    assert_eq!(rig.call(OWNER, Op::LwmDisarm, 1_000).result, ResultCode::Ok);
    assert_eq!(
        rig.call(OWNER, Op::LwmArm, 15_000).result,
        ResultCode::LwmActive
    );
}

/// Review B-2: a first arming is refused while the car may move (charging, no fresh speed or
/// a speed above 0).
#[test]
fn arming_is_refused_while_the_car_may_move() {
    let mut rig = Rig::new(cruising_sirius(), bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    assert_eq!(
        rig.call(OWNER, Op::LwmArm, 15_000).result,
        ResultCode::VehicleMoving
    );
    rig.run_for(10_000);
    assert_eq!(
        rig.call(OWNER, Op::LwmArm, 15_000).result,
        ResultCode::VehicleMoving
    );
    assert!(rig.car.bmt_journal.writes().is_empty());
}

/// Review m-4 (N22): a wrong second arming code leaves the bridge disarmed, and nothing moves.
#[test]
fn a_wrong_second_arming_code_leaves_it_disarmed() {
    let mut rig = Rig::new(driving_sirius(), bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    let (id, code) = challenge(&rig.call(OWNER, Op::LwmArm, 15_000));
    let (id2, _) = challenge(&rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        10_000,
    ));
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id2,
            code: "AAAAAA".into(),
        },
        10_000,
    );
    assert_eq!(r.result, ResultCode::ApprovalMismatch);
    assert!(!rig.bridge.lwm().is_armed());
    rig.battery(14_100);
    rig.run_for(60_000);
    rig.car.bmt_journal.assert_no_writes();
}

/// Review m-4 (N21): no standing lock below 12.0 V (the engine runs, so the bridge stays up).
#[test]
fn no_standing_lock_below_twelve_volts() {
    let mut rig = Rig::new(cruising_sirius(), bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    arm(&mut rig);
    rig.battery(11_900);
    rig.send(OWNER, Op::StartLive(Profile::Drive));
    rig.run_for(40_000);
    rig.car.bmt_journal.assert_no_writes();
}

/// Review m-4 (N20): below 11.8 V with an unlock owed, the unlock is sent before the critical
/// shutdown.
#[test]
fn a_critical_battery_never_shuts_down_over_an_owed_unlock() {
    let sirius =
        cruising_sirius().with_fault(FaultRule::on(&[0x21], ReplyFault::NoReply).after(400));
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    arm(&mut rig);
    assert!(rig.run_until_pred(60_000, |r| !r.car.bmt_journal.writes().is_empty()));
    rig.battery(11_700);
    rig.run_for(120_000);
    let w = rig.car.bmt_journal.writes();
    assert_eq!(
        w.last().map(|w| w.payload.clone()),
        Some(vec![0x31, 0xA1, 0x00, 0x01])
    );
    let unlock_t = w.last().unwrap().t_ms;
    assert!(
        rig.car.sleeps.iter().all(|s| s.0 > unlock_t),
        "{:?}",
        rig.car.sleeps
    );
}

/// Engine off, standing.
fn parked_sirius() -> kline_sim::ecu::SpecEcu {
    sirius_ecu(sirius_scenario().number("Engine speed", 0.0))
}

fn arm_parked(rig: &mut Rig) {
    rig.battery(12_600);
    let (id, code) = challenge(&rig.call(OWNER, Op::LwmArm, 15_000));
    let (id2, code2) = challenge(&rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        10_000,
    ));
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id2,
            code: code2,
        },
        10_000,
    );
    assert_eq!(r.result, ResultCode::Ok, "{r:?}");
}

fn count(w: &[Vec<u8>], fn_byte: u8) -> usize {
    w.iter().filter(|p| p[1] == fn_byte).count()
}

/// Review B-3 (reviewer's break test, adapted to the fix): three refused unlocks make the
/// bridge give up, disarm and report it; a later drive does not lock; once the owner arms
/// again, the next lock gets its unlock.
#[test]
fn r3_a_give_up_never_poisons_later_unlocks() {
    for following in [false, true] {
        give_up_then_rearm(following);
    }
}

fn give_up_then_rearm(following: bool) {
    let change = |e: kline_sim::ecu::SpecEcu, t: u64, v: f64, rpm: f64| {
        e.with_change_after(&[0x21, 0xA9], t, "Vehicle speed", Value::Number(v))
            .with_change_after(&[0x21, 0xA9], t, "Engine speed", Value::Number(rpm))
    };
    let mut sirius = parked_sirius();
    for (t, v, rpm) in [
        (20_000, 40.0, 2_200.0),
        (50_000, 0.0, 0.0),
        (100_000, 40.0, 2_200.0),
        (130_000, 0.0, 0.0),
        (230_000, 40.0, 2_200.0),
        (260_000, 0.0, 0.0),
    ] {
        sirius = change(sirius, t, v, rpm);
    }
    let bmt = bmt_model(following, bmt_scenario(following)).with_fault(FaultRule {
        times: Some(3),
        ..FaultRule::on(&[0x31, 0xA1], ReplyFault::Negative { nrc: 0x22 })
    });
    let mut rig = Rig::new(sirius, bmt);
    rig.connect(OWNER, Role::Owner);
    rig.send(OWNER, Op::StartLive(Profile::Drive));
    rig.run_for(3_000);
    arm_parked(&mut rig);
    rig.run_until(95_000);
    let w = bmt_payloads(&rig);
    assert_eq!((count(&w, 0xA0), count(&w, 0xA1)), (1, 3), "{w:02X?}");
    assert!(!rig.bridge.lwm().locked_by_us());
    assert!(!rig.bridge.lwm().is_armed(), "a give-up disarms");
    let flags = rig.bridge.status().flags;
    assert_ne!(flags & kline_bridge::gatt::status_flags::LWM_GAVE_UP, 0);
    assert_eq!(flags & kline_bridge::gatt::status_flags::LWM_ARMED, 0);
    // Drive 2: no lock without the owner.
    rig.run_until(190_000);
    assert_eq!(bmt_payloads(&rig), w, "no lock after a give-up");
    // The owner arms again (parked, engine off); drive 3 locks and unlocks.
    arm_parked(&mut rig);
    assert!(!rig.bridge.lwm().gave_up());
    rig.run_until(320_000);
    let w = bmt_payloads(&rig);
    assert_eq!((count(&w, 0xA0), count(&w, 0xA1)), (2, 4), "{w:02X?}");
    assert_eq!(w.last().unwrap()[1], 0xA1);
    assert!(!rig.bridge.lwm().locked_by_us());
}

/// Reviews B-3 and B4-1: LWM_DISARM with an unlock owed reads the BMT lock state once; the duty
/// ends only on an observed change (a "locked" read, then an "unlocked" one).
#[test]
fn r3_disarm_ends_the_duty_once_a_read_shows_the_doors_unlocked() {
    for following in [false, true] {
        disarm_reads(following);
    }
}

fn disarm_reads(following: bool) {
    // Drives off, then stands with the engine idling: the duty stays (no engine stop).
    let sirius = cruising_sirius()
        .with_change_after(&[0x21, 0xA9], 30_000, "Vehicle speed", Value::Number(0.0))
        .with_change_after(&[0x21, 0xA9], 30_000, "Engine speed", Value::Number(800.0));
    // Locked after our lock; the first 21A3 (the first disarm's read) says locked, later reads
    // unlocked (the owner opened by hand): an observed change.
    let bmt = bmt_model(following, bmt_scenario(following)).with_change_after(
        &[0x21, 0xA3],
        1,
        "Door lock memory",
        text("unlocked"),
    );
    let mut rig = Rig::new(sirius, bmt);
    rig.connect(OWNER, Role::Owner);
    arm(&mut rig);
    assert!(rig.run_until_pred(30_000, |r| r.bridge.lwm().locked_by_us()));
    rig.run_for(40_000);
    assert!(rig.bridge.lwm().locked_by_us());
    assert_eq!(rig.call(OWNER, Op::LwmDisarm, 1_000).result, ResultCode::Ok);
    rig.run_for(15_000);
    assert!(
        rig.bridge.lwm().locked_by_us(),
        "read showed locked: duty stays"
    );
    // In both models the memory reads locked here (the following one by the K-line lock).
    assert_eq!(
        rig.bridge.status().lock,
        kline_bridge::gatt::LockState::Locked
    );
    assert_eq!(rig.call(OWNER, Op::LwmDisarm, 1_000).result, ResultCode::Ok);
    assert!(rig.run_until_pred(15_000, |r| !r.bridge.lwm().locked_by_us()));
    assert_eq!(bmt_payloads(&rig), vec![vec![0x31, 0xA0, 0x00, 0x01]]);
    // Nothing is held any more: the K line can be yielded.
    assert_eq!(
        rig.call(OWNER, Op::YieldKline, 15_000).result,
        ResultCode::Ok
    );
}

/// Reviews B-3 and B4-1: the owner's app UNLOCK reads "locked", pulses, and the bit changes to
/// "unlocked": that observed change ends an unlock duty whose K-line unlock keeps being
/// refused.
#[test]
fn r3_a_verified_app_unlock_ends_the_duty() {
    for following in [false, true] {
        app_unlock(following);
    }
}

fn app_unlock(following: bool) {
    let bmt = bmt_model(following, bmt_scenario(following))
        .with_fault(FaultRule::on(
            &[0x31, 0xA1],
            ReplyFault::Negative { nrc: 0x22 },
        ))
        .with_change_after(&[0x21, 0xA3], 200, "Door lock memory", text("unlocked"));
    let mut rig = Rig::new(driving_sirius(), bmt);
    rig.connect(OWNER, Role::Owner);
    arm(&mut rig);
    assert!(rig.run_until_pred(60_000, |r| count(&bmt_payloads(r), 0xA1) == 1));
    rig.battery(12_600);
    let r = rig.call(OWNER, Op::Unlock, 30_000);
    assert_eq!(r.result, ResultCode::Ok, "{r:?}");
    assert!(!rig.bridge.lwm().locked_by_us());
    let n = count(&bmt_payloads(&rig), 0xA1);
    assert!(n < 3, "ended before the give-up");
    rig.run_for(60_000);
    assert_eq!(
        count(&bmt_payloads(&rig), 0xA1),
        n,
        "no unlock attempts after it"
    );
    assert!(!rig.bridge.lwm().gave_up());
}

/// Review r3 minor 2: armed and parked, the owner PREPAREs a BMT request and drives off
/// without confirming. The challenge is dropped as soon as the engine may run (here the
/// charging voltage), so lock-when-moving gets the Sirius back and locks; its CONFIRM is
/// answered LwmActive.
#[test]
fn r3_a_pending_bmt_challenge_never_hides_the_drive() {
    let sirius = parked_sirius()
        .with_change_after(&[0x21, 0xA9], 30_000, "Vehicle speed", Value::Number(40.0))
        .with_change_after(
            &[0x21, 0xA9],
            30_000,
            "Engine speed",
            Value::Number(2_200.0),
        );
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.send(OWNER, Op::StartLive(Profile::Drive));
    rig.run_for(3_000);
    arm_parked(&mut rig);
    rig.run_for(3_000);
    let (id, code) = challenge(&rig.call(
        OWNER,
        Op::Prepare {
            ecu: EcuId::Bmt,
            payload: vec![0x31, 0xA0, 0x00, 0x01],
        },
        15_000,
    ));
    let t0 = rig.now();
    rig.battery(14_100);
    assert!(rig.run_until_pred(200_000, |r| !r.car.bmt_journal.writes().is_empty()));
    let w = rig.car.bmt_journal.writes();
    assert!(
        w[0].t_ms < t0 + 40_000,
        "locked at {} (prepare at {t0})",
        w[0].t_ms
    );
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        5_000,
    );
    assert_eq!(r.result, ResultCode::LwmActive);
    assert_eq!(rig.car.bmt_journal.writes().len(), 1);
}

/// Review r3 minor 5: arming at a standstill with the engine idling is refused (parked,
/// engine off only), at a charging voltage and without one.
#[test]
fn r3_arming_with_the_engine_idling_is_refused() {
    let mut rig = Rig::new(sirius_ecu(sirius_scenario()), bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.send(OWNER, Op::StartLive(Profile::Drive));
    rig.run_for(3_000);
    // Refused at once, on the command (no BMT session for it).
    for mv in [14_100, 12_600] {
        rig.battery(mv);
        rig.run_for(3_000);
        let c = rig.send(OWNER, Op::LwmArm);
        assert!(rig.run_until_pred(15_000, |r| !r.car.responses_for(OWNER, c).is_empty()));
        let r = rig.car.responses_for(OWNER, c);
        assert_eq!(
            r[0].result,
            ResultCode::VehicleMoving,
            "at {mv} mV, not queued: {r:?}"
        );
    }
    assert!(!rig.bridge.lwm().is_armed());
}

/// Review r3 minor 1: parked on a charger (13.6 V, engine off), arming works.
#[test]
fn r3_arming_on_a_charger_works() {
    let mut rig = Rig::new(parked_sirius(), bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(13_600);
    rig.run_for(5_000);
    let (id, code) = challenge(&rig.call(OWNER, Op::LwmArm, 15_000));
    let (id2, code2) = challenge(&rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        10_000,
    ));
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id2,
            code: code2,
        },
        10_000,
    );
    assert_eq!(r.result, ResultCode::Ok, "{r:?}");
    assert!(rig.bridge.lwm().is_armed());
}

/// Review B4-1: the give-up report persists over lock-state reads (parked polls, even an
/// observed change) and ends only by the owner's LWM_ACK (owner only) or a new arming.
#[test]
fn r4_the_give_up_flag_persists_until_the_owner_acknowledges() {
    use kline_bridge::gatt::status_flags::LWM_GAVE_UP;
    let bmt = bmt_ecu(bmt_scenario(false))
        .with_fault(FaultRule::on(
            &[0x31, 0xA1],
            ReplyFault::Negative { nrc: 0x22 },
        ))
        .with_change_after(&[0x21, 0xA3], 3_000, "Door lock memory", text("unlocked"));
    let mut rig = Rig::new(driving_sirius(), bmt);
    rig.connect(OWNER, Role::Owner);
    rig.connect(AGENT, Role::Agent);
    arm(&mut rig);
    assert!(rig.run_until_pred(90_000, |r| r.bridge.lwm().gave_up()));
    rig.battery(12_600);
    assert_eq!(
        rig.call(OWNER, Op::StartLive(Profile::Parked), 5_000)
            .result,
        ResultCode::Ok
    );
    rig.run_for(20_000);
    assert_eq!(
        rig.bridge.status().flags & LWM_GAVE_UP,
        LWM_GAVE_UP,
        "reads keep it"
    );
    assert_eq!(
        rig.call(AGENT, Op::LwmAck, 1_000).result,
        ResultCode::NotAllowedForRole
    );
    assert_eq!(rig.bridge.status().flags & LWM_GAVE_UP, LWM_GAVE_UP);
    assert_eq!(rig.call(OWNER, Op::LwmAck, 1_000).result, ResultCode::Ok);
    assert_eq!(rig.bridge.status().flags & LWM_GAVE_UP, 0);
    assert!(!rig.bridge.lwm().is_armed());
}

/// Review nit: repeated LWM_DISARM with an unlock owed queues one lock-state read at a time.
#[test]
fn r4_repeated_disarms_queue_one_lock_check() {
    let mut rig = Rig::new(cruising_sirius(), bmt_ecu(bmt_scenario(false)));
    rig.connect(OWNER, Role::Owner);
    arm(&mut rig);
    assert!(rig.run_until_pred(30_000, |r| r.bridge.lwm().locked_by_us()));
    rig.run_for(6_000);
    let reads = |rig: &Rig| {
        rig.car
            .written_payloads()
            .iter()
            .filter(|(t, p)| *t == EcuId::Bmt.address() && p[..] == [0x21, 0xA3])
            .count()
    };
    let before = reads(&rig);
    for _ in 0..6 {
        rig.send(OWNER, Op::LwmDisarm);
    }
    rig.run_for(20_000);
    assert_eq!(reads(&rig) - before, 1);
    assert!(rig.bridge.lwm().locked_by_us());
}

/// Live extras at the highest rate do not change lock-when-moving: same lock and unlock, the
/// drive schedule keeps its `21 A9` samples (review: "Sirius stays polled").
#[test]
fn locks_and_unlocks_with_live_extras_on() {
    let mut rig = Rig::new(driving_sirius(), bmt_model(true, bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    assert_eq!(
        rig.call(OWNER, Op::LiveExtras(1), 1_000).result,
        ResultCode::Ok
    );
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
    assert!(!rig.car.channel(Channel::Extra).is_empty());
    rig.assert_wire_tier0_except(&[
        (0x26, vec![0x31, 0xA0, 0x00, 0x01]),
        (0x26, vec![0x31, 0xA1, 0x00, 0x01]),
    ]);
    assert!(!rig.bridge.lwm().locked_by_us());
}
