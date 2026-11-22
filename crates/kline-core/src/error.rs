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
    #[error("no keywords received (got {})", HexSp(received))]
    NoKeywords { received: Vec<u8> },
    /// Key bytes differ from the definition and `reject_keyword_mismatch` is set.
    #[error("keywords {received:02X?} differ from expected {expected:02X?}")]
    KeywordMismatch {
        expected: (u8, u8),
        received: (u8, u8),
    },
    /// No ~address byte after ~KW2 (0x3E9).
    #[error("address complement not received (got {})", HexSp(received))]
    NoAddressComplement { received: Vec<u8> },
    /// Echo differs from what was sent, or stops short (0x3ED).
    #[error("data collision on K line: sent {}, echo {}", HexSp(sent), HexSp(echo))]
    Collision { sent: Vec<u8>, echo: Vec<u8> },
    /// No reply byte within RequestToReply timeout (DDT2000 0x7D0).
    #[error("no reply from ECU")]
    NoReply,
    /// The reply stopped before its declared length (0x3EE).
    #[error("incomplete reply {}", HexSp(received))]
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
    /// The ECU address has bit 7 set: it is the Multipoint wake byte of another address
    /// (`address ^ 0x80`), so the woken ECU could differ from the one the policy checks.
    #[error("ECU address {address:#04x} aliases another ECU (bit 7 set)")]
    AliasedAddress { address: u8 },
    /// A permit was issued for another protocol or ECU than the framing addresses.
    #[error("permit target does not match the framing")]
    PermitMismatch,
    /// The driver fed an event that does not answer the last action, or fed a finished machine.
    #[error("event does not match the outstanding action")]
    UnexpectedEvent,
    /// The host woke up from a wake-up sleep (5-baud start/data bit, fast-init TiniL/TWuP) so
    /// late that the pattern was already outside its tolerance; the transport aborted the
    /// attempt (ours, no DDT2000 status). In a 5-baud address it held K low through the
    /// stop-bit slot first, so no receiver decodes a valid, different address byte.
    #[error(
        "init timing missed: a {requested_ms} ms wake-up sleep woke {late_us} us late \
         (max {max_late_us} us); attempt aborted, K line released"
    )]
    InitTiming {
        requested_ms: u32,
        late_us: u64,
        max_late_us: u64,
    },
}

/// Bytes as spaced upper-case hex (`86 F1 26`), the format of every frame in the trace;
/// `nothing` when empty.
pub(crate) struct HexSp<'a>(pub &'a [u8]);

impl core::fmt::Display for HexSp<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0.is_empty() {
            return f.write_str("nothing");
        }
        for (i, b) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{b:02X}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    #[test]
    fn byte_lists_print_as_spaced_hex_like_the_trace() {
        let e = LinkError::IncompleteReply {
            received: vec![0x86, 0xF1, 0x26, 0x61, 0xA3, 0x03, 0x00, 0x00],
        };
        assert_eq!(e.to_string(), "incomplete reply 86 F1 26 61 A3 03 00 00");
        let e = LinkError::NoKeywords { received: vec![] };
        assert_eq!(e.to_string(), "no keywords received (got nothing)");
        let e = LinkError::NoAddressComplement {
            received: vec![0x55],
        };
        assert_eq!(e.to_string(), "address complement not received (got 55)");
        let e = LinkError::Collision {
            sent: vec![0x81, 0x7A],
            echo: vec![0x81],
        };
        assert_eq!(
            e.to_string(),
            "data collision on K line: sent 81 7A, echo 81"
        );
    }
}
