//! `BridgeClient` against a kline-bridge running on kline-sim (feature `ble`).
#![cfg(feature = "ble")]

#[path = "support/ble_sim.rs"]
mod ble_sim;

use ble_sim::SimBridge;
use kline_bridge::gate::Role;
use kline_bridge::gatt::{BridgeState, Op, ResultCode};
use kline_bridge::profile::EcuId;
use kline_core::safety::Tier;
use kline_host::ble::{BleError, BridgeClient};

#[test]
fn agent_reads_through_the_bridge() {
    let mut c = BridgeClient::with_counter(Box::new(SimBridge::new(Role::Agent)), 100);
    let r = c.read(EcuId::Sirius, &[0x21, 0xA9]).unwrap();
    assert_eq!(&r[..2], &[0x61, 0xA9]);
    let r = c.read(EcuId::Bmt, &[0x21, 0xA3]).unwrap();
    assert_eq!(&r[..2], &[0x61, 0xA3]);
    assert_eq!(c.status().unwrap().state, BridgeState::Session);
}

#[test]
fn the_client_refuses_non_reads_before_writing() {
    let mut c = BridgeClient::with_counter(Box::new(SimBridge::new(Role::Agent)), 1);
    match c.read(EcuId::Sirius, &[0x14, 0x00, 0x00]) {
        Err(BleError::NotRead {
            tier: Some(Tier::Service),
        }) => {}
        other => panic!("{other:?}"),
    }
    match c.read(EcuId::Bmt, &[0x21, 0xA4, 0x01]) {
        Err(BleError::NotRead { tier: None }) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_bridge_refuses_them_too_for_the_agent_bond() {
    // Bypass the client check: write the commands directly.
    let mut c = BridgeClient::with_counter(Box::new(SimBridge::new(Role::Agent)), 1);
    let r = c
        .call(Op::Request {
            ecu: EcuId::Sirius,
            payload: vec![0x14, 0x00, 0x00],
        })
        .unwrap();
    assert_eq!(r.result, ResultCode::NotAllowedForRole);
    let r = c
        .call(Op::Prepare {
            ecu: EcuId::Bmt,
            payload: vec![0x31, 0xA0, 0x00, 0x01],
        })
        .unwrap();
    assert_eq!(r.result, ResultCode::NotAllowedForRole);
    let r = c.call(Op::Lock).unwrap();
    assert_eq!(r.result, ResultCode::NotAllowedForRole);
    // A read still works afterwards; nothing but reads went out.
    c.read(EcuId::Sirius, &[0x21, 0xAA]).unwrap();
    let link = c.into_link();
    drop(link);
}

#[test]
fn yield_and_resume() {
    let mut c = BridgeClient::with_counter(Box::new(SimBridge::new(Role::Agent)), 1);
    c.read(EcuId::Sirius, &[0x21, 0xA9]).unwrap();
    c.yield_kline().unwrap();
    match c.read(EcuId::Sirius, &[0x21, 0xA9]) {
        Err(BleError::Bridge {
            code: ResultCode::Yielded,
            ..
        }) => {}
        other => panic!("{other:?}"),
    }
    c.resume_kline().unwrap();
    c.read(EcuId::Sirius, &[0x21, 0xA9]).unwrap();
}
