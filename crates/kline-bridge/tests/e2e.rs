//! The bridge against kline-sim: Sirius and BMT on one virtual K line (tests/common).

mod common;

use common::*;
use kline_bridge::bridge::SleepReason;
use kline_bridge::gate::Role;
use kline_bridge::gatt::{
    status_flags, BridgeState, Channel, LiveSample, LockState, Op, ResultCode, Status,
};
use kline_bridge::hal::Clock;
use kline_bridge::profile::{EcuId, Profile};
use kline_bridge::runner::Stepped;

fn last_status(rig: &Rig) -> Status {
    Status::decode(rig.car.channel(Channel::Status).last().expect("a status")).unwrap()
}

#[test]
fn boot_inits_the_sirius_and_streams_the_drive_profile() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.run_until(10_000);

    // The first frame on the wire after the 5-baud init goes to the Sirius.
    let p = rig.car.written_payloads();
    assert!(!p.is_empty());
    assert!(
        p.iter().all(|(t, _)| *t == 0x7A),
        "only the Sirius: {p:02X?}"
    );
    let aa = p.iter().filter(|(_, x)| x == &[0x21, 0xAA]).count();
    let a9 = p.iter().filter(|(_, x)| x == &[0x21, 0xA9]).count();
    assert!(a9 > 10, "{a9} 21A9 polls");
    assert!(
        aa >= 3 * a9 - 3 && aa <= 3 * a9 + 3,
        "AA,A9,AA,AA: {aa} vs {a9}"
    );

    let live: Vec<LiveSample> = rig
        .car
        .channel(Channel::Live)
        .iter()
        .map(|f| LiveSample::decode(f).unwrap())
        .collect();
    let last = live.last().unwrap();
    assert_eq!(last.rpm, 850);
    assert_eq!(last.speed_kmh, 0);
    assert_eq!(last.coolant_c, 90);
    assert_eq!(last.batt_cv, 1430);
    assert_eq!(last.inj_us, 2400);
    assert_eq!(last.fuel_mlh, 960);
    assert!(live
        .windows(2)
        .all(|w| w[1].seq == w[0].seq.wrapping_add(1)));

    let st = last_status(&rig);
    assert_eq!(st.state, BridgeState::Session);
    assert_eq!(st.ecu, 0x7A);
    assert_eq!(st.profile, Profile::Drive);
    rig.assert_wire_tier0_except(&[]);
    rig.car.sirius_journal.assert_no_writes();
    rig.car.bmt_journal.assert_no_writes();
    rig.car.sirius_journal.assert_no_violations();
}

#[test]
fn parked_profile_polls_the_bmt_only() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    let r = rig.call(OWNER, Op::StartLive(Profile::Parked), 100);
    assert_eq!(r.result, ResultCode::Ok);
    rig.run_for(8_000);
    let p = rig.car.written_payloads();
    assert!(p.iter().all(|(t, _)| *t == 0x26), "only the BMT: {p:02X?}");
    let lids: Vec<u8> = p
        .iter()
        .filter(|(_, x)| x.len() == 2 && x[0] == 0x21)
        .map(|(_, x)| x[1])
        .collect();
    assert!(lids.len() > 30);
    // The transponder status first (start of a session), then the schedule.
    assert_eq!(&lids[..7], &[0xA4, 0xA3, 0xA0, 0xA2, 0xA3, 0xA0, 0xA2]);
    let a4 = lids.iter().filter(|l| **l == 0xA4).count();
    let a3 = lids.iter().filter(|l| **l == 0xA3).count();
    assert!(
        a4 >= 2 && a4 * 6 <= a3 + 6,
        "about once per 7 cycles: {a4} vs {a3}"
    );
    let st = last_status(&rig);
    assert_eq!(st.lock, LockState::Unlocked);
    assert_eq!(st.ecu, 0x26);
    rig.assert_wire_tier0_except(&[]);
    rig.car.bmt_journal.assert_no_writes();
    rig.car.bmt_journal.assert_no_violations();
}

#[test]
fn locked_bmt_reports_locked() {
    let mut rig = Rig::new(sirius_ecu(sirius_scenario()), bmt_ecu(bmt_scenario(false)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_for(5_000);
    assert_eq!(last_status(&rig).lock, LockState::Locked);
}

/// BMT `21A0` data byte 2: 0x02 front, 0x04 rear door open (definition position; NOT
/// VERIFIED on the car) reach the Status flags.
#[test]
fn bmt_door_bits_reach_the_status_flags() {
    for (bits, front, rear) in [
        (0u8, false, false),
        (2, true, false),
        (4, false, true),
        (6, true, true),
    ] {
        let scenario = bmt_scenario(true).number("Door bits", f64::from(bits));
        let mut rig = Rig::new(sirius_ecu(sirius_scenario()), bmt_ecu(scenario));
        rig.connect(OWNER, Role::Owner);
        rig.battery(12_600);
        rig.send(OWNER, Op::StartLive(Profile::Parked));
        rig.run_for(5_000);
        let f = last_status(&rig).flags;
        assert_eq!(f & status_flags::DOOR_FRONT_OPEN != 0, front, "bits {bits}");
        assert_eq!(f & status_flags::DOOR_REAR_OPEN != 0, rear, "bits {bits}");
    }
}

#[test]
fn tier0_request_round_trip_for_owner_and_agent() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.connect(AGENT, Role::Agent);
    rig.battery(12_600);
    let r = rig.call(
        AGENT,
        Op::Request {
            ecu: EcuId::Sirius,
            payload: vec![0x21, 0xA9],
        },
        10_000,
    );
    assert_eq!(r.result, ResultCode::Ok);
    assert_eq!(&r.data[..2], &[0x61, 0xA9]);
    let r = rig.call(
        OWNER,
        Op::Request {
            ecu: EcuId::Bmt,
            payload: vec![0x21, 0xA4],
        },
        10_000,
    );
    assert_eq!(
        r.result,
        ResultCode::Ok,
        "transponder status read is Tier 0"
    );
    assert_eq!(&r.data[..2], &[0x61, 0xA4]);
    // Unknown identifier: the ECU's negative reply comes back as data.
    let r = rig.call(
        AGENT,
        Op::Request {
            ecu: EcuId::Bmt,
            payload: vec![0x21, 0x99],
        },
        10_000,
    );
    assert_eq!(r.result, ResultCode::NegativeReply);
    assert_eq!(r.data, vec![0x7F, 0x21, 0x12]);
    rig.assert_wire_tier0_except(&[]);
    rig.car.sirius_journal.assert_no_writes();
    rig.car.bmt_journal.assert_no_writes();
}

#[test]
fn forbidden_and_unapproved_requests_never_reach_the_wire() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.connect(AGENT, Role::Agent);
    rig.battery(12_600);
    let cases: Vec<(u8, Op, ResultCode)> = vec![
        // Tier 1 as a plain request: owner must PREPARE, agent may not at all.
        (
            OWNER,
            Op::Request {
                ecu: EcuId::Sirius,
                payload: vec![0x14, 0x00, 0x00],
            },
            ResultCode::ApprovalRequired,
        ),
        (
            AGENT,
            Op::Request {
                ecu: EcuId::Sirius,
                payload: vec![0x14, 0x00, 0x00],
            },
            ResultCode::NotAllowedForRole,
        ),
        (
            AGENT,
            Op::Prepare {
                ecu: EcuId::Bmt,
                payload: vec![0x31, 0xA0, 0x00, 0x01],
            },
            ResultCode::NotAllowedForRole,
        ),
        // Immobilizer: forbidden for everyone, in every tier.
        (
            OWNER,
            Op::Request {
                ecu: EcuId::Bmt,
                payload: vec![0x21, 0xA4, 0x00],
            },
            ResultCode::Forbidden,
        ),
        (
            OWNER,
            Op::Prepare {
                ecu: EcuId::Bmt,
                payload: vec![0x3B, 0xD2, 0x02, 0x01],
            },
            ResultCode::Forbidden,
        ),
        (
            OWNER,
            Op::Prepare {
                ecu: EcuId::Bmt,
                payload: vec![0x31, 0xC0, 0x00, 0x00],
            },
            ResultCode::Forbidden,
        ),
        (
            OWNER,
            Op::Prepare {
                ecu: EcuId::Sirius,
                payload: vec![0x31, 0xA3, 0x00],
            },
            ResultCode::Forbidden,
        ),
        (
            AGENT,
            Op::Request {
                ecu: EcuId::Sirius,
                payload: vec![0x3B, 0xA0, 0x00],
            },
            ResultCode::Forbidden,
        ),
        // Protocol frames are never requests.
        (
            OWNER,
            Op::Request {
                ecu: EcuId::Sirius,
                payload: vec![0x82],
            },
            ResultCode::Forbidden,
        ),
        // LOCK/UNLOCK: owner only.
        (AGENT, Op::Lock, ResultCode::NotAllowedForRole),
        (AGENT, Op::LwmArm, ResultCode::NotAllowedForRole),
        (
            STRANGER,
            Op::Request {
                ecu: EcuId::Sirius,
                payload: vec![0x21, 0xA9],
            },
            ResultCode::NotAllowedForRole,
        ),
    ];
    for (client, op, want) in cases {
        let r = rig.call(client, op.clone(), 10_000);
        assert_eq!(r.result, want, "{op:?} from client {client}");
    }
    // A Tier 1 PREPARE by the owner needs the approvals feature; without it: Unsupported.
    let r = rig.call(
        OWNER,
        Op::Prepare {
            ecu: EcuId::Sirius,
            payload: vec![0x14, 0x00, 0x00],
        },
        10_000,
    );
    if cfg!(feature = "approvals") {
        assert_eq!(r.result, ResultCode::Challenge);
    } else {
        assert_eq!(r.result, ResultCode::Unsupported);
    }
    rig.run_for(5_000);
    assert!(rig.pin_highs().is_empty());
    rig.assert_wire_tier0_except(&[]);
    rig.car.sirius_journal.assert_no_writes();
    rig.car.bmt_journal.assert_no_writes();
}

#[test]
fn stale_counter_and_bad_frames_are_refused() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    let frame = kline_bridge::gatt::Command {
        counter: 7,
        op: Op::StartLive(Profile::Parked),
    }
    .encode()
    .unwrap();
    rig.send_raw(OWNER, &frame);
    rig.send_raw(OWNER, &frame);
    rig.run_for(10);
    let rs = rig.car.responses_for(OWNER, 7);
    assert_eq!(rs[0].result, ResultCode::Ok);
    assert_eq!(rs[1].result, ResultCode::StaleCounter);
    let mut bad = frame.clone();
    bad[4] ^= 0x01;
    rig.send_raw(OWNER, &bad);
    rig.run_for(10);
    let last = rig.car.responses(OWNER).pop().unwrap();
    assert_eq!((last.counter, last.result), (0, ResultCode::BadFrame));
}

#[test]
fn reconnects_after_the_line_fails() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.run_until(6_000);
    let before = rig.car.channel(Channel::Live).len();
    assert!(before > 0);
    rig.car.cable_ok = false;
    rig.run_until(9_000);
    assert_eq!(last_status(&rig).state, BridgeState::Backoff);
    rig.car.cable_ok = true;
    let during = rig.car.channel(Channel::Live).len();
    rig.run_until(25_000);
    let after = rig.car.channel(Channel::Live).len();
    assert!(
        after > during + 20,
        "live data resumed: {during} -> {after}"
    );
    assert_eq!(last_status(&rig).state, BridgeState::Session);
    rig.assert_wire_tier0_except(&[]);
}

#[test]
fn reconnects_after_the_ecu_stops_answering() {
    // The Sirius session times out on the ECU side (5 s of silence while the cable is down);
    // the bridge must notice and run a new 5-baud init.
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.run_until(5_000);
    rig.car.cable_ok = false;
    rig.run_until(12_000);
    rig.car.cable_ok = true;
    rig.run_until(30_000);
    let inits = rig.car.writes.iter().filter(|w| w.bytes == [0x70]).count();
    assert!(inits >= 2, "{inits} ~KW2 handshakes");
    assert_eq!(last_status(&rig).state, BridgeState::Session);
}

#[test]
fn yield_releases_the_k_line_and_resume_takes_it_back() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.connect(AGENT, Role::Agent);
    rig.battery(14_100);
    rig.run_until(6_000);
    let r = rig.call(AGENT, Op::YieldKline, 2_000);
    assert_eq!(r.result, ResultCode::Ok);
    rig.run_for(1_000);
    let n = rig.car.writes.len();
    let last = rig.car.written_payloads().pop().unwrap();
    assert_eq!(
        last,
        (0x7A, vec![0x82]),
        "StopCommunication before letting go"
    );
    assert!(!rig.car.power, "transceiver off");
    rig.run_for(20_000);
    assert_eq!(rig.car.writes.len(), n, "nothing written while yielded");
    assert_eq!(last_status(&rig).state, BridgeState::Yielded);
    let r = rig.call(
        OWNER,
        Op::Request {
            ecu: EcuId::Sirius,
            payload: vec![0x21, 0xA9],
        },
        1_000,
    );
    assert_eq!(r.result, ResultCode::Yielded);
    rig.call(AGENT, Op::ResumeKline, 1_000);
    rig.run_for(6_000);
    assert!(rig.car.writes.len() > n + 10);
}

#[test]
fn sleeps_below_twelve_volts_with_the_engine_off() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(11_900);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_for(9_000);
    assert!(
        rig.car.sleeps.is_empty(),
        "debounced: not before 10 s below 12.0 V"
    );
    rig.run_for(3_000);
    assert_eq!(
        rig.car.sleeps.first().map(|s| (s.1, s.2)),
        Some((Some(3_600_000), SleepReason::LowBattery))
    );
    assert!(!rig.car.power);
}

/// Review M3: one low sample (cranking dip, ADC noise) while driving never sleeps.
#[test]
fn a_low_sample_while_driving_does_not_sleep() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.run_for(5_000);
    assert!(rig.bridge.session_ecu().is_some());
    rig.battery(11_700);
    rig.run_for(200);
    rig.battery(14_100);
    rig.run_for(20_000);
    assert!(rig.car.sleeps.is_empty(), "{:?}", rig.car.sleeps);
    // Even a long low reading while the engine runs: the engine wins (a measurement fault).
    rig.battery(11_700);
    rig.run_for(30_000);
    assert!(rig.car.sleeps.is_empty(), "{:?}", rig.car.sleeps);
}

#[test]
fn sleeps_when_idle_and_nobody_is_connected() {
    let mut rig = Rig::default_car();
    rig.battery(12_600);
    rig.run_until(119_000);
    assert!(rig.car.sleeps.is_empty());
    rig.run_until(121_000);
    assert_eq!(
        rig.car.sleeps.first().map(|s| (s.1, s.2)),
        Some((Some(60_000), SleepReason::Idle))
    );
    assert!(rig.car.writes.is_empty());
}

#[test]
fn critical_battery_stops_the_session() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_for(5_000);
    assert_eq!(last_status(&rig).state, BridgeState::Session);
    rig.battery(11_700);
    rig.run_for(4_000);
    assert_eq!(
        last_status(&rig).state,
        BridgeState::Session,
        "debounced 5 s"
    );
    rig.run_for(2_000);
    let last = rig.car.written_payloads().pop().unwrap();
    assert_eq!(last, (0x26, vec![0x82]));
    assert!(!rig.car.power);
    assert_eq!(
        rig.car.sleeps.first().map(|s| s.2),
        Some(SleepReason::CriticalBattery)
    );
}

#[test]
fn fob_lock_reads_state_pulses_once_and_verifies() {
    // The BMT reports "locked" from the first 21A3 that comes 200 ms after the bridge's first
    // state read, i.e. after the pulse (the sim does not model the RF remote itself).
    let bmt = bmt_ecu(bmt_scenario(true)).with_change_after(
        &[0x21, 0xA3],
        200,
        "Door lock memory",
        text("locked"),
    );
    let mut rig = Rig::new(sirius_ecu(sirius_scenario()), bmt);
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    rig.run_for(1_000);
    let r = rig.call(OWNER, Op::Lock, 15_000);
    assert_eq!(r.result, ResultCode::Ok);
    let log = rig.pin_log.borrow().clone();
    assert_eq!(log[0], (0, false), "driven low at boot");
    let highs: Vec<_> = log.iter().filter(|(_, h)| *h).collect();
    assert_eq!(highs.len(), 1, "one press: {log:?}");
    let i = log.iter().position(|(_, h)| *h).unwrap();
    assert_eq!(log[i + 1].0 - log[i].0, 300, "300 ms pulse");
    assert!(!log.last().unwrap().1, "ends low");
    rig.assert_wire_tier0_except(&[]);
    rig.car.bmt_journal.assert_no_writes();
}

#[test]
fn fob_does_not_pulse_when_already_in_the_target_state() {
    let mut rig = Rig::new(sirius_ecu(sirius_scenario()), bmt_ecu(bmt_scenario(false)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    let r = rig.call(OWNER, Op::Lock, 15_000);
    assert_eq!(r.result, ResultCode::Ok);
    assert!(rig.pin_highs().is_empty());
}

#[test]
fn fob_reports_when_the_lock_bit_does_not_follow() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    let r = rig.call(OWNER, Op::Lock, 20_000);
    assert_eq!(r.result, ResultCode::LockNotConfirmed);
    assert_eq!(rig.pin_highs().len(), 1, "one pulse, no retry");
    // UNLOCK: the BMT still says unlocked, so nothing to do and no pulse.
    let r = rig.call(OWNER, Op::Unlock, 10_000);
    assert_eq!(r.result, ResultCode::Ok);
    assert_eq!(rig.pin_highs().len(), 1);
}

#[test]
fn lock_commands_are_rate_limited() {
    let mut rig = Rig::new(sirius_ecu(sirius_scenario()), bmt_ecu(bmt_scenario(false)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    let a = rig.send(OWNER, Op::Lock);
    let b = rig.send(OWNER, Op::Unlock);
    rig.run_for(10);
    assert_eq!(
        rig.car.responses_for(OWNER, a)[0].result,
        ResultCode::Accepted
    );
    assert_eq!(
        rig.car.responses_for(OWNER, b)[0].result,
        ResultCode::RateLimited
    );
    // At most 6 in 10 minutes.
    let mut ok = 1;
    for _ in 0..10 {
        rig.run_for(6_000);
        let c = rig.send(OWNER, Op::Lock);
        rig.run_for(10);
        if rig.car.responses_for(OWNER, c)[0].result == ResultCode::Accepted {
            ok += 1;
        }
    }
    assert_eq!(ok, 6);
}

#[test]
fn fob_is_refused_with_the_ignition_on_or_a_low_battery() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    assert_eq!(
        rig.call(OWNER, Op::Lock, 1_000).result,
        ResultCode::IgnitionOn
    );
    rig.battery(11_950);
    assert_eq!(
        rig.call(OWNER, Op::Lock, 1_000).result,
        ResultCode::LowBattery
    );
    rig.run_for(3_000);
    assert!(rig.pin_highs().is_empty());
}

#[test]
fn lwm_arm_needs_the_standing_lock_feature() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    let r = rig.call(OWNER, Op::LwmArm, 15_000);
    if cfg!(feature = "standing-lock") {
        assert_eq!(r.result, ResultCode::Challenge);
    } else {
        assert_eq!(r.result, ResultCode::Unsupported);
    }
}

#[test]
fn runner_feeds_the_watchdog_every_step() {
    let mut rig = Rig::default_car();
    rig.battery(14_100);
    let mut steps = 0;
    for _ in 0..500 {
        if let Stepped::Idle { until_ms } = rig.step() {
            rig.car.wait_until(until_ms);
        }
        steps += 1;
    }
    assert_eq!(rig.car.watchdog_feeds, steps);
}

/// Review M1: the fresh `21 A3` of the LOCK job gets a negative reply; an older positive read
/// said "unlocked". The bridge must not pulse on the stale value.
#[test]
fn fob_never_pulses_on_a_stale_lock_state() {
    let bmt = bmt_ecu(bmt_scenario(true)).with_fault(
        kline_sim::ecu::FaultRule::on(
            &[0x21, 0xA3],
            kline_sim::ecu::ReplyFault::Negative { nrc: 0x22 },
        )
        .after(3),
    );
    let mut rig = Rig::new(sirius_ecu(sirius_scenario()), bmt);
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_for(5_000);
    assert_eq!(
        rig.bridge.status().lock,
        LockState::Unlocked,
        "the early reads"
    );
    rig.send(OWNER, Op::StopLive);
    rig.run_for(1_000);
    let r = rig.call(OWNER, Op::Lock, 20_000);
    assert_eq!(r.result, ResultCode::LockNotConfirmed);
    assert!(rig.pin_highs().is_empty(), "pulsed on a stale state");
}

/// Review M6: the yield is a lease. Only the client that yielded (or an owner) resumes; YIELD
/// is answered Ok only after the session is stopped and the transceiver is off.
#[test]
fn yield_is_a_lease_answered_after_the_line_is_released() {
    let other_agent = 5;
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.connect(AGENT, Role::Agent);
    rig.connect(other_agent, Role::Agent);
    rig.battery(14_100);
    rig.run_for(6_000);
    let c = rig.send(AGENT, Op::YieldKline);
    rig.run_for(10);
    assert_eq!(
        rig.car.responses_for(AGENT, c)[0].result,
        ResultCode::Accepted
    );
    assert!(rig.run_until_pred(2_000, |r| r
        .car
        .responses_for(AGENT, c)
        .iter()
        .any(|x| x.result == ResultCode::Ok)));
    let ok_t = rig
        .car
        .notes
        .iter()
        .rfind(|(_, to, ch, f)| {
            *to == Some(AGENT)
                && *ch == Channel::Response
                && kline_bridge::gatt::Response::decode(f).unwrap().counter == c
        })
        .unwrap()
        .0;
    let off_t = rig.car.power_log.iter().rfind(|(_, on)| !on).unwrap().0;
    assert!(off_t <= ok_t, "Ok at {ok_t} before power off at {off_t}");
    assert_eq!(rig.bridge.yield_holder(), Some(AGENT));
    assert_eq!(
        rig.call(other_agent, Op::ResumeKline, 1_000).result,
        ResultCode::NotAllowedForRole
    );
    assert!(rig.bridge.is_yielded());
    assert_eq!(
        rig.call(OWNER, Op::ResumeKline, 1_000).result,
        ResultCode::Ok
    );
    assert!(!rig.bridge.is_yielded());
}

/// Review M6: after a reset with a yield saved, the bridge stays off the line even with the
/// engine running.
#[test]
fn a_restored_yield_keeps_the_line_free_after_reset() {
    let mut rig = Rig::default_car();
    rig.bridge.restore_yield(AGENT);
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.run_for(30_000);
    assert!(rig.car.writes.is_empty());
    assert!(!rig.car.power);
    rig.connect(AGENT, Role::Agent);
    assert_eq!(
        rig.call(AGENT, Op::ResumeKline, 1_000).result,
        ResultCode::Ok
    );
    rig.run_for(6_000);
    assert!(!rig.car.writes.is_empty());
}

/// Review minor: after a session ended without a clean Stop, the next init waits P3max (5 s)
/// so the old ECU has timed out first.
#[test]
fn an_unclean_session_end_waits_p3max_before_the_next_init() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_for(5_000);
    rig.car.cable_ok = false;
    let t_fail = rig.now();
    rig.run_for(300);
    rig.car.cable_ok = true;
    let n = rig.car.writes.len();
    rig.run_until(t_fail + 4_900);
    assert_eq!(rig.car.writes.len(), n, "an init started before P3max");
    rig.run_for(8_000);
    assert!(rig.car.writes.len() > n);
}

#[test]
fn unbonded_peers_get_no_notifications() {
    let mut rig = Rig::default_car();
    rig.connect(STRANGER, Role::Unbonded);
    rig.battery(12_600);
    rig.run_for(5_000);
    assert!(rig.car.notes.is_empty(), "{:?}", rig.car.notes.len());
    rig.run_until(121_000);
    assert_eq!(rig.car.sleeps.first().map(|s| s.2), Some(SleepReason::Idle));
}

#[test]
fn abs_and_airbag_reads_over_ble() {
    let mut rig = Rig::full_car();
    rig.connect(OWNER, Role::Owner);
    rig.connect(AGENT, Role::Agent);
    rig.battery(12_600);
    let r = rig.call(
        AGENT,
        Op::Request {
            ecu: EcuId::Abs,
            payload: vec![0x58],
        },
        30_000,
    );
    assert_eq!(r.result, ResultCode::Ok, "{r:?}");
    assert_eq!(r.data[0], 0x58, "Bosch 5.3 echoes the SID");
    let r = rig.call(
        AGENT,
        Op::Request {
            ecu: EcuId::Airbag,
            payload: vec![0x21, 0x80],
        },
        30_000,
    );
    assert_eq!(r.result, ResultCode::Ok, "{r:?}");
    assert_eq!(&r.data[..2], &[0x61, 0x80]);
    for (ecu, p) in [
        (EcuId::Abs, vec![0x41, 0x00]),
        (EcuId::Abs, vec![0x71, 0x00]),
        (EcuId::Airbag, vec![0x14, 0xFF, 0x00]),
        (EcuId::Airbag, vec![0x3B, 0xD0, 0x00]),
        (EcuId::Airbag, vec![0x30, 0x80, 0x00]),
    ] {
        let r = rig.call(
            OWNER,
            Op::Prepare {
                ecu,
                payload: p.clone(),
            },
            5_000,
        );
        assert_eq!(r.result, ResultCode::Forbidden, "{ecu:?} {p:02X?}");
    }
    rig.assert_wire_tier0_except(&[]);
    for j in &rig.car.other_journals {
        j.assert_no_writes();
    }
}

/// Review minor (survivor N7): after an unclean end of the BMT session, switching to the
/// Sirius also waits P3max before its init.
#[test]
fn switching_ecu_after_an_unclean_end_waits_p3max() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_for(5_000);
    rig.car.cable_ok = false;
    let t_fail = rig.now();
    rig.run_for(300);
    rig.car.cable_ok = true;
    rig.send(OWNER, Op::StopLive);
    rig.send(
        OWNER,
        Op::Request {
            ecu: EcuId::Sirius,
            payload: vec![0x21, 0xA9],
        },
    );
    let n = rig.car.writes.len();
    rig.run_until(t_fail + 4_900);
    assert_eq!(rig.car.writes.len(), n, "an init started before P3max");
    rig.run_for(8_000);
    assert!(rig.car.written_payloads().iter().any(|(t, _)| *t == 0x7A));
}

/// Owner decision 2026-11-25: the battery estimate comes from the bridge. Unknown until the
/// bridge saw the engine run; after a rest of over 4 h with a settled voltage it is "rested".
#[test]
fn battery_estimate_in_status_needs_a_rest_seen_by_the_bridge() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_450);
    rig.run_for(2_000);
    let st = last_status(&rig);
    assert_eq!(
        st.soc_pct,
        kline_bridge::gatt::SOC_UNKNOWN,
        "rest unknown since boot"
    );
    // The engine runs (charging), then stops; 5 h parked at 12.45 V.
    rig.battery(14_100);
    rig.run_for(5_000);
    let st = last_status(&rig);
    assert_eq!(
        st.soc_pct,
        kline_bridge::gatt::SOC_UNKNOWN,
        "never while charging"
    );
    rig.battery(12_450);
    let start = rig.now();
    let mut t = start;
    while t < start + 5 * 3_600_000 {
        t += 60_000;
        rig.car.wait_until(t);
        rig.battery(12_450);
    }
    rig.run_for(2_000);
    let st = rig.bridge.status();
    assert_eq!(st.soc_pct, 75);
    assert_eq!(st.soc_conf, kline_bridge::soc::Confidence::Rested as u8);
}
