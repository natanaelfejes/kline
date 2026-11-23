//! Exhaustive tier matrix (review of the Bosch 5.3 ABS branch, finding 4): every 1- and 2-byte
//! payload, both protocols, the special addresses (ABS 0x01, BMT 0x26/0xA6, airbag 0x2C, Sirius
//! 0x7A) and a generic one (0x40), with several key bytes. Outside the Bosch 5.3 ABS (ISO8 at
//! 0x01 with key bytes DC 83) the tiers must equal the policy before the ABS rules existed,
//! except the safety-system refusals the owner decided on 2026-11-23 ([`owner_forbidden`]),
//! which are checked on their own.

use kline_core::safety::{assess, tier_of, Denied, DenyReason, Protocol, Target, Tier};

/// Tier of a payload as one byte; the forbidden reasons that existed before the ABS rules have
/// their own codes, any other refusal is 14.
fn code(r: Result<Tier, Denied>) -> u8 {
    match r {
        Ok(Tier::Read) => 0,
        Ok(Tier::Service) => 1,
        Ok(Tier::ConfigWrite) => 2,
        Ok(Tier::Expert) => 3,
        Err(d) => match d.reason {
            DenyReason::Empty => 10,
            DenyReason::Immobilizer => 11,
            DenyReason::ImmobilizerNamed { .. } => 12,
            DenyReason::ProtocolReserved { .. } => 13,
            _ => 14,
        },
    }
}

/// The 1- and 2-byte payloads the owner decision of 2026-11-23 forbids on a safety-system ECU
/// (written out here independently of the library): on the airbag ECU 0x2C (any protocol)
/// every `14` and `30`, every `10` except `10 81`, `11 FF`, and `3B D0`/`D2`/`D3`/`D6`/`D7`/
/// `E0`; on the ISO8 unit at 0x01 (any key bytes) `11 FF`.
fn owner_forbidden(t: &Target, p: &[u8]) -> bool {
    match t.ecu_address {
        0x2C => {
            matches!(p[0], 0x14 | 0x30)
                || (p[0] == 0x10 && p != [0x10, 0x81])
                || p == [0x11, 0xFF]
                || (p[0] == 0x3B
                    && matches!(p.get(1), Some(0xD0 | 0xD2 | 0xD3 | 0xD6 | 0xD7 | 0xE0)))
        }
        0x01 => t.protocol == Protocol::Iso8 && p == [0x11, 0xFF],
        _ => false,
    }
}

/// FNV-1a over the tier codes of all 1-byte payloads, then all 2-byte payloads, in order; a
/// payload of [`owner_forbidden`] counts as code 0xFF whatever its tier.
fn digest(t: &Target) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    let mut one = |p: &[u8]| {
        feed(if owner_forbidden(t, p) {
            0xFF
        } else {
            code(tier_of(t, p))
        })
    };
    for a in 0..=255u8 {
        one(&[a]);
    }
    for a in 0..=255u8 {
        for b in 0..=255u8 {
            one(&[a, b]);
        }
    }
    h
}

const ADDRESSES: [u8; 6] = [0x01, 0x26, 0x2C, 0x40, 0x7A, 0xA6];

/// The same digest computed with the policy of commit 78f9b2f (before the Bosch 5.3 ABS
/// rules; `Target` had no key bytes then), by an identical test (same [`owner_forbidden`])
/// run in a checkout of that commit.
const PRE_BRANCH: [(Protocol, u8, u64); 12] = [
    (Protocol::Iso8, 0x01, 0xf494b970d0a1fd3a),
    (Protocol::Iso8, 0x26, 0x08a6aa874831fd38),
    (Protocol::Iso8, 0x2C, 0xe3dffae212e9b89a),
    (Protocol::Iso8, 0x40, 0x8720e16b0b4c3f36),
    (Protocol::Iso8, 0x7A, 0x3ccc888c5531247e),
    (Protocol::Iso8, 0xA6, 0x08a6aa874831fd38),
    (Protocol::Kwp, 0x01, 0xf94931264a686e72),
    (Protocol::Kwp, 0x26, 0x3c3679eeee5c5234),
    (Protocol::Kwp, 0x2C, 0xfcb3ba54817caedb),
    (Protocol::Kwp, 0x40, 0xf94931264a686e72),
    (Protocol::Kwp, 0x7A, 0xefa0d01c2f80e23a),
    (Protocol::Kwp, 0xA6, 0x3c3679eeee5c5234),
];

const BOSCH_ABS_KW: (u8, u8) = (0xDC, 0x83);

/// Key bytes tried at every target: unknown, the Bosch 5.3 ABS, the BMT/Sirius KWP pair, the
/// Safir pair, and one byte off the ABS pair.
const KEYWORDS: [Option<(u8, u8)>; 5] = [
    None,
    Some(BOSCH_ABS_KW),
    Some((0xD0, 0x8F)),
    Some((0xC2, 0x85)),
    Some((0xDC, 0x84)),
];

fn is_bosch_abs(t: &Target) -> bool {
    t.protocol == Protocol::Iso8 && t.ecu_address == 0x01 && t.keywords == Some(BOSCH_ABS_KW)
}

fn targets() -> impl Iterator<Item = Target> {
    [Protocol::Iso8, Protocol::Kwp]
        .into_iter()
        .flat_map(|protocol| {
            ADDRESSES.into_iter().flat_map(move |ecu_address| {
                KEYWORDS.into_iter().map(move |keywords| Target {
                    protocol,
                    ecu_address,
                    keywords,
                })
            })
        })
}

#[test]
fn tiers_outside_the_bosch_abs_equal_the_pre_branch_policy() {
    let mut checked = 0;
    for t in targets().filter(|t| !is_bosch_abs(t)) {
        let (_, _, want) = PRE_BRANCH
            .iter()
            .find(|(p, a, _)| *p == t.protocol && *a == t.ecu_address)
            .unwrap();
        assert_eq!(digest(&t), *want, "{t:?}");
        checked += 1;
    }
    assert_eq!(checked, 2 * ADDRESSES.len() * KEYWORDS.len() - 1);
}

/// KWP at 0x01 (any key bytes, DC 83 included) and ISO8 at 0x01 without DC 83 get neither the
/// ABS tiers (above) nor the Bosch 5.3 risk texts: their texts equal those of the same payload
/// at a generic address (0x01 only adds the address-wide brake-system note).
#[test]
fn other_units_at_0x01_do_not_get_the_bosch_abs_risk_texts() {
    for t in targets().filter(|t| t.ecu_address == 0x01 && !is_bosch_abs(t)) {
        let generic = Target {
            ecu_address: 0x40,
            ..t
        };
        let mut payloads: Vec<Vec<u8>> = (0..=255u8).map(|a| vec![a]).collect();
        payloads.extend((0..=255u8).flat_map(|a| [vec![a, 0x00], vec![a, 0x04], vec![a, 0xFF]]));
        for p in payloads.into_iter().filter(|p| !owner_forbidden(&t, p)) {
            match (assess(&t, &p), assess(&generic, &p)) {
                (Ok(a), Ok(g)) => {
                    assert_eq!(
                        (a.tier, a.what_it_changes, a.what_can_go_wrong),
                        (g.tier, g.what_it_changes, g.what_can_go_wrong),
                        "{t:?} {p:02X?}"
                    );
                    assert!(!a.what_it_changes.contains("Bosch"), "{t:?} {p:02X?}");
                }
                (Err(a), Err(g)) => assert_eq!(a.reason, g.reason, "{t:?} {p:02X?}"),
                (a, g) => panic!("{t:?} {p:02X?}: {a:?} vs {g:?}"),
            }
        }
    }
}

/// The Bosch 5.3 ABS itself, every 1- and 2-byte payload: Tier 0 is exactly `51` and `58`,
/// `45` is the only Tier 1, every `41`/`71` is forbidden, `36`/`3F` are link frames, the rest
/// is Expert.
#[test]
fn bosch_abs_tier_matrix() {
    let abs = Target {
        protocol: Protocol::Iso8,
        ecu_address: 0x01,
        keywords: Some(BOSCH_ABS_KW),
    };
    let mut payloads: Vec<Vec<u8>> = (0..=255u8).map(|a| vec![a]).collect();
    payloads.extend((0..=255u8).flat_map(|a| (0..=255u8).map(move |b| vec![a, b])));
    let mut reads = Vec::new();
    for p in payloads {
        let got = tier_of(&abs, &p);
        match (p[0], p.len()) {
            (0x41 | 0x71, _) => assert!(
                matches!(&got, Err(d) if matches!(d.reason, DenyReason::BoschAbsRoutine { service, .. } if service == p[0])),
                "{p:02X?}: {got:?}"
            ),
            (0x36 | 0x3F, _) => assert!(
                matches!(&got, Err(d) if d.reason == DenyReason::ProtocolReserved { service: p[0] }),
                "{p:02X?}: {got:?}"
            ),
            (0x45, 1) => assert_eq!(got, Ok(Tier::Service)),
            (0x11, 2) if p[1] == 0xFF => assert!(
                matches!(&got, Err(d) if matches!(d.reason, DenyReason::SafetySystemForbidden { .. })),
                "{p:02X?}: {got:?}"
            ),
            _ => match got {
                Ok(Tier::Read) => reads.push(p),
                Ok(Tier::Expert) => {}
                other => panic!("{p:02X?}: {other:?}"),
            },
        }
    }
    assert_eq!(reads, [vec![0x51], vec![0x58]]);
}

/// Every payload of [`owner_forbidden`] is refused as a safety-system request at every key
/// bytes; nothing else at those targets is (the digests above cover the rest).
#[test]
fn owner_forbidden_payloads_are_refused_as_safety_system_requests() {
    let mut n = 0;
    for t in targets() {
        for a in 0..=255u8 {
            for p in [vec![a]].into_iter().chain((0..=255u8).map(|b| vec![a, b])) {
                let got = tier_of(&t, &p);
                let safety = matches!(&got, Err(d) if matches!(d.reason, DenyReason::SafetySystemForbidden { .. }));
                assert_eq!(safety, owner_forbidden(&t, &p), "{t:?} {p:02X?}: {got:?}");
                n += usize::from(safety);
            }
        }
    }
    assert!(n > 0);
}
