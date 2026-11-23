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
    let write = Op::Prepare {
        ecu: EcuId::Bmt,
        payload: vec![0x3B, 0xD2, 0x00, 0x01],
    };
    // Without 21 80 and 21 A1 in this session: kline-core refuses the challenge.
    assert_eq!(
        rig.call(OWNER, write.clone(), 15_000).result,
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
    let r = rig.call(OWNER, write, 15_000);
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
