//! The sans-IO contract between the state machines and a transport driver.
//!
//! A machine emits one [`Action`] at a time. The driver performs it and feeds back exactly one
//! [`Event`]:
//!
//! | Action | Event |
//! |---|---|
//! | `SetDtr`, `SetBreak`, `Sleep`, `FlushRx`, `Write` | `Done` |
//! | `CheckBreakLoopback` | `BreakLoopbackSeen(bool)` |
//! | `Read { count, .. }` | `ReadOk(bytes)` with exactly `count` bytes, or `ReadTimeout(partial)` with fewer |
//!
//! `Read::timeout_ms` is a total timeout for the whole call, not an inter-byte timeout
//! (RE: IOTBD32V3 COMMTIMEOUTS: interval 0, multiplier 0). Transport failures (port gone) are the
//! driver's business: it stops feeding events and drops the machine.

use alloc::vec::Vec;

/// What the driver must do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Set (true) or clear (false) the DTR modem line.
    SetDtr(bool),
    /// true: hold TX in break (K line low); false: release it (K line idle high).
    SetBreak(bool),
    /// Block for this many milliseconds.
    Sleep { ms: u32 },
    /// Report whether RX saw the K line go low since the last `SetBreak(true)` (on a KKL cable
    /// the break loops back to RX as a break condition / 0x00 with framing error).
    CheckBreakLoopback,
    /// Discard everything in the RX buffer.
    FlushRx,
    /// Transmit `bytes`. `inter_byte_ms == 0`: in one write; otherwise byte by byte with this gap.
    Write { bytes: Vec<u8>, inter_byte_ms: u32 },
    /// Read exactly `count` bytes within `timeout_ms` in total.
    Read { count: usize, timeout_ms: u32 },
}

/// What happened, in reply to the last [`Action`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A non-read action completed.
    Done,
    /// Reply to [`Action::CheckBreakLoopback`].
    BreakLoopbackSeen(bool),
    /// All requested bytes arrived.
    ReadOk(Vec<u8>),
    /// The timeout expired; the bytes that did arrive (possibly none).
    ReadTimeout(Vec<u8>),
}

/// Result of feeding a machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step<T, E> {
    /// Perform this action and report its event.
    Act(Action),
    /// Finished successfully. Further events are an error.
    Done(T),
    /// Finished with an error. Further events are an error.
    Failed(E),
}
