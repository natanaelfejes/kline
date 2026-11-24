//! Tier 1 over BLE: owner PREPARE / CONFIRM against kline-sim (feature `approvals`).
#![cfg(feature = "approvals")]

mod common;

use common::*;
use kline_bridge::gate::Role;
use kline_bridge::gatt::{Channel, Op, RawFrame, Response, ResultCode};
use kline_bridge::profile::EcuId;

/// (challenge id, code) from a Challenge response.
fn challenge(r: &Response) -> (u32, String) {
    assert_eq!(r.result, ResultCode::Challenge, "{r:?}");
    let id = u32::from_le_bytes(r.data[..4].try_into().unwrap());
    assert_eq!(r.data[4], kline_core::safety::Tier::Service as u8);
    (id, String::from_utf8(r.data[5..].to_vec()).unwrap())
}

fn clear_dtc() -> Op {
    Op::Prepare {
        ecu: EcuId::Sirius,
        payload: vec![0x14, 0x00, 0x00],
    }
}

fn rig() -> Rig {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.connect(AGENT, Role::Agent);
    rig.battery(12_600);
    rig.send(AGENT, Op::RawStream(true));
    rig
}

#[test]
fn owner_code_sends_exactly_the_prepared_request_once() {
    let mut rig = rig();
    let (id, code) = challenge(&rig.call(OWNER, clear_dtc(), 15_000));
    assert_eq!(code.len(), 6);
    assert!(rig.car.written_payloads().iter().all(|(_, p)| p[0] != 0x14));
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code: code.clone(),
        },
        10_000,
    );
    assert_eq!(r.result, ResultCode::Ok);
    assert_eq!(r.data, vec![0x54, 0x00, 0x00]);
    let w = rig.car.sirius_journal.writes();
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].payload, vec![0x14, 0x00, 0x00]);
    rig.assert_wire_tier0_except(&[(0x7A, vec![0x14, 0x00, 0x00])]);
    // The same code again: the challenge is gone.
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        5_000,
    );
    assert_eq!(r.result, ResultCode::ApprovalMismatch);
    assert_eq!(rig.car.sirius_journal.writes().len(), 1);
}

#[test]
fn the_code_goes_to_the_owner_only_and_the_agent_cannot_use_it() {
    let mut rig = rig();
    let (id, code) = challenge(&rig.call(OWNER, clear_dtc(), 15_000));
    // Not on any notification the agent can see.
    for (_, to, ch, f) in &rig.car.notes {
        if *to != Some(OWNER) {
            let hay = String::from_utf8_lossy(f);
            assert!(!hay.contains(&code), "code leaked on {ch:?}");
        }
        if *ch == Channel::Raw {
            let raw = RawFrame::decode(f).unwrap();
            assert!(!String::from_utf8_lossy(&raw.bytes).contains(&code));
        }
    }
    let r = rig.call(
        AGENT,
        Op::Confirm {
            challenge: id,
            code: code.clone(),
        },
        5_000,
    );
    assert_eq!(r.result, ResultCode::NotAllowedForRole);
    rig.run_for(3_000);
    rig.car.sirius_journal.assert_no_writes();
    rig.assert_wire_tier0_except(&[]);
}

#[test]
fn a_wrong_code_closes_the_challenge() {
    let mut rig = rig();
    let (id, code) = challenge(&rig.call(OWNER, clear_dtc(), 15_000));
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code: "WRONG1".into(),
        },
        5_000,
    );
    assert_eq!(r.result, ResultCode::ApprovalMismatch);
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        5_000,
    );
    assert_eq!(r.result, ResultCode::ApprovalMismatch, "one try per code");
    rig.run_for(2_000);
    rig.car.sirius_journal.assert_no_writes();
}

#[test]
fn the_session_is_kept_alive_while_the_owner_types() {
    let mut rig = rig();
    let (id, code) = challenge(&rig.call(OWNER, clear_dtc(), 15_000));
    let tp_before = rig
        .car
        .written_payloads()
        .iter()
        .filter(|(_, p)| p == &[0x3E])
        .count();
    rig.run_for(20_000);
    let tp_after = rig
        .car
        .written_payloads()
        .iter()
        .filter(|(_, p)| p == &[0x3E])
        .count();
    assert!(
        tp_after >= tp_before + 5,
        "TesterPresent every 3 s: {tp_before} -> {tp_after}"
    );
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        10_000,
    );
    assert_eq!(
        r.result,
        ResultCode::Ok,
        "same session, the challenge is still bound"
    );
    rig.car.sirius_journal.assert_no_violations();
}

#[test]
fn an_expired_challenge_is_refused() {
    let mut rig = rig();
    let (id, code) = challenge(&rig.call(OWNER, clear_dtc(), 15_000));
    rig.run_for(121_000);
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        5_000,
    );
    assert_eq!(r.result, ResultCode::ApprovalMismatch);
    rig.car.sirius_journal.assert_no_writes();
}

#[test]
fn low_battery_refuses_prepare_and_confirm() {
    let mut rig = rig();
    rig.battery(11_990);
    assert_eq!(
        rig.call(OWNER, clear_dtc(), 5_000).result,
        ResultCode::LowBattery
    );
    rig.battery(12_600);
    let (id, code) = challenge(&rig.call(OWNER, clear_dtc(), 15_000));
    rig.battery(11_990);
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        5_000,
    );
    assert_ne!(r.result, ResultCode::Ok);
    rig.run_for(2_000);
    rig.car.sirius_journal.assert_no_writes();
}

#[test]
fn bmt_actuator_with_code_goes_to_the_bmt() {
    let mut rig = rig();
    let (id, code) = challenge(&rig.call(
        OWNER,
        Op::Prepare {
            ecu: EcuId::Bmt,
            payload: vec![0x31, 0xA0, 0x00, 0x01],
        },
        15_000,
    ));
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        10_000,
    );
    assert_eq!(r.result, ResultCode::Ok);
    assert_eq!(&r.data[..2], &[0x71, 0xA0]);
    let w = rig.car.bmt_journal.writes();
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].payload, vec![0x31, 0xA0, 0x00, 0x01]);
    rig.assert_wire_tier0_except(&[(0x26, vec![0x31, 0xA0, 0x00, 0x01])]);
}

/// Review M2 (surviving mutation): another owner bond cannot answer this owner's challenge.
#[test]
fn only_the_client_that_prepared_may_confirm() {
    let second_owner = 3;
    let mut rig = rig();
    rig.connect(second_owner, Role::Owner);
    let (id, code) = challenge(&rig.call(OWNER, clear_dtc(), 15_000));
    let r = rig.call(
        second_owner,
        Op::Confirm {
            challenge: id,
            code,
        },
        5_000,
    );
    assert_eq!(r.result, ResultCode::ApprovalMismatch);
    rig.run_for(3_000);
    rig.car.sirius_journal.assert_no_writes();
}

/// Owner scope v1 = everything: Tier 2 with the identification and backup reads of the same
/// session, like the CLI.
#[test]
fn tier2_config_write_needs_identification_and_backup_in_the_session() {
    let mut rig = rig();
    let payload = vec![0x3B, 0xD2, 0x00, 0x01];
    // A plain PREPARE of a Tier 2 write needs the identification (review m-1).
    let r = rig.call(
        OWNER,
        Op::Prepare {
            ecu: EcuId::Bmt,
            payload: payload.clone(),
        },
        15_000,
    );
    assert_eq!(r.result, ResultCode::IdentificationRequired);
    let ident = |id: Vec<u8>| Op::PrepareWrite {
        ecu: EcuId::Bmt,
        identification: id,
        payload: payload.clone(),
    };
    // Without 21 80 and 21 A1 in this session: kline-core refuses the challenge.
    assert_eq!(
        rig.call(OWNER, ident(vec![0x61, 0x80]), 15_000).result,
        ResultCode::ApprovalMismatch
    );
    for p in [vec![0x21, 0x80], vec![0x21, 0xA1]] {
        let r = rig.call(
            OWNER,
            Op::Request {
                ecu: EcuId::Bmt,
                payload: p,
            },
            15_000,
        );
        assert_eq!(r.result, ResultCode::Ok);
    }
    let r = rig.call(
        OWNER,
        Op::Request {
            ecu: EcuId::Bmt,
            payload: vec![0x21, 0x80],
        },
        15_000,
    );
    let id80 = r.data.clone();
    // Another identification than the session's latest 21 80 reply: refused by kline-core.
    let mut other = id80.clone();
    *other.last_mut().unwrap() ^= 0xFF;
    assert_eq!(
        rig.call(OWNER, ident(other), 15_000).result,
        ResultCode::ApprovalMismatch
    );
    let r = rig.call(OWNER, ident(id80), 15_000);
    assert_eq!(r.data[4], kline_core::safety::Tier::ConfigWrite as u8);
    let id = u32::from_le_bytes(r.data[..4].try_into().unwrap());
    let code = String::from_utf8(r.data[5..].to_vec()).unwrap();
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        10_000,
    );
    assert_eq!(r.result, ResultCode::Ok, "{r:?}");
    let w = rig.car.bmt_journal.writes();
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].payload, vec![0x3B, 0xD2, 0x00, 0x01]);
}

/// Expert (Tier 3) gets the 10-character code, like the CLI.
#[test]
fn expert_requests_use_the_long_code() {
    let mut rig = rig();
    let r = rig.call(
        OWNER,
        Op::Prepare {
            ecu: EcuId::Sirius,
            payload: vec![0x27, 0x01],
        },
        15_000,
    );
    assert_eq!(r.result, ResultCode::Challenge);
    assert_eq!(r.data[4], kline_core::safety::Tier::Expert as u8);
    assert_eq!(r.data.len() - 5, 10);
    let id = u32::from_le_bytes(r.data[..4].try_into().unwrap());
    let code = String::from_utf8(r.data[5..].to_vec()).unwrap();
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        10_000,
    );
    assert_eq!(
        r.result,
        ResultCode::NegativeReply,
        "the fixture does not know 27"
    );
    assert_eq!(rig.car.sirius_journal.writes()[0].payload, vec![0x27, 0x01]);
}

/// Review minor: no Sirius non-read while the car moves (injector inhibition, fuel pump...).
#[test]
fn non_reads_are_refused_while_moving() {
    use kline_bridge::profile::Profile;
    let sirius = sirius_ecu(sirius_scenario()).with_change_after(
        &[0x21, 0xA9],
        1_000,
        "Vehicle speed",
        kline_spec::Value::Number(40.0),
    );
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.send(OWNER, Op::StartLive(Profile::Drive));
    rig.run_for(8_000);
    for p in [
        vec![0x31, 0xA0, 0x00],
        vec![0x30, 0x80, 0x00, 0x01],
        vec![0x14, 0x00, 0x00],
    ] {
        let r = rig.call(
            OWNER,
            Op::Prepare {
                ecu: EcuId::Sirius,
                payload: p.clone(),
            },
            5_000,
        );
        assert_eq!(r.result, ResultCode::VehicleMoving, "{p:02X?}");
    }
    rig.car.sirius_journal.assert_no_writes();
}

#[test]
fn standing_still_with_the_engine_running_is_allowed() {
    let mut rig = rig();
    rig.battery(14_100);
    rig.run_for(6_000);
    let r = rig.call(OWNER, clear_dtc(), 15_000);
    assert_eq!(r.result, ResultCode::Challenge);
}

/// Review minor (survivor N12): engine running and no fresh speed (21A9 not answered): no
/// Sirius non-read, since nothing proves the car stands; the ABS is not affected.
#[test]
fn sirius_non_reads_need_a_known_standstill_with_the_engine_running() {
    let sirius = sirius_ecu(sirius_scenario()).with_fault(kline_sim::ecu::FaultRule::on(
        &[0x21, 0xA9],
        kline_sim::ecu::ReplyFault::Negative { nrc: 0x12 },
    ));
    let mut rig = Rig::from_car(
        Car::with_others(sirius, bmt_ecu(bmt_scenario(true)), vec![abs_ecu()]),
        Default::default(),
    );
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    rig.send(OWNER, Op::StartLive(kline_bridge::profile::Profile::Drive));
    rig.run_for(6_000);
    let r = rig.call(OWNER, clear_dtc(), 5_000);
    assert_eq!(r.result, ResultCode::VehicleMoving);
}

fn prepare_abs_clear() -> Op {
    Op::Prepare {
        ecu: EcuId::Abs,
        payload: vec![0x45],
    }
}

fn code_of(r: &Response) -> (u32, String) {
    assert_eq!(r.result, ResultCode::Challenge, "{r:?}");
    let id = u32::from_le_bytes(r.data[..4].try_into().unwrap());
    (id, String::from_utf8(r.data[5..].to_vec()).unwrap())
}

/// Sirius parked: engine off, standing.
fn parked_sirius() -> kline_sim::ecu::SpecEcu {
    sirius_ecu(sirius_scenario().number("Engine speed", 0.0))
}

/// Review M-1: prepared parked (engine off), the engine starts and the car drives off while the
/// bridge sits on the ABS (no fresh speed; the battery rises to a charging voltage): CONFIRM
/// is refused, nothing is written.
#[test]
fn r2_abs_approval_confirmed_while_driving() {
    let sirius = parked_sirius()
        .with_change_after(
            &[0x21, 0xA9],
            8_000,
            "Vehicle speed",
            kline_spec::Value::Number(40.0),
        )
        .with_change_after(
            &[0x21, 0xA9],
            8_000,
            "Engine speed",
            kline_spec::Value::Number(2_500.0),
        );
    let car = Car::with_others(sirius, bmt_ecu(bmt_scenario(true)), vec![abs_ecu()]);
    let mut rig = Rig::from_car(car, Default::default());
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    rig.send(OWNER, Op::StartLive(kline_bridge::profile::Profile::Drive));
    rig.run_for(5_000);
    let (id, code) = code_of(&rig.call(OWNER, prepare_abs_clear(), 30_000));
    rig.battery(14_100);
    rig.run_for(6_000);
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        30_000,
    );
    assert_eq!(r.result, ResultCode::VehicleMoving);
    rig.car.other_journals[0].assert_no_writes();
}

/// Review r3 minor 1: with the engine idling at a standstill, a non-Sirius request is refused
/// at PREPARE already (its CONFIRM, on the other ECU without a speed, could never pass).
#[test]
fn r3_engine_idling_refuses_other_ecus_at_prepare() {
    let car = Car::with_others(
        sirius_ecu(sirius_scenario()),
        bmt_ecu(bmt_scenario(true)),
        vec![abs_ecu()],
    );
    let mut rig = Rig::from_car(car, Default::default());
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    rig.send(OWNER, Op::StartLive(kline_bridge::profile::Profile::Drive));
    rig.run_for(5_000);
    // Refused at once, on the command: the bridge never leaves the Sirius for it.
    let c = rig.send(OWNER, prepare_abs_clear());
    assert!(rig.run_until_pred(30_000, |r| !r.car.responses_for(OWNER, c).is_empty()));
    let r = rig.car.responses_for(OWNER, c);
    assert_eq!(
        r[0].result,
        ResultCode::VehicleMoving,
        "not queued first: {r:?}"
    );
    rig.car.other_journals[0].assert_no_writes();
}

/// Review r3 minor 1: parked, engine off, a battery charger holds 13.6 V. A fresh Sirius sample
/// shows 0 rpm and 0 km/h, so BMT and ABS work goes through (prepare and confirm).
#[test]
fn r3_charger_does_not_block_parked_work() {
    let car = Car::with_others(
        parked_sirius(),
        bmt_ecu(bmt_scenario(true)),
        vec![abs_ecu()],
    );
    let mut rig = Rig::from_car(car, Default::default());
    rig.connect(OWNER, Role::Owner);
    rig.battery(13_600);
    rig.run_for(5_000);
    let (id, code) = code_of(&rig.call(
        OWNER,
        Op::Prepare {
            ecu: EcuId::Bmt,
            payload: vec![0x31, 0xA0, 0x00, 0x01],
        },
        15_000,
    ));
    rig.run_for(10_000);
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        15_000,
    );
    assert_eq!(r.result, ResultCode::Ok, "{r:?}");
    rig.run_for(5_000);
    let (id, code) = code_of(&rig.call(OWNER, prepare_abs_clear(), 30_000));
    rig.run_for(10_000);
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        30_000,
    );
    assert_eq!(r.result, ResultCode::Ok, "{r:?}");
    assert_eq!(rig.car.bmt_journal.writes().len(), 1);
    assert_eq!(rig.car.other_journals[0].writes().len(), 1);
}

/// Review r3 minor 1: on a charger, a start's voltage dip after the engine-off sample voids it:
/// CONFIRM is refused.
#[test]
fn r3_a_start_dip_voids_the_engine_off_sample() {
    let car = Car::with_others(
        parked_sirius(),
        bmt_ecu(bmt_scenario(true)),
        vec![abs_ecu()],
    );
    let mut rig = Rig::from_car(car, Default::default());
    rig.connect(OWNER, Role::Owner);
    rig.battery(13_600);
    rig.run_for(5_000);
    let (id, code) = code_of(&rig.call(OWNER, prepare_abs_clear(), 30_000));
    rig.run_for(3_000);
    rig.battery(10_400);
    rig.run_for(200);
    rig.battery(13_600);
    rig.run_for(3_000);
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        30_000,
    );
    assert_eq!(r.result, ResultCode::VehicleMoving);
    rig.car.other_journals[0].assert_no_writes();
}

/// Review r3 minor 1: no fresh speed (the parked profile polls the BMT) and a charger's
/// voltage: the PREPARE first reads `21 A9` from the Sirius, sees 0 rpm and 0 km/h, and goes on.
#[test]
fn r3_a_stale_speed_is_sampled_before_the_prepare() {
    let car = Car::with_others(
        parked_sirius(),
        bmt_ecu(bmt_scenario(true)),
        vec![abs_ecu()],
    );
    let mut rig = Rig::from_car(car, Default::default());
    rig.connect(OWNER, Role::Owner);
    rig.battery(13_600);
    rig.send(OWNER, Op::StartLive(kline_bridge::profile::Profile::Parked));
    rig.run_for(8_000);
    let a9 = |rig: &Rig| {
        rig.car
            .written_payloads()
            .iter()
            .filter(|(t, p)| *t == EcuId::Sirius.address() && p[..] == [0x21, 0xA9])
            .count()
    };
    assert_eq!(a9(&rig), 0, "the parked profile does not poll the Sirius");
    let (id, code) = code_of(&rig.call(OWNER, prepare_abs_clear(), 30_000));
    assert!(a9(&rig) > 0, "sampled");
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        30_000,
    );
    assert_eq!(r.result, ResultCode::Ok, "{r:?}");
}

/// Review r3 minor 4 (mutant challenge_ttl_ms -> silence_ms): the engine was running 15 s ago
/// at 12.6 V (no charging voltage) and the Sirius stopped answering: no ABS non-read.
#[test]
fn r3_a_running_engine_is_remembered_for_the_challenge_lifetime() {
    let sirius = sirius_ecu(sirius_scenario()).with_fault(
        kline_sim::ecu::FaultRule::on(&[0x21], kline_sim::ecu::ReplyFault::NoReply).after(30),
    );
    let car = Car::with_others(sirius, bmt_ecu(bmt_scenario(true)), vec![abs_ecu()]);
    let mut rig = Rig::from_car(car, Default::default());
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    rig.send(OWNER, Op::StartLive(kline_bridge::profile::Profile::Drive));
    rig.run_for(20_000);
    rig.send(OWNER, Op::StopLive);
    rig.run_for(2_000);
    assert_eq!(
        rig.call(OWNER, prepare_abs_clear(), 60_000).result,
        ResultCode::VehicleMoving
    );
    rig.car.other_journals[0].assert_no_writes();
}

/// Review M-1: engine running (14.1 V) and no speed from the Sirius: the ABS is refused too.
#[test]
fn r2_abs_non_read_with_engine_running_and_unknown_speed() {
    let sirius = sirius_ecu(sirius_scenario()).with_fault(kline_sim::ecu::FaultRule::on(
        &[0x21, 0xA9],
        kline_sim::ecu::ReplyFault::Negative { nrc: 0x12 },
    ));
    let car = Car::with_others(sirius, bmt_ecu(bmt_scenario(true)), vec![abs_ecu()]);
    let mut rig = Rig::from_car(car, Default::default());
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.send(OWNER, Op::StartLive(kline_bridge::profile::Profile::Drive));
    rig.run_for(6_000);
    assert_eq!(
        rig.call(OWNER, prepare_abs_clear(), 30_000).result,
        ResultCode::VehicleMoving
    );
}

/// Review m-2: parked, the ABS clear goes through: the ABS session survives its 7000 ms
/// keep-alive while the owner types (as on the car).
#[test]
fn r2_abs_clear_parked_works() {
    let car = Car::with_others(
        sirius_ecu(sirius_scenario()),
        bmt_ecu(bmt_scenario(true)),
        vec![abs_ecu()],
    );
    let mut rig = Rig::from_car(car, Default::default());
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    let (id, code) = code_of(&rig.call(OWNER, prepare_abs_clear(), 30_000));
    rig.run_for(20_000);
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        30_000,
    );
    assert_eq!(r.result, ResultCode::Ok, "{r:?}");
    assert_eq!(rig.car.other_journals[0].writes().len(), 1);
    rig.car.other_journals[0].assert_no_violations();
}

/// Review m-2: the session the challenge was made on ends; CONFIRM never writes into the new
/// session.
#[test]
fn a_challenge_dies_with_its_session() {
    let mut rig = rig();
    let (id, code) = code_of(&rig.call(OWNER, clear_dtc(), 15_000));
    rig.car.cable_ok = false;
    rig.run_for(500);
    rig.car.cable_ok = true;
    rig.run_for(10_000);
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        15_000,
    );
    assert_eq!(r.result, ResultCode::SessionLost);
    rig.run_for(3_000);
    rig.car.sirius_journal.assert_no_writes();
}

/// Review r3 minor 1: the motion rule is checked again when the PREPARE job runs on the ECU:
/// the engine starts (charging voltage) while the bridge initialises the ABS.
#[test]
fn r3_prepare_rechecks_motion_when_its_job_runs() {
    let car = Car::with_others(
        parked_sirius(),
        bmt_ecu(bmt_scenario(true)),
        vec![abs_ecu()],
    );
    let mut rig = Rig::from_car(car, Default::default());
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    rig.send(OWNER, Op::StartLive(kline_bridge::profile::Profile::Drive));
    rig.run_for(5_000);
    let c = rig.send(OWNER, prepare_abs_clear());
    rig.run_for(50);
    rig.battery(14_100);
    assert!(rig.run_until_pred(30_000, |r| r
        .car
        .responses_for(OWNER, c)
        .iter()
        .any(|x| x.result != ResultCode::Accepted)));
    let r = rig.car.responses_for(OWNER, c).pop().unwrap();
    assert_eq!(r.result, ResultCode::VehicleMoving, "{r:?}");
}

/// Review M4-1 (reviewer's break test): the engine stalled with the voltage still high, the
/// owner PREPAREs on the ABS (fresh 0 rpm, 0 km/h), the engine restarts without a dip being
/// seen and the car drives off. The alternator's rise over the voltage of the engine-off sample
/// voids it: CONFIRM is refused.
#[test]
fn rev4_stall_restart_without_dip_abs_confirm() {
    let sirius = parked_sirius()
        .with_change_after(
            &[0x21, 0xA9],
            20_000,
            "Vehicle speed",
            kline_spec::Value::Number(50.0),
        )
        .with_change_after(
            &[0x21, 0xA9],
            20_000,
            "Engine speed",
            kline_spec::Value::Number(2_500.0),
        );
    let car = Car::with_others(sirius, bmt_ecu(bmt_scenario(true)), vec![abs_ecu()]);
    let mut rig = Rig::from_car(car, Default::default());
    rig.connect(OWNER, Role::Owner);
    rig.battery(13_600);
    rig.send(OWNER, Op::StartLive(kline_bridge::profile::Profile::Drive));
    rig.run_for(3_000);
    let (id, code) = code_of(&rig.call(OWNER, prepare_abs_clear(), 15_000));
    rig.battery(14_100);
    rig.run_for(30_000);
    let r = rig.call(
        OWNER,
        Op::Confirm {
            challenge: id,
            code,
        },
        10_000,
    );
    assert_eq!(r.result, ResultCode::VehicleMoving, "{r:?}");
    rig.car.other_journals[0].assert_no_writes();
}

/// On a charger, a Sirius that stopped answering `21 A9` and an ABS PREPARE: refused when the
/// last engine-off sample does not count (`VehicleMoving`), granted when it does.
fn abs_prepare_with(sirius: kline_sim::ecu::SpecEcu, run_ms: u64) -> ResultCode {
    let car = Car::with_others(sirius, bmt_ecu(bmt_scenario(true)), vec![abs_ecu()]);
    let mut rig = Rig::from_car(car, Default::default());
    rig.connect(OWNER, Role::Owner);
    rig.battery(13_600);
    rig.send(OWNER, Op::StartLive(kline_bridge::profile::Profile::Drive));
    rig.run_for(run_ms);
    rig.call(OWNER, prepare_abs_clear(), 60_000).result
}

fn a9_refused_after(n: u32) -> kline_sim::ecu::FaultRule {
    kline_sim::ecu::FaultRule::on(
        &[0x21, 0xA9],
        kline_sim::ecu::ReplyFault::Negative { nrc: 0x12 },
    )
    .after(n)
}

/// Review M4-2: an `21 AA` frame with rpm above 0 after the engine-off `21 A9` voids it.
#[test]
fn r4_aa_rpm_voids_the_engine_off_sample() {
    // Control: without the rpm change the sample counts.
    assert_eq!(
        abs_prepare_with(parked_sirius().with_fault(a9_refused_after(5)), 10_000),
        ResultCode::Challenge
    );
    let sirius = parked_sirius()
        .with_fault(a9_refused_after(5))
        .with_change_after(
            &[0x21, 0xAA],
            3_000,
            "Engine speed",
            kline_spec::Value::Number(800.0),
        );
    assert_eq!(abs_prepare_with(sirius, 10_000), ResultCode::VehicleMoving);
}

/// Review M4-2: an `21 A9` with rpm above 0, or with a speed above 0, voids the engine-off
/// sample (also once it is no longer fresh).
#[test]
fn r4_a9_rpm_or_speed_voids_the_engine_off_sample() {
    for (data, v) in [("Engine speed", 800.0), ("Vehicle speed", 10.0)] {
        // The changed value is sent by a few 21 A9 replies, then 21 A9 is refused.
        let sirius = parked_sirius()
            .with_change_after(&[0x21, 0xA9], 1_000, data, kline_spec::Value::Number(v))
            .with_fault(a9_refused_after(12));
        assert_eq!(
            abs_prepare_with(sirius, 10_000),
            ResultCode::VehicleMoving,
            "{data}"
        );
    }
}

/// Review M4-2: the engine-off sample counts for the challenge lifetime (120 s) only.
#[test]
fn r4_the_engine_off_sample_expires() {
    assert_eq!(
        abs_prepare_with(parked_sirius().with_fault(a9_refused_after(5)), 100_000),
        ResultCode::Challenge
    );
    assert_eq!(
        abs_prepare_with(parked_sirius().with_fault(a9_refused_after(5)), 130_000),
        ResultCode::VehicleMoving
    );
}

/// Review M4-2: when the Sirius sample taken for a PREPARE shows the car moving, the PREPARE
/// is refused there, on the Sirius, without opening the target ECU.
#[test]
fn r4_a_moving_sample_refuses_before_the_target_ecu() {
    let sirius = sirius_ecu(sirius_scenario().number("Vehicle speed", 40.0));
    let car = Car::with_others(sirius, bmt_ecu(bmt_scenario(true)), vec![abs_ecu()]);
    let mut rig = Rig::from_car(car, Default::default());
    rig.connect(OWNER, Role::Owner);
    rig.battery(13_600);
    rig.send(OWNER, Op::StartLive(kline_bridge::profile::Profile::Parked));
    rig.run_for(8_000);
    let c = rig.send(OWNER, prepare_abs_clear());
    assert!(rig.run_until_pred(30_000, |r| r
        .car
        .responses_for(OWNER, c)
        .iter()
        .any(|x| x.result != ResultCode::Accepted)));
    let r = rig.car.responses_for(OWNER, c).pop().unwrap();
    assert_eq!(r.result, ResultCode::VehicleMoving);
    assert_eq!(
        rig.bridge.session_ecu(),
        Some(EcuId::Sirius),
        "refused on the Sirius"
    );
}
