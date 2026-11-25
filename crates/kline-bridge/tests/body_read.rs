//! READ_BODY_ONCE and the automatic body read at a standstill (owner decision 2026-11-25):
//! with the engine idling and the car standing, the bridge leaves the Sirius, reads BMT `21A0`
//! and `21A3`, publishes them as Extra frames (ECU 26) and goes back. Reads only, once per stop.

mod common;

use common::*;
use kline_bridge::gate::Role;
use kline_bridge::gatt::{Channel, ExtraFrame, LiveSample, Op, ResultCode};
use kline_bridge::profile::{Profile, BODY_READ_SET};
use kline_spec::Value;

fn bmt_extras(rig: &Rig) -> Vec<ExtraFrame> {
    rig.car
        .channel(Channel::Extra)
        .iter()
        .map(|f| ExtraFrame::decode(f).unwrap())
        .filter(|f| f.ecu == 0x26)
        .collect()
}

fn first_live(rig: &Rig) -> u32 {
    rig.car
        .channel(Channel::Live)
        .first()
        .map(|f| LiveSample::decode(f).unwrap().t_ms)
        .expect("Live")
}

/// The longest pause between two Live samples after `from` (the body read's gap in the engine
/// data), in ms.
fn longest_live_gap(rig: &Rig, from: u32) -> u32 {
    let t: Vec<u32> = rig
        .car
        .channel(Channel::Live)
        .iter()
        .map(|f| LiveSample::decode(f).unwrap().t_ms)
        .filter(|&t| t >= from)
        .collect();
    t.windows(2).map(|w| w[1] - w[0]).max().unwrap_or(0)
}

/// The whole body set in order, each as an Extra frame (ECU 26), then back on the Sirius. The
/// pause in the engine data is measured here: 5.3 s in the sim (BMT 5-baud init about 2.5 s,
/// six reads about 0.3 s, Sirius 5-baud init about 2.5 s); docs/bridge-protocol.md.
#[test]
fn read_body_once_reads_the_whole_body_set_and_returns() {
    let mut rig = Rig::default_car(); // idling, 0 km/h
    rig.connect(AGENT, Role::Agent); // reads only: the agent may ask too
    rig.battery(14_100);
    rig.run_until_pred(10_000, |r| !r.car.channel(Channel::Live).is_empty());
    rig.run_for(500); // a 21A9 too: a fresh speed
    let start = first_live(&rig);
    let r = rig.call(AGENT, Op::ReadBodyOnce, 20_000);
    assert_eq!(r.result, ResultCode::Ok);
    let reqs: Vec<u8> = bmt_extras(&rig).iter().map(|f| f.request[1]).collect();
    assert_eq!(
        reqs,
        BODY_READ_SET.to_vec(),
        "the set in order, no pre-drive read after it"
    );
    assert!(bmt_extras(&rig)
        .iter()
        .all(|f| f.request.len() == 2 && f.request[0] == 0x21));
    rig.run_for(10_000);
    let gap = longest_live_gap(&rig, start);
    assert!((4_000..6_500).contains(&gap), "engine data paused {gap} ms");
    // Once per stop.
    assert_eq!(
        rig.call(AGENT, Op::ReadBodyOnce, 2_000).result,
        ResultCode::RateLimited
    );
    rig.assert_wire_tier0_except(&[]);
    rig.car.bmt_journal.assert_no_writes();
}

/// Moving: refused. A new stop within 30 s of the last read: refused (`RateLimited`), and the
/// automatic read waits for the 30 s too.
#[test]
fn read_body_once_is_refused_while_moving_and_within_thirty_seconds() {
    let sirius = sirius_ecu(sirius_scenario())
        .with_change_after(&[0x21, 0xA9], 10_000, "Vehicle speed", Value::Number(40.0))
        .with_change_after(&[0x21, 0xA9], 18_000, "Vehicle speed", Value::Number(0.0));
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.run_until_pred(10_000, |r| !r.car.channel(Channel::Live).is_empty());
    rig.run_for(500); // a 21A9 too: a fresh speed
    let t0 = rig.now();
    assert_eq!(
        rig.call(OWNER, Op::ReadBodyOnce, 20_000).result,
        ResultCode::Ok
    );
    rig.run_until(t0 + 15_000); // moving
    assert_eq!(
        rig.call(OWNER, Op::ReadBodyOnce, 2_000).result,
        ResultCode::VehicleMoving
    );
    rig.run_until(t0 + 25_000); // stopped again, but within 30 s of the last read
    assert_eq!(
        rig.call(OWNER, Op::ReadBodyOnce, 2_000).result,
        ResultCode::RateLimited
    );
    rig.run_until(t0 + 50_000);
    let a2: Vec<u32> = bmt_extras(&rig)
        .iter()
        .filter(|f| f.request == [0x21, 0xA2])
        .map(|f| f.t_ms)
        .collect();
    assert_eq!(a2.len(), 2, "{a2:?}");
    assert!(a2[1] - a2[0] >= 30_000, "{a2:?}");
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

/// The automatic body read: at every stop over 3 s with the engine running, whatever the
/// doors said at drive-off; once per stop (a long stop gets one read).
#[test]
fn automatic_body_read_at_every_stop() {
    for doors in [4.0, 0.0] {
        let bmt = bmt_ecu(bmt_scenario(true).number("Door bits", doors));
        let mut rig = Rig::new(sirius_ecu(sirius_scenario()), bmt);
        rig.connect(OWNER, Role::Owner);
        rig.battery(12_600);
        rig.send(OWNER, Op::StartLive(Profile::Parked));
        rig.run_for(6_000);
        rig.battery(14_100); // start: pre-drive read, then the Sirius idling at 0 km/h
        rig.run_for(60_000);
        let a2: Vec<u32> = bmt_extras(&rig)
            .iter()
            .filter(|f| f.request == [0x21, 0xA2])
            .map(|f| f.t_ms)
            .collect();
        assert_eq!(a2.len(), 1, "doors {doors}: {a2:?}");
        // Not before the car has stood for 3 s: the first Live sample is the stop's start.
        assert!(a2[0] > first_live(&rig) + 3_000, "{a2:?}");
        rig.assert_wire_tier0_except(&[]);
        rig.car.bmt_journal.assert_no_writes();
    }
}

/// A drive-off seen in the BMT's own speed (`21A2`, the first read): the rest is dropped and the
/// bridge goes back to the Sirius at once.
#[test]
fn a_drive_off_during_the_read_drops_the_rest() {
    let bmt = bmt_ecu(bmt_scenario(true)).with_change_after(
        &[0x21, 0xA2],
        0,
        "Body speed",
        Value::Number(30.0),
    );
    let mut rig = Rig::new(sirius_ecu(sirius_scenario()), bmt);
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.run_until_pred(10_000, |r| !r.car.channel(Channel::Live).is_empty());
    rig.run_for(500); // a 21A9 too: a fresh speed
    let r = rig.call(OWNER, Op::ReadBodyOnce, 20_000);
    assert_eq!(r.result, ResultCode::VehicleMoving);
    let reqs: Vec<u8> = bmt_extras(&rig).iter().map(|f| f.request[1]).collect();
    assert_eq!(
        reqs,
        vec![0xA2],
        "the current request finished, the rest dropped"
    );
    rig.car.bmt_journal.assert_no_writes();
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

fn sirius_a9_count(rig: &Rig, from: u64) -> usize {
    rig.car
        .timed_payloads()
        .iter()
        .filter(|(t, e, x)| *t >= from && *e == 0x7A && x == &[0x21, 0xA9])
        .count()
}

/// Review r4 MAJOR-1: the body read pauses the Sirius for longer than the 3 s rpm window. At a
/// warm idle (13.0 V, below the charging voltage) with the app asking for parked, the bridge must
/// still come back to the Sirius: the pause it made itself does not end "engine running".
#[test]
fn after_a_body_read_at_a_warm_idle_the_bridge_returns_to_the_sirius() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100); // the start
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_until_pred(10_000, |r| !r.car.channel(Channel::Live).is_empty());
    rig.run_for(500);
    rig.battery(13_000); // warm idle: no charging voltage, only the rpm says "running"
    rig.run_for(500);
    assert_eq!(
        rig.call(OWNER, Op::ReadBodyOnce, 20_000).result,
        ResultCode::Ok
    );
    let t = rig.now();
    rig.run_for(20_000);
    let n = sirius_a9_count(&rig, t);
    assert!(
        n > 10,
        "{n} Sirius 21A9 reads in the 20 s after the body read"
    );
}

/// Review r4 minor 1: the engine stops and a new drive starts (the speed never rose above 0):
/// the first stop of the new drive gets its body read, and not at the first 21A9 of the new
/// Sirius session (the standstill restarts with the session).
#[test]
fn the_first_stop_of_a_second_drive_is_read_after_three_seconds() {
    let sirius = sirius_ecu(sirius_scenario())
        .with_change_after(&[0x21, 0xA9], 15_000, "Engine speed", Value::Number(0.0))
        .with_change_after(&[0x21, 0xA9], 44_000, "Engine speed", Value::Number(850.0));
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.run_until_pred(10_000, |r| !r.car.channel(Channel::Live).is_empty());
    let t0 = rig.now();
    rig.run_until(t0 + 14_000);
    rig.battery(12_600); // engine stopping
    rig.run_until(t0 + 45_000);
    rig.battery(10_500); // the second start
    rig.run_for(500);
    rig.battery(14_100);
    let restart = rig.now();
    rig.run_for(25_000);
    let a2: Vec<u64> = rig
        .car
        .timed_payloads()
        .iter()
        .filter(|(_, e, x)| *e == 0x26 && x == &[0x21, 0xA2])
        .map(|(t, _, _)| *t)
        .collect();
    assert_eq!(a2.len(), 2, "one read per drive: {a2:?}");
    let first_a9 = rig
        .car
        .timed_payloads()
        .iter()
        .find(|(t, e, x)| *t >= restart && *e == 0x7A && x == &[0x21, 0xA9])
        .unwrap()
        .0;
    assert!(
        a2[1] > first_a9 + 3_000,
        "not at the first 21A9 of the drive: {} vs {first_a9}",
        a2[1]
    );
}

/// Review r4 minor 2: the car moved while the body read waited behind other jobs (here an
/// agent's Sirius reads: the first makes the car drive off, the second sees it): the read is
/// refused when it would start, no BMT session for it.
#[test]
fn a_body_read_that_waited_while_the_car_drove_off_is_refused() {
    let sirius = sirius_ecu(sirius_scenario()).with_effect(
        &[0x21, 0xA0],
        "Vehicle speed",
        Value::Number(40.0),
    );
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.connect(AGENT, Role::Agent);
    rig.battery(14_100);
    rig.run_until_pred(10_000, |r| !r.car.channel(Channel::Live).is_empty());
    rig.run_for(500);
    rig.send(
        AGENT,
        Op::Request {
            ecu: kline_bridge::profile::EcuId::Sirius,
            payload: vec![0x21, 0xA0],
        },
    );
    rig.send(
        AGENT,
        Op::Request {
            ecu: kline_bridge::profile::EcuId::Sirius,
            payload: vec![0x21, 0xA9],
        },
    );
    let r = rig.call(OWNER, Op::ReadBodyOnce, 10_000);
    assert_eq!(r.result, ResultCode::VehicleMoving);
    assert!(bmt_extras(&rig).is_empty());
}

/// Review r5 MAJOR-A: a yield during a body read. The line goes to the client, so the pause no
/// longer stands for a running engine: at 11.5 V the bridge deep-sleeps (it would never sleep
/// with the engine "running"), and soon, not only after the 15 s bound.
#[test]
fn a_yield_during_a_body_read_lets_the_bridge_sleep_on_a_low_battery() {
    use kline_bridge::power::SleepMode;
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.connect(AGENT, Role::Agent);
    rig.battery(14_100);
    rig.run_until_pred(10_000, |r| !r.car.channel(Channel::Live).is_empty());
    rig.run_for(500);
    rig.send(OWNER, Op::ReadBodyOnce);
    assert!(rig.run_until_pred(10_000, |r| r
        .car
        .timed_payloads()
        .iter()
        .any(|(_, e, x)| *e == 0x26 && x == &[0x21, 0xA2])));
    assert_eq!(
        rig.call(AGENT, Op::YieldKline, 5_000).result,
        ResultCode::Ok
    );
    rig.battery(11_500);
    let t = rig.now();
    rig.run_for(10 * 60_000);
    let s = rig.car.sleeps.first().expect("asleep");
    assert_eq!(s.2.mode(), SleepMode::DeepTimer, "{s:?}");
    assert!(s.0 < t + 10_000, "asleep {} ms after the yield", s.0 - t);
}
