//! Sans-IO K-line protocol engine for the Clio II ECUs, and the safety policy.
//!
//! `no_std` + `alloc`. No serial port code: the machines emit [`io::Action`]s and consume
//! [`io::Event`]s; a transport driver (CLI, simulator, ESP32) performs them. Time is passed in
//! as values, so every run is deterministic. Behaviour and constants are documented in
//! `docs/protocol.md`.
//!
//! Flow: build a [`LinkConfig`] (by hand or with the `spec` feature from an ECU definition),
//! run [`init::FiveBaudInit`] or [`init::FastInit`], then use a [`session::Session`] for
//! requests. Every request is checked by [`safety`] before it can be written.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod echo;
mod error;
mod exchange;
pub mod frame;
pub mod init;
pub mod io;
pub mod safety;
mod seq;
pub mod session;
#[cfg(feature = "spec")]
pub mod spec;
pub mod timing;

pub use error::LinkError;
pub use io::{Action, Event, Step};
pub use timing::{InitKind, InitVariant, KTiming, LinkConfig};
