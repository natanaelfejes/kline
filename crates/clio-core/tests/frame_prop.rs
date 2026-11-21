//! Property tests: encode/decode round trip and reply-length rules, both framings.

use clio_core::echo::strip_echo;
use clio_core::frame::{checksum, Framing, KwpHeader, ReplyNeed};
use proptest::prelude::*;

/// Total frame length predicted by the reply-length rules from the first bytes.
fn predicted_len(framing: &Framing, frame: &[u8]) -> usize {
    match framing.need_after_first(frame[0]).unwrap() {
        ReplyNeed::Bytes(n) => 1 + n,
        ReplyNeed::HeaderWithLength => 4 + Framing::need_after_length(frame[3]).unwrap(),
    }
}

fn kwp_strategy() -> impl Strategy<Value = Framing> {
    (any::<u8>(), any::<u8>(), any::<bool>()).prop_map(|(t, s, l)| {
        Framing::Kwp(KwpHeader {
            target: t,
            source: s,
            length_in_format: l,
        })
    })
}

proptest! {
    #[test]
    fn iso8_round_trip(payload in prop::collection::vec(any::<u8>(), 1..=254)) {
        let f = Framing::Iso8.encode(&payload).unwrap();
        prop_assert_eq!(f.len(), payload.len() + 2);
        prop_assert_eq!(*f.last().unwrap(), checksum(&f[..f.len() - 1]));
        prop_assert_eq!(Framing::Iso8.decode(&f).unwrap(), &payload[..]);
        prop_assert_eq!(predicted_len(&Framing::Iso8, &f), f.len());
    }

    #[test]
    fn kwp_round_trip(framing in kwp_strategy(), payload in prop::collection::vec(any::<u8>(), 1..=255)) {
        // A reply travels the other way: build it with the reversed header.
        let f = framing.reversed().encode(&payload).unwrap();
        prop_assert_eq!(*f.last().unwrap(), checksum(&f[..f.len() - 1]));
        prop_assert_eq!(framing.decode(&f).unwrap(), &payload[..]);
        prop_assert_eq!(predicted_len(&framing, &f), f.len());
        let separate = f[0] & 0x3F == 0;
        let Framing::Kwp(h) = framing else { unreachable!() };
        prop_assert_eq!(separate, payload.len() > 0x3F || !h.length_in_format);
    }

    #[test]
    fn corrupted_byte_is_rejected(payload in prop::collection::vec(any::<u8>(), 1..=60), pos in any::<prop::sample::Index>(), flip in 1u8..=255) {
        let framing = Framing::Kwp(KwpHeader { target: 0x7A, source: 0xF1, length_in_format: true });
        let mut f = framing.reversed().encode(&payload).unwrap();
        let i = pos.index(f.len());
        f[i] ^= flip;
        prop_assert!(framing.decode(&f).is_err());
        let mut g = Framing::Iso8.encode(&payload).unwrap();
        let j = pos.index(g.len());
        g[j] ^= flip;
        prop_assert!(Framing::Iso8.decode(&g).is_err());
    }

    #[test]
    fn echo_strip_round_trip(sent in prop::collection::vec(any::<u8>(), 1..40), rest in prop::collection::vec(any::<u8>(), 0..20), residue in any::<bool>()) {
        // A frame starting with 0x00 plus residue is ambiguous; strip_echo prefers the exact match.
        prop_assume!(!(residue && sent[0] == 0));
        let mut rx = Vec::new();
        if residue { rx.push(0); }
        rx.extend_from_slice(&sent);
        rx.extend_from_slice(&rest);
        prop_assert_eq!(strip_echo(&sent, &rx).unwrap(), &rest[..]);
    }
}
