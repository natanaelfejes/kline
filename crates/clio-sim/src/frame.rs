//! The ECU side of ISO8 and KWP2000 framing, written independently of `clio_core::frame` so
//! that a bug in the core's codec is not mirrored by the simulator. Both follow the same
//! description (docs/protocol.md "Framing"), so a misreading of that description is shared.

/// 8-bit sum of all bytes.
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |a, &b| a.wrapping_add(b))
}

/// How frames look on this ECU's link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireFormat {
    /// `[N][data][cs]`, `N = len(data) + 1`.
    Iso8,
    /// `[80+n][tgt][src][data][cs]`, or `[80][tgt][src][n][data][cs]`.
    /// `length_in_format`: the ECU puts short lengths in the format byte in its replies.
    Kwp { length_in_format: bool },
}

/// A request as the ECU parsed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// KWP target/source (None for ISO8).
    pub target: Option<u8>,
    pub source: Option<u8>,
    pub payload: Vec<u8>,
}

/// Build a reply frame.
pub fn encode_reply(fmt: WireFormat, ecu: u8, tester: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(payload.len() + 5);
    match fmt {
        WireFormat::Iso8 => f.push(payload.len() as u8 + 1),
        WireFormat::Kwp { length_in_format } => {
            let n = payload.len();
            if length_in_format && n <= 0x3F {
                f.extend_from_slice(&[0x80 | n as u8, tester, ecu]);
            } else {
                f.extend_from_slice(&[0x80, tester, ecu, n as u8]);
            }
        }
    }
    f.extend_from_slice(payload);
    f.push(checksum(&f));
    f
}

/// Collects request bytes until a frame is complete.
#[derive(Debug, Clone, Default)]
pub struct Assembler {
    buf: Vec<u8>,
}

/// Result of feeding one byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feed {
    More,
    Frame(Parsed),
    /// Complete but invalid (checksum, format byte); the reason.
    Bad(String),
}

impl Assembler {
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn clear(&mut self) {
        self.buf.clear();
    }

    fn total_len(&self, fmt: WireFormat) -> Option<Result<usize, String>> {
        let b = &self.buf;
        match fmt {
            WireFormat::Iso8 => {
                let n = *b.first()? as usize;
                if n < 2 {
                    return Some(Err(format!("ISO8 length byte {n} too small")));
                }
                Some(Ok(n + 1))
            }
            WireFormat::Kwp { .. } => {
                let f = *b.first()?;
                if f & 0xC0 != 0x80 {
                    return Some(Err(format!(
                        "format byte {f:#04x} is not physical addressing"
                    )));
                }
                match (f & 0x3F) as usize {
                    0 => {
                        let n = *b.get(3)? as usize;
                        if n == 0 {
                            return Some(Err("length byte 0".into()));
                        }
                        Some(Ok(4 + n + 1))
                    }
                    n => Some(Ok(3 + n + 1)),
                }
            }
        }
    }

    pub fn push(&mut self, byte: u8, fmt: WireFormat) -> Feed {
        self.buf.push(byte);
        let total = match self.total_len(fmt) {
            None => return Feed::More,
            Some(Err(e)) => {
                self.buf.clear();
                return Feed::Bad(e);
            }
            Some(Ok(t)) => t,
        };
        if self.buf.len() < total {
            return Feed::More;
        }
        let frame = std::mem::take(&mut self.buf);
        let (body, cs) = frame.split_at(frame.len() - 1);
        if checksum(body) != cs[0] {
            return Feed::Bad(format!(
                "checksum {:#04x}, expected {:#04x} in {frame:02X?}",
                cs[0],
                checksum(body)
            ));
        }
        Feed::Frame(match fmt {
            WireFormat::Iso8 => Parsed {
                target: None,
                source: None,
                payload: body[1..].to_vec(),
            },
            WireFormat::Kwp { .. } => {
                let start = if body[0] & 0x3F == 0 { 4 } else { 3 };
                Parsed {
                    target: Some(body[1]),
                    source: Some(body[2]),
                    payload: body[start..].to_vec(),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kwp_examples_from_protocol_doc() {
        // docs/protocol.md: `21 A3` to the BMT -> `82 26 F1 21 A3 5D`.
        let mut a = Assembler::default();
        let fmt = WireFormat::Kwp {
            length_in_format: true,
        };
        let mut last = Feed::More;
        for b in [0x82, 0x26, 0xF1, 0x21, 0xA3, 0x5D] {
            last = a.push(b, fmt);
        }
        assert_eq!(
            last,
            Feed::Frame(Parsed {
                target: Some(0x26),
                source: Some(0xF1),
                payload: vec![0x21, 0xA3]
            })
        );
        // Reply direction: tgt = tester, src = ECU.
        assert_eq!(
            encode_reply(fmt, 0x26, 0xF1, &[0x7E]),
            vec![0x81, 0xF1, 0x26, 0x7E, 0x16]
        );
        assert_eq!(
            encode_reply(WireFormat::Iso8, 0, 0, &[0x36]),
            vec![0x02, 0x36, 0x38]
        );
    }

    #[test]
    fn separate_length_byte_and_bad_checksum() {
        let fmt = WireFormat::Kwp {
            length_in_format: false,
        };
        let f = encode_reply(fmt, 0x7A, 0xF1, &[0x61, 0xA0]);
        assert_eq!(f[..4], [0x80, 0xF1, 0x7A, 0x02]);
        let mut a = Assembler::default();
        let res: Vec<Feed> = f.iter().map(|&b| a.push(b, fmt)).collect();
        assert!(matches!(res.last(), Some(Feed::Frame(p)) if p.payload == [0x61, 0xA0]));
        let mut bad = f.clone();
        *bad.last_mut().unwrap() ^= 1;
        let res: Vec<Feed> = bad.iter().map(|&b| a.push(b, fmt)).collect();
        assert!(matches!(res.last(), Some(Feed::Bad(_))));
    }
}
