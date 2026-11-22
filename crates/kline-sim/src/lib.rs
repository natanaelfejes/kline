//! ECU simulator for end-to-end tests without the car. See docs/sim.md, including what it
//! can and cannot prove.
//!
//! - [`bus::Bus`]: the K line of a dumb KKL cable: echo, break loopback, 5-baud and fast-init
//!   wake-up detection, timed delivery of the ECU's bytes.
//! - [`ecu::SpecEcu`]: an ECU answering from a kline-spec definition and a
//!   [`scenario::Scenario`], with failure modes and the ECU write log ([`journal`]).
//! - [`replay::ReplayEcu`]: an ECU replaying a recorded JSONL log.
//! - Transports: [`line::VirtualLine`] (in-process, virtual clock) and [`pty::PtySim`]
//!   (pseudo-terminal plus a control socket for break/DTR).
//! - [`driver::Driver`]: runs kline-core's machines over any [`line::Line`].

#![forbid(unsafe_code)]

pub mod bus;
pub mod classify;
pub mod driver;
pub mod ecu;
pub mod frame;
pub mod journal;
pub mod line;
#[cfg(unix)]
pub mod pty;
pub mod replay;
pub mod scenario;
pub mod timing;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SimError {
    #[error("scenario names unknown data {0:?}")]
    UnknownData(String),
    #[error("cannot encode scenario value for {data:?}: {source}")]
    Encode {
        data: String,
        source: kline_spec::SpecError,
    },
    #[error("empty request")]
    EmptyRequest,
    #[error("service {0:#04x} has no positive response SID")]
    NoPositiveSid(u8),
    #[error("definition has no <Function Address>")]
    NoAddress,
    #[error("definition has no usable <K> section")]
    NoKLine,
    #[error("definition declares no key bytes")]
    NoKeywords,
    #[error("profile address {profile:#04x} differs from the definition's {definition:#04x}")]
    AddressMismatch { profile: u8, definition: u8 },
}
