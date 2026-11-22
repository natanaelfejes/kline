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
use core::fmt;

/// Bytes to put on the K line, produced only inside this crate.
///
/// There is no public constructor, `From` impl or `Clone`: a transport can read the bytes with
/// [`Frame::bytes`] but cannot make a frame of its own, so every [`Action::Write`] it performs
/// is a frame the core built (from a safety [`Permit`](crate::safety::Permit) or a protocol
/// handshake byte). Security review M2 finding 4.
#[derive(PartialEq, Eq)]
pub struct Frame {
    bytes: Vec<u8>,
}

impl Frame {
    pub(crate) fn new(bytes: Vec<u8>) -> Frame {
        Frame { bytes }
    }

    /// The bytes to transmit, in order.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Frame({:02X?})", self.bytes)
    }
}

/// What the driver must do next.
///
/// Not `Clone`: an action is performed once.
#[derive(Debug, PartialEq, Eq)]
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
    /// Transmit `frame.bytes()`. `inter_byte_ms == 0`: in one write; otherwise byte by byte with
    /// this gap.
    Write { frame: Frame, inter_byte_ms: u32 },
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
#[derive(Debug, PartialEq, Eq)]
pub enum Step<T, E> {
    /// Perform this action and report its event.
    Act(Action),
    /// Finished successfully. Further events are an error.
    Done(T),
    /// Finished with an error. Further events are an error.
    Failed(E),
}
