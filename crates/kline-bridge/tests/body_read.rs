//! READ_BODY_ONCE and the automatic body read at a standstill (owner decision 2026-11-25):
//! with the engine idling and the car standing, the bridge leaves the Sirius, reads BMT `21A0`
//! and `21A3`, publishes them as Extra frames (ECU 26) and goes back. Reads only, once per stop.

mod common;

use common::*;
use kline_bridge::gate::Role;
use kline_bridge::gatt::{Channel, ExtraFrame, LiveSample, Op, ResultCode};
use kline_bridge::profile::Profile;
use kline_spec::Value;

fn bmt_extras(rig: &Rig) -> Vec<ExtraFrame> {
    rig.car
        .channel(Channel::Extra)
        .iter()
        .map(|f| ExtraFrame::decode(f).unwrap())
        .filter(|f| f.ecu == 0x26)
        .collect()
}

#[test]
fn read_body_once_at_a_standstill_reads_doors_and_lock_and_returns() {
    let mut rig = Rig::default_car(); // idling, 0 km/h
    rig.connect(AGENT, Role::Agent); // reads only: the agent may ask too
    rig.battery(14_100);
    rig.run_until(8_000);
    let before = bmt_extras(&rig).len();
    let r = rig.call(AGENT, Op::ReadBodyOnce, 20_000);
    assert_eq!(r.result, ResultCode::Ok);
    let x = &bmt_extras(&rig)[before..];
    let reqs: Vec<Vec<u8>> = x.iter().map(|f| f.request.clone()).collect();
    assert_eq!(reqs, vec![vec![0x21, 0xA0], vec![0x21, 0xA3]]);
    // Back on the Sirius: Live again within a few seconds.
    let done = rig.now();
    rig.run_for(10_000);
    let next = rig
        .car
        .channel(Channel::Live)
        .iter()
        .map(|f| u64::from(LiveSample::decode(f).unwrap().t_ms))
        .find(|&t| t > done)
        .expect("Live again");
    assert!(
        next < done + 6_000,
        "Sirius back {} ms after the read",
        next - done
    );
    // Once per stop.
    assert_eq!(
        rig.call(AGENT, Op::ReadBodyOnce, 2_000).result,
        ResultCode::RateLimited
    );
    rig.assert_wire_tier0_except(&[]);
    rig.car.bmt_journal.assert_no_writes();
}

#[test]
fn read_body_once_is_refused_while_moving_and_allowed_again_at_the_next_stop() {
    let sirius = sirius_ecu(sirius_scenario())
        .with_change_after(&[0x21, 0xA9], 6_000, "Vehicle speed", Value::Number(40.0))
        .with_change_after(&[0x21, 0xA9], 16_000, "Vehicle speed", Value::Number(0.0));
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.run_until(5_000);
    assert_eq!(
        rig.call(OWNER, Op::ReadBodyOnce, 20_000).result,
        ResultCode::Ok
    );
    rig.run_until(13_000); // moving
    assert_eq!(
        rig.call(OWNER, Op::ReadBodyOnce, 2_000).result,
        ResultCode::VehicleMoving
    );
    rig.run_until(24_000); // stopped again: a new stop
    assert_eq!(
        rig.call(OWNER, Op::ReadBodyOnce, 20_000).result,
        ResultCode::Ok
    );
    assert_eq!(
        bmt_extras(&rig)
            .iter()
            .filter(|f| f.request == [0x21, 0xA3])
            .count(),
        2
    );
}

#[test]
fn read_body_once_with_the_engine_off_answers_at_once() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    assert_eq!(
        rig.call(OWNER, Op::StartLive(Profile::Parked), 1_000)
            .result,
        ResultCode::Ok
    );
    rig.run_for(5_000);
    let n = rig.car.written_payloads().len();
    let r = rig.call(OWNER, Op::ReadBodyOnce, 1_000);
    assert_eq!(
        r.result,
        ResultCode::Ok,
        "the parked profile reads the body anyway"
    );
    let all = rig.car.responses(OWNER);
    assert_ne!(all[all.len() - 1].result, ResultCode::Accepted);
    assert!(
        !all.iter()
            .any(|x| x.counter == r.counter && x.result == ResultCode::Accepted),
        "answered at once, nothing queued"
    );
    assert!(rig.car.written_payloads()[n..]
        .iter()
        .all(|(t, _)| *t == 0x26));
}

#[test]
fn unbonded_peers_cannot_ask() {
    let mut rig = Rig::default_car();
    rig.battery(14_100);
    rig.run_until(5_000);
    assert_eq!(
        rig.call(STRANGER, Op::ReadBodyOnce, 1_000).result,
        ResultCode::NotAllowedForRole
    );
}

/// The automatic body read: the pre-drive read said a door was open, the car stands for more
/// than 3 s with the engine running: one read in that stop, none without an open door.
#[test]
fn automatic_body_read_when_the_doors_were_open_at_drive_off() {
    for (doors, want) in [(4.0, 1usize), (0.0, 0)] {
        let bmt = bmt_ecu(bmt_scenario(true).number("Door bits", doors));
        let mut rig = Rig::new(sirius_ecu(sirius_scenario()), bmt);
        rig.connect(OWNER, Role::Owner);
        rig.battery(12_600);
        rig.send(OWNER, Op::StartLive(Profile::Parked));
        rig.run_for(6_000);
        rig.battery(14_100); // start: pre-drive read, then the Sirius idling at 0 km/h
        rig.run_for(40_000);
        let auto = bmt_extras(&rig)
            .iter()
            .filter(|f| f.request == [0x21, 0xA3])
            .count();
        assert_eq!(auto, want, "doors {doors}");
        if want == 1 {
            // Not before the car has stood for 3 s: the first Live sample is the stop's start.
            let first_live = rig
                .car
                .channel(Channel::Live)
                .iter()
                .map(|f| LiveSample::decode(f).unwrap().t_ms)
                .next()
                .unwrap();
            let a0 = bmt_extras(&rig)
                .iter()
                .filter(|f| f.request == [0x21, 0xA0])
                .map(|f| f.t_ms)
                .nth(1)
                .expect("the automatic door read");
            assert!(a0 > first_live + 3_000, "{a0} vs {first_live}");
        }
        rig.assert_wire_tier0_except(&[]);
    }
}

/// No fresh speed (the Sirius does not answer, the engine may run): refused, the car may move.
#[test]
fn read_body_once_without_a_fresh_speed_is_refused() {
    use kline_sim::ecu::InitFault;
    let sirius = sirius_ecu(sirius_scenario()).with_init_fault(InitFault::Silent);
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(10_500); // a start
    rig.run_for(500);
    rig.battery(14_100);
    rig.run_for(5_000);
    assert_eq!(
        rig.call(OWNER, Op::ReadBodyOnce, 2_000).result,
        ResultCode::VehicleMoving
    );
}
