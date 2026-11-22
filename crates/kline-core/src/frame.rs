//! ISO8 and KWP2000 frame codec. See docs/protocol.md "Framing".
//!
//! - ISO8: `[N][data..][cs]`, `N = data.len() + 1`, no addresses.
//! - KWP2000: `[0x80+n][tgt][src][data..][cs]`, or `[0x80][tgt][src][n][data..][cs]` when
//!   `n > 0x3F` or the ECU does not support the length in the format byte.
//! - `cs` is the 8-bit sum of all preceding bytes in both framings.

use alloc::vec::Vec;

use thiserror::Error;

/// Largest ISO8 payload: `N = n + 1` must fit in one byte.
pub const ISO8_MAX_PAYLOAD: usize = 254;
/// Largest KWP payload with the length in the format byte.
pub const KWP_MAX_FMT_LEN: usize = 0x3F;
/// Largest KWP payload with a separate length byte. RE: KWP2000Diag4 KWP builder (error 0xBB8).
pub const KWP_MAX_PAYLOAD: usize = 0xFF;

/// KWP format byte: physical addressing with target and source bytes.
const FMT_PHYSICAL: u8 = 0x80;
const FMT_ADDR_MASK: u8 = 0xC0;
const FMT_LEN_MASK: u8 = 0x3F;

/// Framing errors (encode and decode).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FrameError {
    #[error("empty payload")]
    EmptyPayload,
    #[error("payload of {len} bytes exceeds the maximum of {max}")]
    PayloadTooLong { len: usize, max: usize },
    #[error("frame of {len} bytes is too short")]
    TooShort { len: usize },
    #[error("checksum mismatch: frame says {actual:#04x}, computed {expected:#04x}")]
    Checksum { expected: u8, actual: u8 },
    #[error("length field says {declared} bytes, frame carries {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("reply addressed to {actual:#04x}, tool is {expected:#04x}")]
    ToolAddress { expected: u8, actual: u8 },
    #[error("reply from {actual:#04x}, ECU is {expected:#04x}")]
    EcuAddress { expected: u8, actual: u8 },
    #[error("unsupported KWP format byte {0:#04x} (only physical addressing 0x80 is accepted)")]
    UnsupportedFormat(u8),
    #[error("negative response shorter than 7F SID NRC")]
    MalformedNegative,
}

/// 8-bit sum of `bytes`. RE: KWP2000Diag4 builders (DiagLibs2 ByteArray.Sum & 0xFF).
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b))
}

/// KWP2000 header parameters, seen from the sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KwpHeader {
    /// Receiver address (the ECU for requests).
    pub target: u8,
    /// Sender address (the tool, 0xF1, for requests).
    pub source: u8,
    /// Put the length in the format byte when `n <= 0x3F`. Cleared after init when KW1 is not
    /// 0xD0 and KW1 bit 0 is 0 (RE: KWP2000Diag4 KWP2000Ecu init), see
    /// [`KwpHeader::length_in_format_for_kw1`].
    pub length_in_format: bool,
}

impl KwpHeader {
    /// DDT2000's rule for `length_in_format` from the received KW1.
    pub fn length_in_format_for_kw1(kw1: u8) -> bool {
        kw1 == 0xD0 || kw1 & 0x01 != 0
    }
}

/// How frames are built and checked on one link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    Iso8,
    Kwp(KwpHeader),
}

/// How many more bytes to read after the first byte of a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyNeed {
    /// Read this many bytes; then the frame is complete.
    Bytes(usize),
    /// KWP with `fmt & 0x3F == 0`: read target, source and length (3 bytes), then
    /// [`Framing::need_after_length`].
    HeaderWithLength,
}

impl Framing {
    /// The framing of frames travelling the other way (ECU -> tool): KWP addresses swapped.
    /// Used by simulators and tests to build replies that [`Framing::decode`] accepts.
    pub fn reversed(self) -> Framing {
        match self {
            Framing::Iso8 => Framing::Iso8,
            Framing::Kwp(h) => Framing::Kwp(KwpHeader {
                target: h.source,
                source: h.target,
                length_in_format: h.length_in_format,
            }),
        }
    }

    /// Build the wire frame for `payload` (service byte + parameters).
    pub fn encode(&self, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
        if payload.is_empty() {
            return Err(FrameError::EmptyPayload);
        }
        let mut f = Vec::with_capacity(payload.len() + 5);
        match self {
            Framing::Iso8 => {
                if payload.len() > ISO8_MAX_PAYLOAD {
                    return Err(FrameError::PayloadTooLong {
                        len: payload.len(),
                        max: ISO8_MAX_PAYLOAD,
                    });
                }
                f.push(payload.len() as u8 + 1);
            }
            Framing::Kwp(h) => {
                let n = payload.len();
                if n > KWP_MAX_PAYLOAD {
                    return Err(FrameError::PayloadTooLong {
                        len: n,
                        max: KWP_MAX_PAYLOAD,
                    });
                }
                if n <= KWP_MAX_FMT_LEN && h.length_in_format {
                    f.extend_from_slice(&[FMT_PHYSICAL | n as u8, h.target, h.source]);
                } else {
                    f.extend_from_slice(&[FMT_PHYSICAL, h.target, h.source, n as u8]);
                }
            }
        }
        f.extend_from_slice(payload);
        f.push(checksum(&f));
        Ok(f)
    }

    /// After reading the first byte of a reply, how much more to read.
    pub fn need_after_first(&self, first: u8) -> Result<ReplyNeed, FrameError> {
        match self {
            // RE: IOTBD32V3 worker (ISO8 flag): first byte counts the bytes that follow.
            Framing::Iso8 => {
                if first < 2 {
                    Err(FrameError::TooShort {
                        len: first as usize + 1,
                    })
                } else {
                    Ok(ReplyNeed::Bytes(first as usize))
                }
            }
            // RE: IOTBD32V3 worker: (fmt & 0x3F) + 3 (tgt, src, data, cs). We also handle the
            // separate length byte, which DDT2000's reader does not.
            Framing::Kwp(_) => {
                if first & FMT_ADDR_MASK != FMT_PHYSICAL {
                    return Err(FrameError::UnsupportedFormat(first));
                }
                match (first & FMT_LEN_MASK) as usize {
                    0 => Ok(ReplyNeed::HeaderWithLength),
                    n => Ok(ReplyNeed::Bytes(n + 3)),
                }
            }
        }
    }

    /// For [`ReplyNeed::HeaderWithLength`]: bytes still to read after `[fmt tgt src len]`
    /// (data plus checksum).
    pub fn need_after_length(len: u8) -> Result<usize, FrameError> {
        if len == 0 {
            Err(FrameError::TooShort { len: 4 })
        } else {
            Ok(len as usize + 1)
        }
    }

    /// Validate a complete reply frame and return its payload (starting at the response SID).
    pub fn decode<'a>(&self, frame: &'a [u8]) -> Result<&'a [u8], FrameError> {
        let len = frame.len();
        match self {
            Framing::Iso8 => {
                if len < 3 {
                    return Err(FrameError::TooShort { len });
                }
                if frame[0] as usize != len - 1 {
                    return Err(FrameError::LengthMismatch {
                        declared: frame[0] as usize,
                        actual: len - 1,
                    });
                }
                check_sum(frame)?;
                Ok(&frame[1..len - 1])
            }
            Framing::Kwp(h) => {
                if len < 5 {
                    return Err(FrameError::TooShort { len });
                }
                let fmt = frame[0];
                if fmt & FMT_ADDR_MASK != FMT_PHYSICAL {
                    return Err(FrameError::UnsupportedFormat(fmt));
                }
                check_sum(frame)?;
                // A reply travels ECU -> tool: tgt must be us (our source), src the ECU.
                if frame[1] != h.source {
                    return Err(FrameError::ToolAddress {
                        expected: h.source,
                        actual: frame[1],
                    });
                }
                if frame[2] != h.target {
                    return Err(FrameError::EcuAddress {
                        expected: h.target,
                        actual: frame[2],
                    });
                }
                let n = (fmt & FMT_LEN_MASK) as usize;
                if n == 0 {
                    if len < 6 {
                        return Err(FrameError::TooShort { len });
                    }
                    let declared = frame[3] as usize;
                    if declared != len - 5 {
                        return Err(FrameError::LengthMismatch {
                            declared,
                            actual: len - 5,
                        });
                    }
                    Ok(&frame[4..len - 1])
                } else {
                    if n != len - 4 {
                        return Err(FrameError::LengthMismatch {
                            declared: n,
                            actual: len - 4,
                        });
                    }
                    Ok(&frame[3..len - 1])
                }
            }
        }
    }
}

fn check_sum(frame: &[u8]) -> Result<(), FrameError> {
    let (body, cs) = frame.split_at(frame.len() - 1);
    let expected = checksum(body);
    if expected != cs[0] {
        return Err(FrameError::Checksum {
            expected,
            actual: cs[0],
        });
    }
    Ok(())
}

/// KWP negative response service id.
pub const NEGATIVE_RESPONSE: u8 = 0x7F;
/// NRC requestCorrectlyReceived-ResponsePending.
pub const NRC_RESPONSE_PENDING: u8 = 0x78;
/// ISO8 3-byte status replies `[02][code][cs]`. RE: KWP2000Diag4 ISO8 parser.
pub const ISO8_STATUS_CODES: [u8; 4] = [0x15, 0x37, 0x38, 0x39];

/// A reply payload, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// Anything that is not a negative/status reply. Starts at the response SID.
    Positive(Vec<u8>),
    /// `7F SID NRC` with NRC != 0x78.
    Negative { sid: u8, nrc: u8 },
    /// `7F SID 78`: the ECU needs more time; keep reading.
    ResponsePending { sid: u8 },
    /// ISO8 single-byte status reply (0x15, 0x37, 0x38, 0x39); meaning unknown.
    Iso8Status(u8),
}

impl Reply {
    /// Classify a decoded payload.
    pub fn classify(framing: &Framing, payload: &[u8]) -> Result<Reply, FrameError> {
        if matches!(framing, Framing::Iso8)
            && payload.len() == 1
            && ISO8_STATUS_CODES.contains(&payload[0])
        {
            return Ok(Reply::Iso8Status(payload[0]));
        }
        if payload.first() == Some(&NEGATIVE_RESPONSE) {
            if payload.len() < 3 {
                return Err(FrameError::MalformedNegative);
            }
            let (sid, nrc) = (payload[1], payload[2]);
            return Ok(if nrc == NRC_RESPONSE_PENDING {
                Reply::ResponsePending { sid }
            } else {
                Reply::Negative { sid, nrc }
            });
        }
        Ok(Reply::Positive(payload.to_vec()))
    }
}
