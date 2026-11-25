//! Profile selection (owner decision 2026-11-25): the bridge itself runs the drive profile
//! whenever the engine may run, whatever the app requested, and goes back to the requested
//! parked profile after the engine stops; the BMT transponder status is published once per
//! ignition-on; the doors are read once more right before the switch to the drive profile.

mod common;

use common::*;
use kline_bridge::gate::Role;
use kline_bridge::gatt::{Channel, ExtraFrame, LiveSample, Op, ResultCode};
use kline_bridge::profile::{select, Profile};
use kline_spec::Value;

/// The table both implementations follow: kline-bridge `profile::select` and the app's
/// `ProfileSelection.select` (ClioBridgeProtocolTests `ProfileSelectionTests`, same rows).
pub const SELECTION: [(Profile, bool, bool, Profile); 9] = [
    // (requested, engine may run, lock-when-moving holds the Sirius) -> polled
    (Profile::Parked, false, false, Profile::Parked),
    (Profile::Parked, true, false, Profile::Drive),
    (Profile::Off, false, false, Profile::Off),
    (Profile::Off, true, false, Profile::Drive),
    (Profile::Drive, false, false, Profile::Drive),
    (Profile::Parked, false, true, Profile::Drive),
    (Profile::Off, false, true, Profile::Drive),
    (Profile::Diagnostics, false, false, Profile::Diagnostics),
    (Profile::Diagnostics, true, false, Profile::Drive),
];

#[test]
fn selection_table() {
    for (req, may_run, lwm, want) in SELECTION {
        assert_eq!(select(req, may_run, lwm), want, "{req:?} {may_run} {lwm}");
    }
}

fn sirius_lids_since(rig: &Rig, from: usize) -> Vec<(u8, Vec<u8>)> {
    rig.car.written_payloads()[from..].to_vec()
}

/// The blocker of the review: the app asks for parked at start-up; with the engine running the
/// bridge must stream the drive profile anyway, and go back to the BMT after the engine stops.
#[test]
fn parked_request_still_streams_the_drive_profile_while_the_engine_runs() {
    let sirius = sirius_ecu(sirius_scenario()).with_change_after(
        &[0x21, 0xA9],
        20_000,
        "Engine speed",
        Value::Number(0.0),
    );
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    assert_eq!(
        rig.call(OWNER, Op::StartLive(Profile::Parked), 1_000)
            .result,
        ResultCode::Ok
    );
    rig.run_for(15_000);
    let live: Vec<LiveSample> = rig
        .car
        .channel(Channel::Live)
        .iter()
        .map(|f| LiveSample::decode(f).unwrap())
        .collect();
    assert!(
        live.len() > 50,
        "engine data while the engine runs: {}",
        live.len()
    );
    assert_eq!(live.last().unwrap().rpm, 850);
    // The engine stops (rpm 0, the voltage falls): back to the requested parked profile.
    rig.run_for(10_000);
    rig.battery(12_600);
    let n = rig.car.written_payloads().len();
    rig.run_for(15_000);
    let after = sirius_lids_since(&rig, n);
    assert!(
        after.iter().rev().take(10).all(|(t, _)| *t == 0x26),
        "parked again: {after:02X?}"
    );
    rig.assert_wire_tier0_except(&[]);
}

/// A charger holds 13.6 V with the ignition off: the Sirius does not answer, so the bridge
/// polls the BMT (the requested parked profile) instead of retrying the Sirius forever.
#[test]
fn a_charger_with_the_ignition_off_keeps_the_parked_profile() {
    let sirius = sirius_ecu(sirius_scenario()).with_fault(kline_sim::ecu::FaultRule {
        on: None,
        ..kline_sim::ecu::FaultRule::on(&[], kline_sim::ecu::ReplyFault::NoReply)
    });
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(13_600);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_for(40_000);
    let p = rig.car.written_payloads();
    let bmt_reads = p.iter().filter(|(t, x)| *t == 0x26 && x.len() == 2).count();
    assert!(bmt_reads > 20, "{bmt_reads} BMT reads");
}

/// Owner addition (c): the doors are read once more right before the BMT session ends for
/// the drive profile, and published as an Extra frame.
#[test]
fn doors_are_read_once_more_before_the_drive_profile() {
    let scenario = bmt_scenario(true).number("Door bits", 4.0);
    let mut rig = Rig::new(sirius_ecu(sirius_scenario()), bmt_ecu(scenario));
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_for(6_000);
    let before = rig.car.channel(Channel::Extra).len();
    rig.battery(14_100); // the engine started
    rig.run_for(10_000);
    let p = rig.car.written_payloads();
    let stop = p
        .iter()
        .position(|(t, x)| *t == 0x26 && x == &[0x82])
        .expect("the BMT session was stopped");
    assert_eq!(
        p[stop - 1],
        (0x26, vec![0x21, 0xA0]),
        "door read right before the stop"
    );
    let x: Vec<ExtraFrame> = rig.car.channel(Channel::Extra)[before..]
        .iter()
        .map(|f| ExtraFrame::decode(f).unwrap())
        .collect();
    let doors: Vec<&ExtraFrame> = x
        .iter()
        .filter(|f| f.ecu == 0x26 && f.request == [0x21, 0xA0])
        .collect();
    assert_eq!(doors.len(), 1);
    assert_eq!(
        doors[0].reply[4] & 0x04,
        0x04,
        "rear door open in data byte 2"
    );
    assert!(
        p[stop..]
            .iter()
            .any(|(t, x)| *t == 0x7A && x == &[0x21, 0xA9]),
        "then the Sirius"
    );
    rig.assert_wire_tier0_except(&[]);
}

/// Owner addition (b): the transponder status (BMT 21A4) is published once per ignition-on.
#[test]
fn transponder_status_is_published_once_per_ignition_on() {
    for (ignition, want) in [(0.0, 0usize), (1.0, 1)] {
        let scenario = bmt_scenario(true).number("Ignition bits", ignition);
        let mut rig = Rig::new(sirius_ecu(sirius_scenario()), bmt_ecu(scenario));
        rig.connect(OWNER, Role::Owner);
        rig.battery(12_600);
        rig.send(OWNER, Op::StartLive(Profile::Parked));
        rig.run_for(30_000);
        let reads = rig
            .car
            .written_payloads()
            .iter()
            .filter(|(t, x)| *t == 0x26 && x == &[0x21, 0xA4])
            .count();
        assert!(reads >= 3, "read every few cycles: {reads}");
        let published = rig
            .car
            .channel(Channel::Extra)
            .iter()
            .map(|f| ExtraFrame::decode(f).unwrap())
            .filter(|f| f.ecu == 0x26 && f.request == [0x21, 0xA4])
            .count();
        assert_eq!(published, want, "ignition {ignition}");
    }
    // Ignition off, then on: published at the edge.
    let bmt = bmt_ecu(bmt_scenario(true)).with_change_after(
        &[0x21, 0xA4],
        10_000,
        "Ignition bits",
        Value::Number(1.0),
    );
    let mut rig = Rig::new(sirius_ecu(sirius_scenario()), bmt);
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_for(30_000);
    let published: Vec<ExtraFrame> = rig
        .car
        .channel(Channel::Extra)
        .iter()
        .map(|f| ExtraFrame::decode(f).unwrap())
        .filter(|f| f.request == [0x21, 0xA4])
        .collect();
    assert_eq!(published.len(), 1);
    assert!(published[0].t_ms >= 10_000);
}
