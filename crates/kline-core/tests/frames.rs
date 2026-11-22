//! Byte-exact framing, checksum and length variants, reply classification, echo stripping.

use kline_core::echo::{strip_echo, EchoError};
use kline_core::frame::{checksum, FrameError, Framing, KwpHeader, Reply, ReplyNeed};

fn kwp(target: u8) -> Framing {
    Framing::Kwp(KwpHeader {
        target,
        source: 0xF1,
        length_in_format: true,
    })
}

#[test]
fn iso8_examples() {
    assert_eq!(
        Framing::Iso8.encode(&[0x33, 0x50]).unwrap(),
        [0x03, 0x33, 0x50, 0x86]
    );
    assert_eq!(Framing::Iso8.encode(&[0x36]).unwrap(), [0x02, 0x36, 0x38]);
    assert_eq!(Framing::Iso8.encode(&[0x3F]).unwrap(), [0x02, 0x3F, 0x41]);
}

#[test]
fn kwp_examples() {
    // BMT 21A3.
    assert_eq!(
        kwp(0x26).encode(&[0x21, 0xA3]).unwrap(),
        [0x82, 0x26, 0xF1, 0x21, 0xA3, 0x5D]
    );
    assert_eq!(
        kwp(0x26).encode(&[0x3E]).unwrap(),
        [0x81, 0x26, 0xF1, 0x3E, 0xD6]
    );
    assert_eq!(
        kwp(0x26).encode(&[0x82]).unwrap(),
        [0x81, 0x26, 0xF1, 0x82, 0x1A]
    );
    // Sirius 32 at 0x7A: 21A0 and StartCommunication.
    assert_eq!(
        kwp(0x7A).encode(&[0x21, 0xA0]).unwrap(),
        [0x82, 0x7A, 0xF1, 0x21, 0xA0, 0xAE]
    );
    assert_eq!(
        kwp(0x7A).encode(&[0x81]).unwrap(),
        [0x81, 0x7A, 0xF1, 0x81, 0x6D]
    );
}

#[test]
fn kwp_separate_length_byte() {
    // n > 0x3F always uses the length byte.
    let payload: Vec<u8> = (0..0x40).collect();
    let f = kwp(0x26).encode(&payload).unwrap();
    assert_eq!(&f[..4], &[0x80, 0x26, 0xF1, 0x40]);
    assert_eq!(f.len(), 4 + 0x40 + 1);
    assert_eq!(*f.last().unwrap(), checksum(&f[..f.len() - 1]));
    // n = 0x3F still fits in the format byte.
    let payload: Vec<u8> = (0..0x3F).collect();
    assert_eq!(kwp(0x26).encode(&payload).unwrap()[0], 0xBF);
    // KW1 without length-in-format support: length byte even for short frames.
    let h = Framing::Kwp(KwpHeader {
        target: 0x26,
        source: 0xF1,
        length_in_format: false,
    });
    assert_eq!(
        h.encode(&[0x21, 0xA3]).unwrap(),
        [0x80, 0x26, 0xF1, 0x02, 0x21, 0xA3, 0x5D]
    );
}

#[test]
fn length_in_format_rule() {
    assert!(KwpHeader::length_in_format_for_kw1(0xD0));
    assert!(KwpHeader::length_in_format_for_kw1(0x8F));
    assert!(!KwpHeader::length_in_format_for_kw1(0xC2));
}

#[test]
fn encode_limits() {
    assert_eq!(Framing::Iso8.encode(&[]), Err(FrameError::EmptyPayload));
    assert_eq!(kwp(0x26).encode(&[]), Err(FrameError::EmptyPayload));
    assert!(Framing::Iso8.encode(&[0; 254]).is_ok());
    assert_eq!(
        Framing::Iso8.encode(&[0; 255]),
        Err(FrameError::PayloadTooLong { len: 255, max: 254 })
    );
    assert!(kwp(0x26).encode(&[0; 255]).is_ok());
    assert_eq!(
        kwp(0x26).encode(&[0; 256]),
        Err(FrameError::PayloadTooLong { len: 256, max: 255 })
    );
}

#[test]
fn decode_kwp_reply() {
    // ECU -> tool: 83 F1 26 61 A3 01 cs
    let mut f = vec![0x83, 0xF1, 0x26, 0x61, 0xA3, 0x01];
    f.push(checksum(&f));
    assert_eq!(kwp(0x26).decode(&f).unwrap(), &[0x61, 0xA3, 0x01]);
    // Separate length byte.
    let mut g = vec![0x80, 0xF1, 0x26, 0x03, 0x61, 0xA3, 0x01];
    g.push(checksum(&g));
    assert_eq!(kwp(0x26).decode(&g).unwrap(), &[0x61, 0xA3, 0x01]);
}

#[test]
fn decode_kwp_errors() {
    let good = |v: &[u8]| {
        let mut f = v.to_vec();
        f.push(checksum(&f));
        f
    };
    let mut bad_cs = good(&[0x82, 0xF1, 0x26, 0x61, 0xA3]);
    *bad_cs.last_mut().unwrap() ^= 1;
    assert!(matches!(
        kwp(0x26).decode(&bad_cs),
        Err(FrameError::Checksum { .. })
    ));
    assert_eq!(
        kwp(0x26).decode(&good(&[0x82, 0xF0, 0x26, 0x61, 0xA3])),
        Err(FrameError::ToolAddress {
            expected: 0xF1,
            actual: 0xF0
        })
    );
    assert_eq!(
        kwp(0x26).decode(&good(&[0x82, 0xF1, 0x7A, 0x61, 0xA3])),
        Err(FrameError::EcuAddress {
            expected: 0x26,
            actual: 0x7A
        })
    );
    assert_eq!(
        kwp(0x26).decode(&good(&[0x83, 0xF1, 0x26, 0x61, 0xA3])),
        Err(FrameError::LengthMismatch {
            declared: 3,
            actual: 2
        })
    );
    assert_eq!(
        kwp(0x26).decode(&good(&[0x80, 0xF1, 0x26, 0x03, 0x61, 0xA3])),
        Err(FrameError::LengthMismatch {
            declared: 3,
            actual: 2
        })
    );
    assert_eq!(
        kwp(0x26).decode(&good(&[0x02, 0xF1, 0x26, 0x61, 0xA3])),
        Err(FrameError::UnsupportedFormat(0x02))
    );
    assert_eq!(
        kwp(0x26).decode(&[0x81, 0xF1, 0x26, 0x98]),
        Err(FrameError::TooShort { len: 4 })
    );
}

#[test]
fn decode_iso8() {
    let f = [0x03, 0x73, 0x50, 0xC6];
    assert_eq!(Framing::Iso8.decode(&f).unwrap(), &[0x73, 0x50]);
    assert_eq!(
        Framing::Iso8.decode(&[0x03, 0x73, 0x50, 0xC7]),
        Err(FrameError::Checksum {
            expected: 0xC6,
            actual: 0xC7
        })
    );
    assert_eq!(
        Framing::Iso8.decode(&[0x04, 0x73, 0x50, 0xC7]),
        Err(FrameError::LengthMismatch {
            declared: 4,
            actual: 3
        })
    );
    assert_eq!(
        Framing::Iso8.decode(&[0x01, 0x01]),
        Err(FrameError::TooShort { len: 2 })
    );
}

#[test]
fn reply_need() {
    assert_eq!(
        Framing::Iso8.need_after_first(0x0F),
        Ok(ReplyNeed::Bytes(15))
    );
    assert!(Framing::Iso8.need_after_first(0x01).is_err());
    assert_eq!(kwp(0x26).need_after_first(0x83), Ok(ReplyNeed::Bytes(6)));
    assert_eq!(
        kwp(0x26).need_after_first(0x80),
        Ok(ReplyNeed::HeaderWithLength)
    );
    assert_eq!(
        kwp(0x26).need_after_first(0x03),
        Err(FrameError::UnsupportedFormat(0x03))
    );
    assert_eq!(Framing::need_after_length(0x40), Ok(0x41));
    assert!(Framing::need_after_length(0).is_err());
}

#[test]
fn classify_replies() {
    let k = kwp(0x26);
    assert_eq!(
        Reply::classify(&k, &[0x7F, 0x21, 0x12]),
        Ok(Reply::Negative {
            sid: 0x21,
            nrc: 0x12
        })
    );
    assert_eq!(
        Reply::classify(&k, &[0x7F, 0x31, 0x78]),
        Ok(Reply::ResponsePending { sid: 0x31 })
    );
    assert_eq!(
        Reply::classify(&k, &[0x7F, 0x31]),
        Err(FrameError::MalformedNegative)
    );
    assert_eq!(
        Reply::classify(&k, &[0x61, 0xA3]),
        Ok(Reply::Positive(vec![0x61, 0xA3]))
    );
    assert_eq!(
        Reply::classify(&Framing::Iso8, &[0x37]),
        Ok(Reply::Iso8Status(0x37))
    );
    // Only ISO8 has single-byte status replies.
    assert_eq!(
        Reply::classify(&k, &[0x37]),
        Ok(Reply::Positive(vec![0x37]))
    );
}

#[test]
fn echo_stripping() {
    let sent = [0x82, 0x26, 0xF1, 0x21, 0xA3, 0x5D];
    let mut rx = sent.to_vec();
    rx.extend_from_slice(&[0x83, 0xF1]);
    assert_eq!(strip_echo(&sent, &rx), Ok(&[0x83, 0xF1][..]));
    // One leading break residue byte.
    let mut rx0 = vec![0x00];
    rx0.extend_from_slice(&sent);
    assert_eq!(strip_echo(&sent, &rx0), Ok(&[][..]));
    // Only one residue byte is tolerated.
    let mut rx00 = vec![0x00, 0x00];
    rx00.extend_from_slice(&sent);
    assert_eq!(strip_echo(&sent, &rx00), Err(EchoError::Mismatch));
    // Short echo, with and without residue.
    assert_eq!(strip_echo(&sent, &sent[..3]), Err(EchoError::Incomplete));
    assert_eq!(
        strip_echo(&sent, &[0x00, 0x82, 0x26]),
        Err(EchoError::Incomplete)
    );
    // Collision.
    assert_eq!(
        strip_echo(&sent, &[0x82, 0x26, 0xF0, 0x21, 0xA3, 0x5D]),
        Err(EchoError::Mismatch)
    );
    // A frame that starts with 0x00 matches exactly.
    assert_eq!(
        strip_echo(&[0x00, 0x01], &[0x00, 0x01, 0x09]),
        Ok(&[0x09][..])
    );
}
