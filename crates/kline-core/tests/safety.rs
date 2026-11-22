//! Tiered safety policy on raw bytes (no Renault files needed).

use kline_core::safety::{
    assess, check, classify, config_backup_read, tier_of, DenyReason, Protocol, Target, Tier,
    Warning,
};

const SIRIUS: Target = Target {
    protocol: Protocol::Kwp,
    ecu_address: 0x7A,
};
const BMT: Target = Target {
    protocol: Protocol::Kwp,
    ecu_address: 0x26,
};
const SAFIR: Target = Target {
    protocol: Protocol::Iso8,
    ecu_address: 0x7A,
};

fn reason(t: &Target, p: &[u8]) -> Option<DenyReason> {
    classify(t, p).err().map(|d| {
        assert_eq!(d.payload, p);
        d.reason
    })
}

fn tier(t: &Target, p: &[u8]) -> Tier {
    classify(t, p).unwrap_or_else(|d| panic!("{p:02X?}: {d}"))
}

#[test]
fn kwp_reads_are_tier0() {
    for t in [SIRIUS, BMT] {
        for p in [
            &[0x3E][..],
            &[0x3E, 0x01],
            &[0x10, 0x81],
            &[0x10, 0xA0],
            &[0x1A, 0x80],
            &[0x17, 0xFF, 0x00],
            &[0x18, 0x02, 0xFF, 0x00],
            &[0x21, 0x80],
            &[0x21, 0xA0],
            &[0x21, 0xA1],
            // Policy v2: BMT transponder status read is allowed (read-only).
            &[0x21, 0xA4],
        ] {
            assert_eq!(tier(&t, p), Tier::Read, "{p:02X?}");
            assert_eq!(check(&t, p).unwrap().payload(), p);
        }
    }
}

#[test]
fn reads_need_exactly_one_identifier() {
    // Gemini M2 #3, catalogue: BMT `21 A1 xx` writes CALTERMI as a side effect.
    for p in [
        &[0x21][..],
        &[0x1A],
        &[0x21, 0xA1, 0x00],
        &[0x21, 0xA1, 0x01],
        &[0x21, 0xA0, 0x00],
        &[0x1A, 0x80, 0x00],
        &[0x17],
        &[0x18],
        &[0x3E, 0x01, 0x00],
    ] {
        assert!(
            matches!(
                reason(&BMT, p),
                Some(DenyReason::ParametersNotAllowed { .. })
            ),
            "{p:02X?}"
        );
        // Undocumented: only as an expert request.
        assert_eq!(tier_of(&BMT, p).unwrap(), Tier::Expert, "{p:02X?}");
    }
    // `21 A4` plus anything is refused outright on the BMT (transponder).
    assert_eq!(
        reason(&BMT, &[0x21, 0xA4, 0x00]),
        Some(DenyReason::Immobilizer)
    );
}

#[test]
fn only_default_and_renault_sessions() {
    for p in [
        &[0x10, 0x85][..],
        &[0x10, 0x86],
        &[0x10, 0xB0],
        &[0x10, 0xC0],
        &[0x10, 0x89],
        &[0x10, 0x00],
        &[0x10],
        &[0x10, 0x81, 0x00],
    ] {
        assert_eq!(
            reason(&SIRIUS, p),
            Some(DenyReason::Forbidden { service: 0x10 }),
            "{p:02X?}"
        );
        // Not even in expert mode.
        assert!(tier_of(&SIRIUS, p).is_err());
    }
}

#[test]
fn protocol_frames_are_reserved() {
    for (t, p) in [
        (SIRIUS, 0x81u8),
        (SIRIUS, 0x82),
        (BMT, 0x82),
        (SAFIR, 0x36),
        (SAFIR, 0x3F),
    ] {
        assert_eq!(
            reason(&t, &[p]),
            Some(DenyReason::ProtocolReserved { service: p })
        );
    }
}

#[test]
fn tier1_service_actions() {
    for (t, p) in [
        (SIRIUS, &[0x14, 0x00, 0x00][..]),
        (BMT, &[0x14, 0xFF, 0x00]),
        (SIRIUS, &[0x30, 0xD0, 0x00, 0x0A]),
        (SIRIUS, &[0x30, 0xD0, 0x01, 0x00]),
        (SIRIUS, &[0x11, 0xFF]),
        (SIRIUS, &[0x32, 0xA0, 0x00]),
        (SIRIUS, &[0x31, 0xA0, 0xA0]),
        (BMT, &[0x31, 0xC7, 0x00, 0x00, 0x00]),
        (BMT, &[0x31, 0xC8, 0x00, 0x00, 0x00]),
        (BMT, &[0x31, 0xA0, 0x00, 0x01]),
        (BMT, &[0x31, 0xB3, 0x00, 0x01]),
    ] {
        assert_eq!(tier(&t, p), Tier::Service, "{p:02X?}");
        assert_eq!(
            check(&t, p).unwrap_err().reason,
            DenyReason::ApprovalRequired {
                tier: Tier::Service
            }
        );
    }
    let r = assess(&SIRIUS, &[0x31, 0xA0, 0xA0]).unwrap();
    assert_eq!(r.tier, Tier::Service);
    assert_eq!(r.warnings, &[Warning::StopsEngine]);
    assert!(r.reversible);
    let r = assess(&SIRIUS, &[0x14, 0x00, 0x00]).unwrap();
    assert_eq!(r.warnings, &[Warning::ErasesFaultMemory]);
    assert!(!r.reversible, "clearing DTCs erases evidence");
    let r = assess(&BMT, &[0x31, 0xC7, 0, 0, 0]).unwrap();
    assert_eq!(r.warnings, &[Warning::ErasesRemotes]);
    assert!(!r.reversible);
    // The BMT actuator with output A0 is not the Sirius engine stop.
    let r = assess(&BMT, &[0x31, 0xA0, 0x00, 0x01]).unwrap();
    assert_eq!(r.warnings, &[Warning::MovesActuator]);
}

#[test]
fn tier2_config_writes() {
    for (t, p) in [
        (SIRIUS, &[0x3B, 0xD0, 0x10][..]),
        (BMT, &[0x3B, 0xD2, 0x00, 0x00]),
        (BMT, &[0x3B, 0xD2, 0x07, 0x01]),
        (BMT, &[0x3B, 0xD2, 0x0D, 0x01]),
    ] {
        assert_eq!(tier(&t, p), Tier::ConfigWrite, "{p:02X?}");
        assert_eq!(
            check(&t, p).unwrap_err().reason,
            DenyReason::ApprovalRequired {
                tier: Tier::ConfigWrite
            }
        );
    }
    // Backup read: BMT config is read with 21 A1, other ECUs with 21 lid.
    assert_eq!(
        config_backup_read(&BMT, &[0x3B, 0xD2, 0x07, 0x01]),
        Some([0x21, 0xA1])
    );
    assert_eq!(
        config_backup_read(&SIRIUS, &[0x3B, 0xD0, 0x10]),
        Some([0x21, 0xD0])
    );
    let r = assess(&BMT, &[0x3B, 0xD2, 0x07, 0x01]).unwrap();
    assert_eq!(r.warnings, &[Warning::EepromWrite]);
    assert!(r.reversible);
    assert!(r.prerequisites.iter().any(|p| p.contains("21 A1")));
    assert!(r.prerequisites.iter().any(|p| p.contains("21 80")));
    // BMT: reserved indices 14/15, values other than 0/1, other lengths: undocumented, expert.
    for p in [
        &[0x3B, 0xD2, 0x0E, 0x00][..],
        &[0x3B, 0xD2, 0x0F, 0x01],
        &[0x3B, 0xD2, 0x00, 0x02],
        &[0x3B, 0xD2, 0x00],
        &[0x3B, 0xD2, 0x00, 0x00, 0x00],
    ] {
        assert_eq!(
            reason(&BMT, p),
            Some(DenyReason::ParametersNotAllowed { service: 0x3B }),
            "{p:02X?}"
        );
        let r = assess(&BMT, p).unwrap();
        assert_eq!(r.tier, Tier::Expert);
        assert_eq!(r.warnings, &[Warning::UnknownEffect]);
    }
}

#[test]
fn immobilizer_is_forbidden_always() {
    // BMT immobilizer routine functions and anything resembling them, every length.
    for f in [0xC0u8, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6] {
        for p in [
            vec![0x31, f, 0x00, 0x00, 0x00],
            vec![0x31, f, 0x00, 0x00],
            vec![0x31, f],
            vec![0x30, f, 0x00, 0x01],
            vec![0x3B, f, 0x00],
            vec![0x2E, f, 0x00],
            vec![0x32, f],
        ] {
            assert_eq!(reason(&BMT, &p), Some(DenyReason::Immobilizer), "{p:02X?}");
            // Also when the target claims ISO8 (security review M2 F3b protocol confusion).
            let iso = Target {
                protocol: Protocol::Iso8,
                ecu_address: 0x26,
            };
            assert_eq!(reason(&iso, &p), Some(DenyReason::Immobilizer), "{p:02X?}");
        }
    }
    // BMT 31 C7/C8 in any other form than the two exact routines.
    for p in [
        &[0x31, 0xC7, 0x00, 0x00][..],
        &[0x31, 0xC8, 0x00, 0x00, 0x00, 0x00],
        &[0x31, 0xC7, 0, 0, 1],
    ] {
        assert_eq!(reason(&BMT, p), Some(DenyReason::Immobilizer), "{p:02X?}");
    }
    // BMT config: index 2 is AP1CLE (key related); any other 3B identifier, and any 2E/2F,
    // cannot be shown not to touch immobilizer config.
    for p in [
        &[0x3B, 0xD2, 0x02, 0x00][..],
        &[0x3B, 0xD2, 0x02],
        &[0x3B, 0xD0, 0x10],
        &[0x3B, 0x01, 0x00],
        &[0x3B],
        &[0x2E, 0xD2, 0x00],
        &[0x2F, 0x01, 0x00],
    ] {
        assert_eq!(reason(&BMT, p), Some(DenyReason::Immobilizer), "{p:02X?}");
    }
    // Sirius writes to frames that carry immobilizer / verrou logiciel fields.
    for lid in [0xA0u8, 0xA2, 0xAE, 0xBD] {
        for s in [0x2Eu8, 0x2F, 0x3B] {
            assert_eq!(
                reason(&SIRIUS, &[s, lid, 0x00]),
                Some(DenyReason::Immobilizer),
                "{s:02X} {lid:02X}"
            );
        }
        // Reading them is fine.
        assert_eq!(tier(&SIRIUS, &[0x21, lid]), Tier::Read);
    }
    // Sirius 31 A0 A3: reset of the backed-up memory, may touch the verrou logiciel area.
    assert_eq!(
        reason(&SIRIUS, &[0x31, 0xA0, 0xA3]),
        Some(DenyReason::Immobilizer)
    );
    assert_eq!(
        reason(&SIRIUS, &[0x31, 0xA0, 0xA3, 0x00]),
        Some(DenyReason::Immobilizer)
    );
}

#[test]
fn forbidden_even_for_experts() {
    for (t, p, service) in [
        (SIRIUS, &[0x27, 0x01][..], 0x27),
        (BMT, &[0x27, 0x01], 0x27),
        (SAFIR, &[0x27, 0x01], 0x27),
        (SIRIUS, &[0x34], 0x34),
        (SIRIUS, &[0x35, 0x00], 0x35),
        (SIRIUS, &[0x36], 0x36),
        (SIRIUS, &[0x37], 0x37),
        (SAFIR, &[0x34], 0x34),
        (SAFIR, &[0x35], 0x35),
        (SAFIR, &[0x37], 0x37),
    ] {
        assert_eq!(
            reason(&t, p),
            Some(DenyReason::Forbidden { service }),
            "{p:02X?}"
        );
        assert!(tier_of(&t, p).is_err());
        assert!(assess(&t, p).is_err());
    }
}

#[test]
fn undocumented_requests_are_expert_only() {
    for (t, p, service) in [
        (SIRIUS, &[0x2E, 0x01, 0x00][..], 0x2E),
        (SIRIUS, &[0x2F, 0x01], 0x2F),
        (SIRIUS, &[0x20, 0x00], 0x20),
        (SIRIUS, &[0x23, 0x00, 0x00], 0x23),
        (SIRIUS, &[0x3D, 0x00], 0x3D),
        (SIRIUS, &[0x2C, 0x00], 0x2C),
        (SIRIUS, &[0x40, 0x02, 0x10], 0x40),
        (SIRIUS, &[0x61, 0x02, 0x01], 0x61),
        (SIRIUS, &[0xFF, 0x00], 0xFF),
        (SIRIUS, &[0x33, 0x50], 0x33),
    ] {
        assert_eq!(
            reason(&t, p),
            Some(DenyReason::ServiceNotAllowed { service }),
            "{p:02X?}"
        );
        assert_eq!(tier_of(&t, p).unwrap(), Tier::Expert);
        assert!(check(&t, p).is_err());
    }
    // Routines and parameters that are not explicitly listed (no routine discovery without
    // an expert approval).
    for (t, p) in [
        (SIRIUS, &[0x31, 0xA0, 0x00][..]),
        (SIRIUS, &[0x31, 0xA0, 0xA1]),
        (SIRIUS, &[0x31, 0xA0, 0xA2]),
        (SIRIUS, &[0x31, 0xA0, 0xA0, 0x00]),
        (SIRIUS, &[0x31, 0xA4, 0x00]),
        (SIRIUS, &[0x31, 0xA2, 0x01, 0x00, 0x00]),
        (SIRIUS, &[0x31, 0xC7, 0x00, 0x00, 0x00]),
        (BMT, &[0x31, 0x00, 0x00, 0x00, 0x00]),
        (BMT, &[0x31, 0x00, 0x00, 0x00]),
        (BMT, &[0x31, 0xB4, 0x00, 0x01]),
        (BMT, &[0x31, 0x9F, 0x00, 0x01]),
        (BMT, &[0x31, 0xA0, 0x00]),
        (SIRIUS, &[0x11, 0x01]),
        (SIRIUS, &[0x11, 0xFF, 0x00]),
        (SIRIUS, &[0x14, 0xFF]),
    ] {
        assert!(
            matches!(reason(&t, p), Some(DenyReason::ParametersNotAllowed { .. })),
            "{t:?} {p:02X?}"
        );
        assert_eq!(tier_of(&t, p).unwrap(), Tier::Expert);
    }
    assert_eq!(reason(&SIRIUS, &[]), Some(DenyReason::Empty));
    assert!(tier_of(&SIRIUS, &[]).is_err());
}

#[test]
fn iso8_allowlist_matches_safir_definitions() {
    for p in [
        &[0x21][..],
        &[0x33, 0x50],
        &[0x33, 0x51],
        &[0x33, 0x52],
        &[0x33, 0x54],
    ] {
        assert_eq!(tier(&SAFIR, p), Tier::Read, "{p:02X?}");
    }
    for p in [
        // Not in Safir_1/Safir_2 (security review policy drift).
        &[0x33, 0x53][..],
        &[0x21, 0x05],
        &[0x33, 0x55],
        &[0x33, 0x4F],
        &[0x33],
        &[0x33, 0x50, 0x00],
        &[0x36, 0x00],
        &[0x31, 0x52],
        &[0x32, 0x51],
        &[0x40, 0x02, 0x10],
        &[0x41, 0x01, 0x00, 0x00, 0x00],
        &[0x45],
        &[0x46, 0xFF],
        &[0x20, 0x00],
        &[0x61, 0x02, 0x01],
        &[0xFF, 0x00],
        &[0x81],
        &[0x3E],
        &[0x10, 0x81],
        &[0x14, 0xFF, 0x00],
        &[0x3B, 0xD0, 0x10],
    ] {
        assert!(classify(&SAFIR, p).is_err(), "{p:02X?}");
    }
}

/// Gemini re-review of M2, finding 1: the Sirius immobilizer rules and the session rule do not
/// depend on the protocol of the target.
#[test]
fn sirius_immobilizer_and_session_rules_hold_under_iso8() {
    let iso8_7a = Target {
        protocol: Protocol::Iso8,
        ecu_address: 0x7A,
    };
    for p in [
        &[0x31, 0xA0, 0xA3][..],
        &[0x3B, 0xAE, 0x01],
        &[0x2E, 0xA0, 0x00],
        &[0x2F, 0xBD, 0x00],
    ] {
        assert_eq!(
            reason(&iso8_7a, p),
            Some(DenyReason::Immobilizer),
            "{p:02X?}"
        );
        assert!(tier_of(&iso8_7a, p).is_err());
    }
    for p in [&[0x10, 0x85][..], &[0x10, 0xC0], &[0x10]] {
        assert_eq!(
            reason(&iso8_7a, p),
            Some(DenyReason::Forbidden { service: 0x10 }),
            "{p:02X?}"
        );
    }
}

/// Re-review finding 2: only documented `30`/`11`/`32` are Tier 1, and only off the BMT.
#[test]
fn undocumented_service_requests_are_expert() {
    for (t, p) in [
        (SIRIUS, &[0x32, 0xA1, 0x00][..]),
        (SIRIUS, &[0x32, 0xA0]),
        (SIRIUS, &[0x32, 0xA0, 0x00, 0x00]),
        (BMT, &[0x32, 0xA0, 0x00]),
        (BMT, &[0x30, 0xD0, 0x00, 0x0A]),
        (BMT, &[0x11, 0xFF]),
    ] {
        assert!(classify(&t, p).is_err(), "{t:?} {p:02X?}");
        let r = assess(&t, p).unwrap();
        assert_eq!(r.tier, Tier::Expert, "{t:?} {p:02X?}");
        assert_eq!(r.warnings, &[Warning::UnknownEffect]);
        assert!(!r.reversible);
    }
    assert_eq!(tier(&SIRIUS, &[0x32, 0xA0, 0x00]), Tier::Service);
    // Finding 2c: the non-BMT 3B RiskInfo names the 21 80 prerequisite the session enforces.
    let r = assess(&SIRIUS, &[0x3B, 0xD0, 0x10]).unwrap();
    assert!(
        r.prerequisites.iter().any(|p| p.contains("21 80")),
        "{:?}",
        r.prerequisites
    );
    // Finding 3: no run of spaces in any RiskInfo text.
    let r = assess(&BMT, &[0x3B, 0xD2, 0x07, 0x01]).unwrap();
    assert!(
        !r.what_can_go_wrong.contains("  "),
        "{}",
        r.what_can_go_wrong
    );
}
