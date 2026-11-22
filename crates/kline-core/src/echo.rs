//! Echo removal for a dumb K-line cable, which receives every byte it transmits.
//!
//! RE: IOTBD32V3 send routine: after a write, read as many bytes as were sent; if the first one is
//! 0x00 (break residue), drop it and read one more; any difference is a collision.

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EchoError {
    /// The bytes received so far match but are fewer than the frame.
    #[error("echo incomplete")]
    Incomplete,
    /// A received byte differs from the sent one: bus collision or a broken cable.
    #[error("echo differs from the transmitted bytes")]
    Mismatch,
}

/// Strip the echo of `sent` from the start of `rx` and return what follows it.
///
/// One leading 0x00 is tolerated as break residue. An exact match is preferred, so a frame that
/// itself starts with 0x00 is still recognised.
pub fn strip_echo<'a>(sent: &[u8], rx: &'a [u8]) -> Result<&'a [u8], EchoError> {
    match prefix(sent, rx) {
        Ok(rest) => Ok(rest),
        Err(first) => match rx.split_first() {
            Some((0x00, tail)) => prefix(sent, tail),
            _ => Err(first),
        },
    }
}

fn prefix<'a>(sent: &[u8], rx: &'a [u8]) -> Result<&'a [u8], EchoError> {
    let n = sent.len().min(rx.len());
    if sent[..n] != rx[..n] {
        Err(EchoError::Mismatch)
    } else if rx.len() < sent.len() {
        Err(EchoError::Incomplete)
    } else {
        Ok(&rx[sent.len()..])
    }
}
