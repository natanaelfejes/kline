//! Power states (owner decision 2026-11-25): light sleep with BLE advertising when parked, deep
//! sleep after 48 h parked or on a low battery, and the stuck-awake watchdog.

mod common;

use common::*;
use kline_bridge::bridge::{BridgeConfig, SleepReason};
use kline_bridge::gate::Role;
use kline_bridge::gatt::Op;
use kline_bridge::power::SleepMode;
use kline_bridge::profile::Profile;

#[test]
fn reasons_map_to_light_or_deep_sleep() {
    assert_eq!(SleepReason::Idle.mode(), SleepMode::LightAdvertising);
    assert_eq!(SleepReason::NoClient.mode(), SleepMode::LightAdvertising);
    for r in [
        SleepReason::LowBattery,
        SleepReason::CriticalBattery,
        SleepReason::ParkedLong,
    ] {
        assert_eq!(r.mode(), SleepMode::DeepTimer, "{r:?}");
    }
}

/// The phone asked for parked and left: the bridge would poll the BMT forever. After 10 min
/// without a client it sleeps (light, BLE advertising), and not before.
#[test]
fn the_stuck_awake_watchdog_sleeps_after_ten_minutes_without_a_client() {
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    assert_eq!(
        rig.call(OWNER, Op::StartLive(Profile::Parked), 1_000)
            .result,
        kline_bridge::gatt::ResultCode::Ok
    );
    let t = rig.now();
    rig.bridge.on_disconnect(OWNER, t);
    rig.run_for(9 * 60_000);
    assert!(rig.car.sleeps.is_empty(), "{:?}", rig.car.sleeps);
    rig.run_for(2 * 60_000);
    let s = rig.car.sleeps.first().expect("asleep");
    assert_eq!(s.2, SleepReason::NoClient);
    assert!(s.0 >= t + 10 * 60_000);
}

/// Parked longer than the light-sleep limit since the engine last ran: deep sleep with the
/// 30 s timer wake (the limit is shortened for the test).
#[test]
fn parked_longer_than_the_limit_goes_to_deep_sleep() {
    let cfg = BridgeConfig {
        parked_light_max_ms: 20 * 60_000,
        ..BridgeConfig::default()
    };
    let car = Car::new(sirius_ecu(sirius_scenario()), bmt_ecu(bmt_scenario(true)));
    let mut rig = Rig::from_car(car, cfg);
    rig.battery(12_600);
    rig.run_for(30 * 60_000);
    let reasons: Vec<SleepReason> = rig.car.sleeps.iter().map(|s| s.2).collect();
    assert_eq!(reasons.first(), Some(&SleepReason::Idle));
    let deep = rig
        .car
        .sleeps
        .iter()
        .find(|s| s.2 == SleepReason::ParkedLong)
        .expect("deep sleep");
    assert!(deep.0 >= 20 * 60_000);
    assert_eq!(deep.1, Some(30_000));
}

/// The stuck-awake watchdog never stops the drive profile: with the engine running and no
/// client for over 10 minutes the bridge keeps reading the Sirius (lock-when-moving and the
/// alerts need it when the phone comes back).
#[test]
fn the_watchdog_never_stops_the_drive_profile() {
    let mut rig = Rig::default_car();
    rig.battery(14_100);
    rig.run_until(11 * 60_000);
    assert!(rig.car.sleeps.is_empty(), "{:?}", rig.car.sleeps);
    let late = rig
        .car
        .written_payloads()
        .iter()
        .filter(|(ecu, p)| *ecu == 0x7A && p.as_slice() == [0x21, 0xA9])
        .count();
    let n = rig.car.written_payloads().len();
    rig.run_for(10_000);
    let later = rig.car.written_payloads()[n..]
        .iter()
        .filter(|(ecu, p)| *ecu == 0x7A && p.as_slice() == [0x21, 0xA9])
        .count();
    assert!(late > 0 && later > 3, "Sirius still read: {later}");
}
