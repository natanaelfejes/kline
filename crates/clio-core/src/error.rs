use alloc::vec::Vec;

use thiserror::Error;

use crate::frame::{FrameError, Reply};
use crate::safety::Denied;

/// Errors of the init and exchange machines. Comments name the matching DDT2000 status where
/// there is one (RE: IOTBD32V3 status codes).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LinkError {
    /// No break seen on RX during the 5-baud start bit, or no echo at all (0x3F0).
    #[error("K line inactive")]
    KLineInactive,
    /// Fewer than 3 bytes (0x55 KW1 KW2) after the 5-baud address (0x3E8).
    #[error("no keywords received (got {received:02X?})")]
    NoKeywords { received: Vec<u8> },
    /// Key bytes differ from the definition and `reject_keyword_mismatch` is set.
    #[error("keywords {received:02X?} differ from expected {expected:02X?}")]
    KeywordMismatch {
        expected: (u8, u8),
        received: (u8, u8),
    },
    /// No ~address byte after ~KW2 (0x3E9).
    #[error("address complement not received (got {received:02X?})")]
    NoAddressComplement { received: Vec<u8> },
    /// Echo differs from what was sent, or stops short (0x3ED).
    #[error("data collision on K line: sent {sent:02X?}, echo {echo:02X?}")]
    Collision { sent: Vec<u8>, echo: Vec<u8> },
    /// No reply byte within RequestToReply timeout (DDT2000 0x7D0).
    #[error("no reply from ECU")]
    NoReply,
    /// The reply stopped before its declared length (0x3EE).
    #[error("incomplete reply {received:02X?}")]
    IncompleteReply { received: Vec<u8> },
    /// The reply frame is malformed.
    #[error("bad reply frame: {0}")]
    Frame(#[from] FrameError),
    /// More consecutive 0x78 replies than `max_response_pending`.
    #[error("ECU kept answering responsePending ({count} times)")]
    TooManyPending { count: u8 },
    /// StartCommunication was answered with something other than `C1 KW1 KW2`.
    #[error("unexpected StartCommunication reply {0:?}")]
    UnexpectedStartReply(Reply),
    /// The request was refused by the safety policy before anything was written.
    #[error(transparent)]
    Denied(#[from] Denied),
    /// The init kind of the config does not match the machine.
    #[error("init kind does not match this init machine")]
    WrongInitKind,
    /// The driver fed an event that does not answer the last action, or fed a finished machine.
    #[error("event does not match the outstanding action")]
    UnexpectedEvent,
}
