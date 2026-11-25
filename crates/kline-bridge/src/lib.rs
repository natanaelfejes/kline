//! Clio Link bridge logic: what the ESP32-C3 firmware does between the car's K line and BLE,
//! written sans-IO so it runs unchanged on the host against kline-sim.
//!
//! - [`bridge::Bridge`]: the state machine (profiles, scheduler, keep-alive, reconnect, sleep,
//!   client jobs, lock-when-moving execution). One K-line master, always.
//! - [`gatt`]: the BLE GATT protocol v1 as a pure codec (docs/bridge-protocol.md).
//! - [`gate`]: BLE roles on top of kline-core's tier policy.
//! - [`lwm`]: the lock-when-moving automaton (MASTER-PLAN-v2 3.1).
//! - [`ratelimit`]: per-client counters, rate limits.
//! - [`fob`]: the fob PhotoMOS output driver (never pulses on boot or reset).
//! - [`hal`], [`runner`]: the hardware traits and the loop that drives the bridge over them.
//!
//! `no_std` + `alloc` without the `std` feature. kline-core's safety policy decides every byte
//! that goes on the wire; this crate cannot build a frame (kline-core's `Frame` has no public
//! constructor).

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod bridge;
pub mod crc;
pub mod fob;
pub mod gate;
pub mod gatt;
pub mod guard_rules;
pub mod hal;
pub mod live;
pub mod lwm;
pub mod profile;
pub mod ratelimit;
pub mod runner;
pub mod soc;

pub use bridge::{Bridge, BridgeConfig, Output};
