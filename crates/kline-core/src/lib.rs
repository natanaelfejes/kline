//! Sans-IO K-line protocol engine for Renault K-line ECUs (target vehicle: Clio II), and the safety policy.
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
pub mod retry;
pub mod safety;
pub mod scan;
mod seq;
pub mod session;
#[cfg(feature = "spec")]
pub mod spec;
pub mod timing;

pub use error::LinkError;

/// Compile-time regressions for security review M2 (doctests only, no items).
///
/// Control: the types below exist under these paths and the trait check compiles for a `Clone`
/// type, so each `compile_fail` below fails for the reason it names, not for a typo.
///
/// ```
/// fn assert_clone<T: Clone>() {}
/// assert_clone::<kline_core::LinkConfig>();
/// fn _named(
///     _: kline_core::session::Session,
///     _: kline_core::init::FiveBaudInit,
///     _: kline_core::init::FastInit,
///     _: kline_core::init::InitOutcome,
///     _: kline_core::io::Frame,
///     _: kline_core::Action,
/// ) {
/// }
/// fn _bytes(f: &kline_core::io::Frame) -> &[u8] {
///     f.bytes()
/// }
/// ```
///
/// F2: `Session` is not `Clone` (one approval, one write).
/// ```compile_fail,E0277
/// fn assert_clone<T: Clone>() {}
/// assert_clone::<kline_core::session::Session>();
/// ```
/// F2: `FiveBaudInit` is not `Clone`.
/// ```compile_fail,E0277
/// fn assert_clone<T: Clone>() {}
/// assert_clone::<kline_core::init::FiveBaudInit>();
/// ```
/// F2: `FastInit` is not `Clone` (it holds the StartCommunication exchange).
/// ```compile_fail,E0277
/// fn assert_clone<T: Clone>() {}
/// assert_clone::<kline_core::init::FastInit>();
/// ```
/// F2/F3: `InitOutcome` is not `Clone`: one init, one session.
/// ```compile_fail,E0277
/// fn assert_clone<T: Clone>() {}
/// assert_clone::<kline_core::init::InitOutcome>();
/// ```
/// F2/F4: an `Action` (and its `Frame`) cannot be duplicated.
/// ```compile_fail,E0277
/// fn assert_clone<T: Clone>() {}
/// assert_clone::<kline_core::Action>();
/// ```
/// ```compile_fail,E0277
/// fn assert_clone<T: Clone>() {}
/// assert_clone::<kline_core::io::Frame>();
/// ```
/// F3b/F3c: an `InitOutcome` cannot be fabricated by a caller.
/// ```compile_fail,E0451
/// let _ = kline_core::init::InitOutcome {
///     cfg: kline_core::LinkConfig::new(0x26, kline_core::KTiming::default()),
///     kind: kline_core::InitKind::Iso8FiveBaud,
///     target: kline_core::safety::Target {
///         protocol: kline_core::safety::Protocol::Iso8,
///         ecu_address: 0x26,
///     },
///     address_byte: None,
///     sync: None,
///     kw1: 0xD0,
///     kw2: 0x8F,
///     address_complement: None,
///     keywords_match: None,
/// };
/// ```
/// F4: a transport cannot build a frame of its own, by literal ...
/// ```compile_fail,E0451
/// let _ = kline_core::io::Frame { bytes: vec![0x82, 0x26, 0xF1, 0x27, 0x01, 0xC1] };
/// ```
/// ... or by the crate-private constructor.
/// ```compile_fail,E0624
/// let _ = kline_core::io::Frame::new(vec![0x27, 0x01]);
/// ```
/// F4: a safety `Permit` cannot be fabricated either.
/// ```compile_fail,E0624
/// let t = kline_core::safety::Target { protocol: kline_core::safety::Protocol::Kwp, ecu_address: 0x26 };
/// let _ = kline_core::safety::Permit::new(&[0x27, 0x01], t, kline_core::safety::Tier::Read);
/// ```
#[cfg_attr(
    not(feature = "approved-routines"),
    doc = "F1: without the `approved-routines` feature there is no approval type at all.\n\
           ```compile_fail,E0425\n\
           let _ = kline_core::safety::Approval::confirm;\n\
           ```"
)]
#[cfg_attr(
    not(feature = "approved-routines"),
    doc = "Nor a way to ask for one.\n\
           ```compile_fail,E0599\n\
           fn f(s: &mut kline_core::session::Session) { let _ = s.service_challenge(&[0x14, 0xFF, 0x00]); }\n\
           ```"
)]
#[cfg_attr(
    feature = "approved-routines",
    doc = "F1 (feature on): an `Approval` has no public constructor besides `confirm`.\n\
           ```compile_fail,E0451\n\
           let _ = kline_core::safety::Approval { session: 0, request: 0, \
           target: kline_core::safety::Target { protocol: kline_core::safety::Protocol::Kwp, ecu_address: 0x26 }, \
           tier: kline_core::safety::Tier::Service, payload: vec![0x31, 0xC7, 0, 0, 0], identification: None, \
           risk: kline_core::safety::assess(&kline_core::safety::Target { protocol: kline_core::safety::Protocol::Kwp, ecu_address: 0x26 }, &[0x31, 0xC7, 0, 0, 0]).unwrap() };\n\
           ```"
)]
pub mod security_regressions {}
pub use io::{Action, Event, Step};
pub use retry::{RetryPolicy, TesterPresentMode};
pub use timing::{InitKind, InitVariant, KTiming, LinkConfig};
