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

/// Owner decision 2026-11-22 (final): other sessions are Expert, with a programming-session
/// warning, not forbidden.
#[test]
fn only_default_and_renault_sessions_are_reads() {
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
            Some(DenyReason::ParametersNotAllowed { service: 0x10 }),
            "{p:02X?}"
        );
        let r = assess(&SIRIUS, p).unwrap();
        assert_eq!(r.tier, Tier::Expert);
        assert!(r.warnings.contains(&Warning::MayLeaveProgrammingSession));
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
        (SIRIUS, &[0x30, 0x80, 0x00, 0x0A]),
        (SIRIUS, &[0x30, 0x9F, 0x01, 0x00]),
        (SIRIUS, &[0x30, 0xA0, 0x11, 0x00]),
        (SIRIUS, &[0x11, 0xFF]),
        (SIRIUS, &[0x32, 0xA0, 0x00]),
        (SIRIUS, &[0x31, 0xA0, 0x00]),
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
    let r = assess(&SIRIUS, &[0x31, 0xA0, 0x00]).unwrap();
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

/// Owner decision 2026-11-22 (final): security access and transfer services are Expert with
/// brick / lockout warnings, not forbidden.
#[test]
fn security_access_and_transfer_are_expert_with_warnings() {
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
            Some(DenyReason::ServiceNotAllowed { service }),
            "{p:02X?}"
        );
        let r = assess(&t, p).unwrap();
        assert_eq!(r.tier, Tier::Expert, "{p:02X?}");
        let w = if service == 0x27 {
            Warning::SecurityAccess
        } else {
            Warning::MayBrickEcu
        };
        assert!(r.warnings.contains(&w), "{p:02X?} {r:?}");
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
        (SIRIUS, &[0x31, 0xA0, 0xA0][..]),
        (SIRIUS, &[0x31, 0xA0, 0xA1]),
        (SIRIUS, &[0x31, 0xA0, 0xA2]),
        (SIRIUS, &[0x31, 0xA0, 0x00, 0x00]),
        (SIRIUS, &[0x31, 0xA4, 0x00]),
        (SIRIUS, &[0x31, 0xA2, 0x00]),
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
        assert_eq!(tier_of(&iso8_7a, p).unwrap(), Tier::Expert, "{p:02X?}");
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

/// Step 7 (independent RE cross-check, safe extract section C.1 and owner GAP 3 decision): BMT
/// actuator outputs 0xA6 "Ligne Transpondeur", 0xAB "Led Verlog", 0xAC "Ligne ECM"
/// (BMT_X65_ph1_p / BMT_SAGEM "COMMANDE DES ACTUATEURS"), and download / flash erase and
/// checksum on the immobilizer decoder, are forbidden on every BMT target; ordinary outputs and
/// the two remote-learning routines stay Tier 1; the Sirius keeps flash services Expert.
#[test]
fn bmt_immobilizer_outputs_and_flash_services_are_forbidden() {
    use kline_core::safety::{classify_in, DefinitionGuard, EcuClass};
    let bmt_guard = DefinitionGuard::new(EcuClass::Immobilizer);
    let elsewhere = Target {
        protocol: Protocol::Kwp,
        ecu_address: 0x40,
    };
    let bmt_alt = Target {
        protocol: Protocol::Kwp,
        ecu_address: 0xA6,
    };
    let none = DefinitionGuard::default();
    let forbidden: &[&[u8]] = &[
        &[0x31, 0xA6, 0x00, 0x01],
        &[0x31, 0xAB, 0x00, 0xFF],
        &[0x31, 0xAC, 0x01, 0x00],
        &[0x30, 0xA6, 0x00, 0x01],
        &[0x32, 0xAC, 0x00],
        &[0x33, 0xAB],
        &[0x34, 0x00, 0x00, 0x00, 0x30],
        &[0x35],
        &[0x36, 0x01, 0x02],
        &[0x37],
        &[0x31, 0x81, 0xF0, 0x5A],
        &[0x31, 0x82, 0x00],
        &[0x32, 0x81],
        &[0x33, 0x82],
    ];
    for p in forbidden {
        for (g, t) in [(&none, BMT), (&none, bmt_alt), (&bmt_guard, elsewhere)] {
            let d = classify_in(g, &t, p).expect_err("must be refused");
            assert_eq!(d.reason, DenyReason::Immobilizer, "{p:02X?} at {t:?}");
            assert!(d.reason.is_forbidden(), "{p:02X?}");
        }
    }
    for p in [
        &[0x31, 0xA0, 0x00, 0x01][..],
        &[0x31, 0xA7, 0x00, 0x01],
        &[0x31, 0xB3, 0x00, 0x01],
        &[0x31, 0xC7, 0x00, 0x00, 0x00],
        &[0x31, 0xC8, 0x00, 0x00, 0x00],
    ] {
        assert_eq!(tier(&BMT, p), Tier::Service, "{p:02X?}");
        assert_eq!(
            classify_in(&bmt_guard, &elsewhere, p),
            Ok(Tier::Service),
            "{p:02X?}"
        );
    }
    // BMT memory read 23 stays Expert (owner decision, GAP 3).
    assert!(matches!(
        reason(&BMT, &[0x23, 0x00, 0x10, 0x00, 0x04]),
        Some(DenyReason::ServiceNotAllowed { .. })
    ));
    // Not a BMT output on the Sirius; flash services stay Expert there.
    for p in [
        &[0x31, 0xA6, 0x00, 0x01][..],
        &[0x34, 0x00],
        &[0x31, 0x81, 0xF0, 0x5A],
        &[0x31, 0x82, 0x00],
    ] {
        let r = reason(&SIRIUS, p);
        assert!(
            r.as_ref().is_some_and(|r| !r.is_forbidden()),
            "{p:02X?} {r:?}"
        );
    }
}

/// Safe extract C.3: Sirius flash erase / checksum routines get specific warnings.
#[test]
fn sirius_flash_routines_warn_may_brick() {
    for p in [
        &[0x31, 0x81, 0xF0, 0x5A][..],
        &[0x31, 0x82, 0x00],
        &[0x31, 0x81],
    ] {
        let r = assess(&SIRIUS, p).unwrap();
        assert_eq!(r.tier, Tier::Expert, "{p:02X?}");
        assert!(r.warnings.contains(&Warning::MayBrickEcu), "{p:02X?} {r:?}");
        assert!(
            r.warnings.contains(&Warning::MayLeaveProgrammingSession),
            "{p:02X?} {r:?}"
        );
    }
}

/// GAP 2 (trf-investigation.md): BMT config index 3 TRF is the remote type, RF (1) or
/// infrared (0), not immobilizer: Tier 2 with a specific risk text and warning.
#[test]
fn bmt_trf_remote_type_is_tier2_with_specific_risk() {
    let ir = assess(&BMT, &[0x3B, 0xD2, 0x03, 0x00]).unwrap();
    assert_eq!(ir.tier, Tier::ConfigWrite);
    assert!(ir.warnings.contains(&Warning::ChangesRemoteType), "{ir:?}");
    assert!(ir.what_it_changes.contains("INFRARED"), "{ir:?}");
    assert!(
        ir.what_can_go_wrong.contains("RF remote stops working"),
        "{ir:?}"
    );
    let rf = assess(&BMT, &[0x3B, 0xD2, 0x03, 0x01]).unwrap();
    assert_eq!(rf.tier, Tier::ConfigWrite);
    assert!(rf.warnings.contains(&Warning::ChangesRemoteType), "{rf:?}");
    assert!(rf.what_it_changes.contains("RF remote"), "{rf:?}");
    assert_eq!(tier(&BMT, &[0x3B, 0xD2, 0x03, 0x00]), Tier::ConfigWrite);
    assert_eq!(tier(&BMT, &[0x3B, 0xD2, 0x03, 0x01]), Tier::ConfigWrite);
    // Security review L5: any other TRF value is still a remote-type change.
    for v in [0x02u8, 0x80, 0xFF] {
        let r = assess(&BMT, &[0x3B, 0xD2, 0x03, v]).unwrap();
        assert_eq!(r.tier, Tier::ConfigWrite, "{v:02X}");
        assert!(r.warnings.contains(&Warning::ChangesRemoteType), "{r:?}");
        assert!(r.what_it_changes.contains("unknown TRF value"), "{r:?}");
        assert!(r.prerequisites.iter().any(|p| p.contains("21 A1")), "{r:?}");
    }
    // Other indices keep value 0/1 only.
    assert_eq!(
        tier_of(&BMT, &[0x3B, 0xD2, 0x04, 0x02]).unwrap(),
        Tier::Expert
    );
}

/// Airbag ECU (X65 addressing 0x2C): writes and tests stay allowed with approval (owner:
/// nothing forbidden except the immobilizer), but every non-read carries a safety-system
/// warning (step 7, HIDDEN.md section 3: 3B D0 / 3B E0 airbag config writes).
#[test]
fn airbag_non_reads_carry_safety_system_warning() {
    const AIRBAG: Target = Target {
        protocol: Protocol::Kwp,
        ecu_address: 0x2C,
    };
    for p in [
        &[0x3B, 0xD0, 0x01][..],
        &[0x3B, 0xE0, 0x00],
        &[0x14, 0xFF, 0x00],
        &[0x30, 0x80, 0x00, 0x01],
        &[0x31, 0x81],
    ] {
        let r = assess(&AIRBAG, p).unwrap_or_else(|d| panic!("{p:02X?}: {d}"));
        assert_ne!(r.tier, Tier::Read, "{p:02X?}");
        assert!(
            r.warnings.contains(&Warning::SafetySystem),
            "{p:02X?} {r:?}"
        );
        assert!(
            r.safety_system.is_some_and(|t| t.contains("AIRBAG")),
            "{p:02X?} {r:?}"
        );
    }
    // Reads are untouched; other ECUs get no airbag warning.
    let read = assess(&AIRBAG, &[0x21, 0x80]).unwrap();
    assert!(!read.warnings.contains(&Warning::SafetySystem));
    assert_eq!(read.safety_system, None);
    let sirius = assess(&SIRIUS, &[0x3B, 0xD1, 0x10]).unwrap();
    assert!(!sirius.warnings.contains(&Warning::SafetySystem));
    assert_eq!(sirius.safety_system, None);
}

/// Code review M2 / security review L1: the airbag override adds `SafetySystem` and the airbag
/// prerequisites to the request's own risk; it never drops the request's own warnings,
/// prerequisites or text, and adds no warning that does not belong to the request.
#[test]
fn airbag_risk_keeps_the_request_warnings_and_prerequisites() {
    const AIRBAG: Target = Target {
        protocol: Protocol::Kwp,
        ecu_address: 0x2C,
    };
    const OTHER: Target = Target {
        protocol: Protocol::Kwp,
        ecu_address: 0x40,
    };
    for p in [
        &[0x14, 0xFF, 0x00][..],
        &[0x11, 0xFF],
        &[0x27, 0x01],
        &[0x10, 0x85],
        &[0x34, 0x00, 0x00],
        &[0x37],
        &[0x31, 0x81],
        &[0x3D, 0x00, 0x10, 0x01],
        &[0x38, 0x10, 0x00],
        &[0x2C, 0xF0],
        &[0x23, 0x00, 0x10],
        &[0x30, 0x80, 0x00, 0x01],
        &[0x32, 0xA0, 0x00],
        &[0x3B, 0xD0, 0x01],
        &[0x31, 0x00],
        &[0x99],
    ] {
        let a = assess(&AIRBAG, p).unwrap_or_else(|d| panic!("{p:02X?}: {d}"));
        let o = assess(&OTHER, p).unwrap_or_else(|d| panic!("{p:02X?}: {d}"));
        assert_eq!(a.tier, o.tier, "{p:02X?}");
        let mut want = vec![Warning::SafetySystem];
        want.extend(o.warnings.iter().copied());
        assert_eq!(
            a.warnings.iter().copied().collect::<Vec<_>>(),
            want,
            "{p:02X?}"
        );
        for pre in o.prerequisites.iter() {
            assert!(a.prerequisites.iter().any(|x| x == pre), "{p:02X?} {pre}");
        }
        assert!(
            a.prerequisites.iter().any(|x| x.contains("airbag lamp")),
            "{p:02X?} {a:?}"
        );
        assert_eq!(a.what_it_changes, o.what_it_changes, "{p:02X?}");
        assert_eq!(a.what_can_go_wrong, o.what_can_go_wrong, "{p:02X?}");
    }
    let clear = assess(&AIRBAG, &[0x14, 0xFF, 0x00]).unwrap();
    assert!(clear.warnings.contains(&Warning::ErasesFaultMemory));
    assert!(!clear.warnings.contains(&Warning::MovesActuator));
    assert!(assess(&AIRBAG, &[0x27, 0x01])
        .unwrap()
        .warnings
        .contains(&Warning::SecurityAccess));
    for p in [&[0x10, 0x85][..], &[0x34, 0x00, 0x00]] {
        assert!(assess(&AIRBAG, p)
            .unwrap()
            .warnings
            .contains(&Warning::MayLeaveProgrammingSession));
    }
    assert!(assess(&AIRBAG, &[0x11, 0xFF])
        .unwrap()
        .warnings
        .contains(&Warning::ErasesLearnedValues));
}

/// Final critique F17: the 31 C8 approval screen must not tell the human to run 31 C7 first
/// (the pairing plan uses 31 C8 alone when remote 1 is already learned).
#[test]
fn bmt_31c8_prerequisite_matches_the_pairing_plan() {
    let r = assess(&BMT, &[0x31, 0xC8, 0x00, 0x00, 0x00]).unwrap();
    let pre: Vec<&str> = r.prerequisites.iter().copied().collect();
    assert!(!pre.iter().any(|p| p.contains("right after")), "{pre:?}");
    assert!(
        pre.contains(&"remote 1 already learned (31C7 earlier, or a working remote 1)"),
        "{pre:?}"
    );
    assert!(pre.contains(&"have remote 2 at hand"), "{pre:?}");
}

/// Final critique F07: count FF on the lock motors / window stays Tier 1 but says so.
#[test]
fn bmt_actuator_count_ff_on_lock_and_window_has_a_specific_warning() {
    const FF: &str = "count FF = permanent: lock/window motor may run until stopped; use 1-3";
    for out in [0xA0, 0xA1, 0xA2, 0xA3] {
        let r = assess(&BMT, &[0x31, out, 0x00, 0xFF]).unwrap();
        assert_eq!(r.tier, Tier::Service, "{out:02X}");
        assert!(
            r.what_can_go_wrong.contains(FF),
            "{out:02X}: {}",
            r.what_can_go_wrong
        );
        assert!(r.warnings.iter().any(|w| *w == Warning::MovesActuator));
        // Counts 1-3 keep the normal text.
        let r = assess(&BMT, &[0x31, out, 0x00, 0x02]).unwrap();
        assert_eq!(r.tier, Tier::Service);
        assert!(
            !r.what_can_go_wrong.contains("count FF"),
            "{}",
            r.what_can_go_wrong
        );
    }
    // Another Tier 1 output with FF (A7): not a lock/window motor, no lock/window text.
    let r = assess(&BMT, &[0x31, 0xA7, 0x00, 0xFF]).unwrap();
    assert_eq!(r.tier, Tier::Service);
    assert!(!r.what_can_go_wrong.contains("count FF"));
    // A4/A5 with FF are Expert, with the generic unknown-effect text.
    for out in [0xA4, 0xA5] {
        let r = assess(&BMT, &[0x31, out, 0x00, 0xFF]).unwrap();
        assert_eq!(r.tier, Tier::Expert, "{out:02X}");
        assert!(!r.what_can_go_wrong.contains("count FF"));
    }
}

const SIRIUS_OUTPUTS: [u8; 13] = [
    0x80, 0x82, 0x83, 0x86, 0x88, 0x8F, 0x94, 0x9E, 0x9F, 0xA0, 0xA7, 0xAA, 0xAC,
];

/// Audit 2026-11-23: Sirius `30 id cmd cycles` is Tier 1 only for the 13 ids of the definition's
/// "Output Temporary Control List"; start needs cycles 1..=10, status/stop need 00 as 4th byte;
/// every other id, command, cycles or length is Tier 3 Expert.
#[test]
fn sirius_output_control_risk_matrix() {
    for id in 0u8..=0xFF {
        let allowed = SIRIUS_OUTPUTS.contains(&id);
        for cmd in 0u8..=0xFF {
            for cc in [0x00u8, 0x01, 0x05, 0x0A, 0x0B, 0x10, 0xFF] {
                let p = [0x30, id, cmd, cc];
                let ok = allowed
                    && match cmd {
                        0x00 => (1..=10).contains(&cc),
                        0x01 | 0x11 => cc == 0,
                        _ => false,
                    };
                let want = if ok { Tier::Service } else { Tier::Expert };
                assert_eq!(tier_of(&SIRIUS, &p).unwrap(), want, "{p:02X?}");
            }
        }
    }
    // Lengths other than 4 on an allowed id: Expert.
    for p in [
        &[0x30, 0x80, 0x00][..],
        &[0x30, 0x80],
        &[0x30, 0x80, 0x00, 0x01, 0x00],
        &[0x30, 0x80, 0x00, 0x01, 0x00, 0x00],
    ] {
        assert_eq!(tier_of(&SIRIUS, p).unwrap(), Tier::Expert, "{p:02X?}");
        assert_eq!(assess(&SIRIUS, p).unwrap().tier, Tier::Expert, "{p:02X?}");
    }
    // The definition's placeholder id D0 and plausible other ids are Expert with UnknownEffect.
    for id in [0xD0u8, 0x00, 0x81, 0x87, 0xB5, 0xA6, 0xAB, 0xC0] {
        let p = [0x30, id, 0x00, 0x0A];
        assert_eq!(tier_of(&SIRIUS, &p).unwrap(), Tier::Expert, "{p:02X?}");
        let r = assess(&SIRIUS, &p).unwrap();
        assert!(r.warnings.contains(&Warning::UnknownEffect), "{p:02X?}");
    }
    let r = assess(&SIRIUS, &[0x30, 0x9F, 0x00, 0x0A]).unwrap();
    assert_eq!(r.tier, Tier::Service);
    assert!(r.warnings.contains(&Warning::MovesActuator));
}

/// Cycle boundaries: 1 and 10 are Tier 1, 0 and 11 (and FF) are Expert with a text that says
/// long or unlimited actuation.
#[test]
fn sirius_output_start_cycles_boundaries() {
    for (cc, t) in [
        (0x00u8, Tier::Expert),
        (0x01, Tier::Service),
        (0x0A, Tier::Service),
        (0x0B, Tier::Expert),
        (0xFF, Tier::Expert),
    ] {
        let p = [0x30, 0x80, 0x00, cc];
        assert_eq!(tier_of(&SIRIUS, &p).unwrap(), t, "{p:02X?}");
        let r = assess(&SIRIUS, &p).unwrap();
        assert_eq!(r.tier, t);
        if t == Tier::Expert {
            assert!(r.what_it_changes.contains("long or unlimited"), "{r:?}");
            assert!(r.warnings.contains(&Warning::UnknownEffect));
            assert!(r.warnings.contains(&Warning::MovesActuator));
        } else {
            assert!(!r.what_it_changes.contains("unlimited"), "{r:?}");
        }
    }
}

/// Status and stop are exactly `30 id 01 00` / `30 id 11 00`; their risk text does not say the
/// output is driven.
#[test]
fn sirius_output_status_and_stop_text() {
    let st = assess(&SIRIUS, &[0x30, 0x80, 0x01, 0x00]).unwrap();
    assert_eq!(st.tier, Tier::Service);
    assert!(st.what_it_changes.contains("status"), "{st:?}");
    assert!(!st.what_it_changes.contains("drives"), "{st:?}");
    assert!(!st.warnings.contains(&Warning::MovesActuator));
    let sp = assess(&SIRIUS, &[0x30, 0x80, 0x11, 0x00]).unwrap();
    assert_eq!(sp.tier, Tier::Service);
    assert!(sp.what_it_changes.contains("ends"), "{sp:?}");
    assert!(!sp.what_it_changes.contains("drives"), "{sp:?}");
    for p in [[0x30, 0x80, 0x01, 0x01], [0x30, 0x80, 0x11, 0x0A]] {
        assert_eq!(tier_of(&SIRIUS, &p).unwrap(), Tier::Expert, "{p:02X?}");
    }
}

/// This car has no A/C: on a Sirius 0x82 start stays Tier 1 but its risk text says so. The text
/// is Sirius-only: the airbag ECU and a generic ECU get the plain output text.
#[test]
fn sirius_ac_output_warns_the_car_has_no_ac() {
    let r = assess(&SIRIUS, &[0x30, 0x82, 0x00, 0x0A]).unwrap();
    assert_eq!(r.tier, Tier::Service);
    assert!(r.what_it_changes.contains("NO A/C"), "{r:?}");
    assert!(
        r.prerequisites.iter().any(|t| t.contains("no A/C")),
        "{r:?}"
    );
    assert!(r.warnings.contains(&Warning::MovesActuator));
    let other = assess(&SIRIUS, &[0x30, 0x80, 0x00, 0x0A]).unwrap();
    assert!(!other.what_it_changes.contains("A/C"), "{other:?}");
    for addr in [0x2Cu8, 0x40] {
        let t = Target {
            protocol: Protocol::Kwp,
            ecu_address: addr,
        };
        let r = assess(&t, &[0x30, 0x82, 0x00, 0x0A]).unwrap();
        assert_eq!(r.tier, Tier::Service, "{addr:02X}");
        assert!(!r.what_it_changes.contains("A/C"), "{addr:02X}: {r:?}");
        assert!(
            !r.prerequisites.iter().any(|t| t.contains("A/C")),
            "{addr:02X}"
        );
    }
}

/// The Sirius allowlist follows `is_sirius_ecu`: a Sirius definition at a non-7A address
/// restricts, an Iso8 target at 7A has no `30` at all, a Generic (S2000) definition at 7A
/// restricts, a Generic definition elsewhere keeps `30 lid` + 1-4 bytes Tier 1.
#[test]
fn sirius_allowlist_scope() {
    use kline_core::safety::{tier_of_in, DefinitionGuard, EcuClass};
    let at = |a| Target {
        protocol: Protocol::Kwp,
        ecu_address: a,
    };
    let sirius_def = DefinitionGuard::new(EcuClass::Sirius);
    let generic = DefinitionGuard::new(EcuClass::Generic);
    let bad = [0x30, 0xD0, 0x00, 0x0A];
    let good = [0x30, 0x80, 0x00, 0x0A];
    // Sirius definition at a non-7A address.
    assert_eq!(
        tier_of_in(&sirius_def, &at(0x40), &bad).unwrap(),
        Tier::Expert
    );
    assert_eq!(
        tier_of_in(&sirius_def, &at(0x40), &good).unwrap(),
        Tier::Service
    );
    let r =
        kline_core::safety::assess_in(&sirius_def, &at(0x40), &[0x30, 0x82, 0x00, 0x0A]).unwrap();
    assert!(r.what_it_changes.contains("NO A/C"));
    // Generic definition (e.g. S2000) at 7A restricts; elsewhere it does not.
    assert_eq!(tier_of_in(&generic, &at(0x7A), &bad).unwrap(), Tier::Expert);
    assert_eq!(
        tier_of_in(&generic, &at(0x40), &bad).unwrap(),
        Tier::Service
    );
    assert_eq!(tier_of(&at(0x40), &bad).unwrap(), Tier::Service);
    // Iso8 at 7A: no `30` request exists there, Expert (not Tier 1).
    let iso = Target {
        protocol: Protocol::Iso8,
        ecu_address: 0x7A,
    };
    assert_eq!(tier_of(&iso, &good).unwrap(), Tier::Expert);
    assert_eq!(tier_of(&iso, &bad).unwrap(), Tier::Expert);
    // A BMT definition at 7A is not a Sirius: BMT rules (`30` Expert, A6 forbidden).
    let bmt_def = DefinitionGuard::new(EcuClass::Immobilizer);
    assert_eq!(
        tier_of_in(&bmt_def, &at(0x7A), &good).unwrap(),
        Tier::Expert
    );
}

/// Immobilizer-class outputs are forbidden on a BMT for `30`-`33`; on a Sirius `30 A6/AB/AC`
/// are other number spaces: Expert (not forbidden) except AC, which is the shift lamp (Tier 1).
#[test]
fn output_control_forbidden_ids_vs_sirius() {
    for id in [0xA6u8, 0xAB, 0xAC, 0xC0, 0xC9] {
        for svc in [0x30u8, 0x31, 0x32, 0x33] {
            let p = [svc, id, 0x00, 0x01];
            assert_eq!(reason(&BMT, &p), Some(DenyReason::Immobilizer), "{p:02X?}");
        }
    }
    for id in [0xA6u8, 0xAB] {
        assert_eq!(
            tier_of(&SIRIUS, &[0x30, id, 0x00, 0x01]).unwrap(),
            Tier::Expert
        );
    }
    assert_eq!(
        tier_of(&SIRIUS, &[0x30, 0xAC, 0x00, 0x01]).unwrap(),
        Tier::Service
    );
}

/// Audit 2026-11-23: BMT actuators A4/A5 (unnamed "LO"/"LF" lines) are Expert; the named ones in
/// A0..=B3 stay Tier 1, A6/AB/AC stay forbidden, B4+ stays Expert.
#[test]
fn bmt_actuator_a4_a5_are_expert() {
    for id in 0xA0u8..=0xB3 {
        let p = [0x31, id, 0x00, 0x01];
        match id {
            0xA6 | 0xAB | 0xAC => {
                assert_eq!(reason(&BMT, &p), Some(DenyReason::Immobilizer), "{p:02X?}")
            }
            0xA4 | 0xA5 => {
                assert_eq!(tier_of(&BMT, &p).unwrap(), Tier::Expert, "{p:02X?}");
                let r = assess(&BMT, &p).unwrap();
                assert_eq!(r.tier, Tier::Expert);
                assert!(r.warnings.contains(&Warning::UnknownEffect));
            }
            _ => assert_eq!(tier_of(&BMT, &p).unwrap(), Tier::Service, "{p:02X?}"),
        }
    }
    for id in [0xB4u8, 0xB5, 0xBF] {
        let p = [0x31, id, 0x00, 0x01];
        assert_eq!(tier_of(&BMT, &p).unwrap(), Tier::Expert, "{p:02X?}");
    }
}
