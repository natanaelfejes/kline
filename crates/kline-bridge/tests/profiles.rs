//! Profile selection (owner decision 2026-11-25): the bridge itself runs the drive profile
//! whenever the engine may run, whatever the app requested, and goes back to the requested
//! parked profile after the engine stops; the BMT transponder status is published once per
//! ignition-on; the doors are read once more right before the switch to the drive profile.

mod common;

use common::*;
use kline_bridge::gate::Role;
use kline_bridge::gatt::{BridgeState, Channel, ExtraFrame, LiveSample, Op, ResultCode};
use kline_bridge::profile::{select, Profile};
use kline_sim::ecu::{FaultRule, InitFault, ReplyFault};
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

/// Sirius init attempts seen in `ms` (transitions into Status state Init on 0x7A).
fn sirius_inits(rig: &mut Rig, ms: u64) -> usize {
    let mut n = 0;
    let mut was = false;
    rig.run_until_pred(ms, |r| {
        let st = r.bridge.status();
        let init = st.state == BridgeState::Init && st.ecu == 0x7A;
        if init && !was {
            n += 1;
        }
        was = init;
        false
    });
    n
}

fn first_live_after(rig: &Rig, t: u64) -> Option<u64> {
    rig.car
        .channel(Channel::Live)
        .iter()
        .map(|f| LiveSample::decode(f).unwrap())
        .map(|s| u64::from(s.t_ms))
        .find(|&x| x >= t)
}

/// A charger holds 13.6 V with the ignition off: two Sirius inits get no answer, so the bridge
/// polls the BMT (the requested parked profile) instead of retrying the Sirius forever.
#[test]
fn a_charger_with_the_ignition_off_keeps_the_parked_profile() {
    let sirius = sirius_ecu(sirius_scenario()).with_init_fault(InitFault::Silent);
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(13_600);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    let inits = sirius_inits(&mut rig, 60_000);
    let p = rig.car.written_payloads();
    let bmt_reads = p.iter().filter(|(t, x)| *t == 0x26 && x.len() == 2).count();
    assert!(bmt_reads > 20, "{bmt_reads} BMT reads");
    assert!((2..=4).contains(&inits), "{inits} Sirius inits in 60 s");
}

/// Review r2 MAJOR 1: an engine that runs (the bridge booted with it running: no dip seen)
/// and a Sirius that misses answers for a few seconds must not park the bridge for 120 s.
#[test]
fn a_running_engine_with_missed_answers_stays_on_the_drive_profile() {
    let sirius = sirius_ecu(sirius_scenario()).with_fault(
        FaultRule {
            times: Some(6),
            ..FaultRule::on(&[0x21], ReplyFault::NoReply)
        }
        .after(40),
    );
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(14_100);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_for(5_000);
    let lost = rig.now();
    rig.run_for(60_000);
    let t = first_live_after(&rig, lost + 1_000).expect("Live again");
    assert!(
        t < lost + 20_000,
        "first Live {} ms after the misses",
        t - lost
    );
}

/// Review r2 MAJOR 1: the silent proof needs two inits in a row without an answer, and a
/// charging voltage that was there before the last dip. Here the engine starts (a dip), and
/// the Sirius misses its first two inits (it boots slowly): no 120 s park.
#[test]
fn a_start_dip_voids_the_charger_proof() {
    let sirius = sirius_ecu(sirius_scenario()).with_init_fault(InitFault::Silent);
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(13_600); // a charger
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_for(30_000); // silent proof, parked
    rig.battery(10_500); // cranking
    rig.run_for(500);
    rig.battery(13_600); // the same voltage again: only the dip says "a start"
    let inits = sirius_inits(&mut rig, 20_000);
    assert!(
        inits >= 2,
        "the dip voids the proof: {inits} Sirius inits within 20 s"
    );
}

/// A rise over the charger's voltage (an alternator on top, the start dip unseen) voids the
/// "no answer" proof too: the bridge looks at the Sirius again.
#[test]
fn a_rise_over_the_charger_voids_the_charger_proof() {
    let sirius = sirius_ecu(sirius_scenario()).with_init_fault(InitFault::Silent);
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(13_600);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_for(30_000); // silent proof, parked
    rig.battery(14_100); // +500 mV, no dip seen
    let inits = sirius_inits(&mut rig, 20_000);
    assert!(
        inits >= 2,
        "the rise voids the proof: {inits} Sirius inits within 20 s"
    );
}

/// Review r2 MAJOR 1: the BMT says the ignition is on (21A4 bit): the Sirius should answer,
/// so the silent proof ends and the bridge looks at the Sirius again at once.
#[test]
fn the_ignition_bit_ends_the_silent_proof() {
    let count = |ignition: f64| {
        let sirius = sirius_ecu(sirius_scenario()).with_init_fault(InitFault::Silent);
        let bmt = bmt_ecu(bmt_scenario(true).number("Ignition bits", ignition));
        let mut rig = Rig::new(sirius, bmt);
        rig.connect(OWNER, Role::Owner);
        rig.battery(13_600);
        rig.send(OWNER, Op::StartLive(Profile::Parked));
        sirius_inits(&mut rig, 90_000)
    };
    let off = count(0.0);
    let on = count(1.0);
    assert!(off <= 4, "ignition off: {off}");
    assert!(on > off + 4, "ignition on: {on} vs {off}");
}

/// Mutant `charging_then`: an engine-off sample taken below the charging voltage proves
/// nothing once the voltage reads charging (a small rise, under the 300 mV rise limit): the
/// bridge looks at the Sirius again.
#[test]
fn an_engine_off_proof_taken_before_charging_does_not_park() {
    let sirius = sirius_ecu(sirius_scenario().number("Engine speed", 0.0));
    let mut rig = Rig::new(sirius, bmt_ecu(bmt_scenario(true)));
    rig.connect(OWNER, Role::Owner);
    rig.battery(13_000);
    rig.send(OWNER, Op::StartLive(Profile::Drive));
    rig.run_for(8_000); // 21A9 with 0 rpm at 13.0 V (not charging)
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_for(8_000); // parked
    let n = rig.car.written_payloads().len();
    rig.battery(13_250); // now charging, +250 mV
    rig.run_for(10_000);
    let after = &rig.car.written_payloads()[n..];
    assert!(
        after.iter().any(|(t, x)| *t == 0x7A && x == &[0x21, 0xA9]),
        "looked at the Sirius again"
    );
}

/// Owner addition (c), and review r2 MAJOR 2 (second drive): the doors are read before every
/// switch to the drive profile, the transponder status is published at every BMT session.
#[test]
fn every_drive_gets_a_predrive_door_read_and_every_stop_a_transponder_status() {
    // The Sirius says 0 rpm: each "start" (a dip, then a charging voltage) makes the bridge
    // leave the BMT for the Sirius, which then shows the engine off, so it goes back to the
    // BMT (a new session): two drives' worth of switches.
    let scenario = bmt_scenario(true)
        .number("Door bits", 4.0)
        .number("Ignition bits", 1.0);
    let sirius = sirius_ecu(sirius_scenario().number("Engine speed", 0.0));
    let mut rig = Rig::new(sirius, bmt_ecu(scenario));
    rig.connect(OWNER, Role::Owner);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.battery(12_600);
    rig.run_for(10_000); // parked
    for _ in 0..2 {
        rig.battery(10_500); // cranking
        rig.run_for(300);
        rig.battery(14_100);
        rig.run_for(25_000);
    }
    let x: Vec<ExtraFrame> = rig
        .car
        .channel(Channel::Extra)
        .iter()
        .map(|f| ExtraFrame::decode(f).unwrap())
        .filter(|f| f.ecu == 0x26)
        .collect();
    let transponders = x.iter().filter(|f| f.request == [0x21, 0xA4]).count();
    assert!(
        transponders >= 2,
        "{transponders} transponder publishes (one per BMT session)"
    );
    let p = rig.car.written_payloads();
    let stops: Vec<usize> = p
        .iter()
        .enumerate()
        .filter(|(_, (t, x))| *t == 0x26 && x == &[0x82])
        .map(|(i, _)| i)
        .collect();
    assert!(stops.len() >= 2, "two BMT sessions ended");
    for &i in &stops[..2] {
        assert_eq!(
            p[i - 1],
            (0x26, vec![0x21, 0xA0]),
            "door read before each drive"
        );
    }
}

/// Owner addition (b), review r2 MAJOR 2: the transponder status is published at the first
/// read of every BMT session (whatever the bit says) and at every change of the bit, also
/// across sessions.
#[test]
fn transponder_status_first_read_of_a_session_and_every_change() {
    for ignition in [0.0, 1.0] {
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
        let published: Vec<ExtraFrame> = rig
            .car
            .channel(Channel::Extra)
            .iter()
            .map(|f| ExtraFrame::decode(f).unwrap())
            .filter(|f| f.ecu == 0x26 && f.request == [0x21, 0xA4])
            .collect();
        assert_eq!(
            published.len(),
            1,
            "ignition {ignition}: the first read only"
        );
    }
    for (from, to) in [(0.0, 1.0), (1.0, 0.0)] {
        let bmt = bmt_ecu(bmt_scenario(true).number("Ignition bits", from)).with_change_after(
            &[0x21, 0xA4],
            10_000,
            "Ignition bits",
            Value::Number(to),
        );
        let mut rig = Rig::new(sirius_ecu(sirius_scenario()), bmt);
        rig.connect(OWNER, Role::Owner);
        rig.battery(12_600);
        rig.send(OWNER, Op::StartLive(Profile::Parked));
        rig.run_for(30_000);
        let bits: Vec<u8> = rig
            .car
            .channel(Channel::Extra)
            .iter()
            .map(|f| ExtraFrame::decode(f).unwrap())
            .filter(|f| f.request == [0x21, 0xA4])
            .map(|f| f.reply[3] & 0x01)
            .collect();
        assert_eq!(bits, vec![from as u8, to as u8], "{from} -> {to}");
    }
}

/// Review r2 NIT: exactly 8 parked cycles between two transponder reads.
#[test]
fn the_transponder_is_read_every_eighth_cycle() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_for(40_000);
    let lids: Vec<u8> = rig
        .car
        .written_payloads()
        .iter()
        .filter(|(t, x)| *t == 0x26 && x.len() == 2 && x[0] == 0x21)
        .map(|(_, x)| x[1])
        .collect();
    let at: Vec<usize> = lids
        .iter()
        .enumerate()
        .filter(|(_, l)| **l == 0xA4)
        .map(|(i, _)| i)
        .collect();
    assert!(at.len() >= 3);
    for w in at.windows(2) {
        let between = &lids[w[0] + 1..w[1]];
        assert_eq!(
            between.iter().filter(|l| **l == 0xA3).count(),
            8,
            "{between:02X?}"
        );
    }
}

/// The ECU boots slowly (two inits unanswered) on a charger with the ignition on: the BMT's
/// ignition bit ends the silent proof, the third init works, the engine data come.
#[test]
fn after_a_slow_ecu_the_ignition_bit_brings_the_drive_profile_back() {
    let sirius = sirius_ecu(sirius_scenario()).with_init_fault_for(InitFault::Silent, 2);
    let bmt = bmt_ecu(bmt_scenario(true).number("Ignition bits", 1.0));
    let mut rig = Rig::new(sirius, bmt);
    rig.connect(OWNER, Role::Owner);
    rig.battery(13_600);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.run_for(60_000);
    let t = first_live_after(&rig, 0).expect("engine data");
    assert!(t < 40_000, "first Live at {t} ms");
}
