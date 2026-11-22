//! Final security re-check regressions (owner decision 2026-11-22 on the immobilizer line).
//! Ported from the review's exploits: each exploit is asserted to FAIL now. Requests the owner
//! decided must stay reachable for mechanics are asserted to stay Expert.

#[cfg(feature = "approved-routines")]
mod common;

use kline_core::safety::{
    self, assess_in, tier_of, tier_of_in, DefinitionGuard, DenyReason, EcuClass, Protocol, Target,
    Tier,
};

const BMT: Target = Target {
    protocol: Protocol::Kwp,
    ecu_address: 0x26,
};
const BMT_ALT: Target = Target {
    protocol: Protocol::Kwp,
    ecu_address: 0xA6,
};
const SIRIUS: Target = Target {
    protocol: Protocol::Kwp,
    ecu_address: 0x7A,
};

fn immo(t: &Target, p: &[u8]) -> bool {
    matches!(
        tier_of(t, p),
        Err(ref d) if d.reason == DenyReason::Immobilizer
    )
}

/// Exploit `bmt_equivalent_sids_reach_expert`: memory write / routine by address / dynamic
/// define and the 31 C9 routine on the BMT (both addresses) are forbidden now.
#[test]
fn bmt_memory_and_routine_by_address_services_are_forbidden() {
    let cases: &[&[u8]] = &[
        &[0x3D, 0x00, 0x10, 0x01, 0xAA],
        &[0x3D],
        &[0x38, 0x00, 0xC2],
        &[0x39, 0x00, 0x10, 0x00],
        &[0x3A, 0x00, 0x10, 0x00],
        &[0x2C, 0xF0, 0x01],
        &[0x31, 0xC9, 0x00],
        &[0x30, 0xC9],
        &[0x32, 0xC9],
        &[0x33, 0xC9],
        &[0x31, 0xC0],
        &[0x31, 0xC6, 0x00, 0x00, 0x00],
        &[0x3B, 0xD2, 0x02, 0x01],
    ];
    for t in [BMT, BMT_ALT] {
        for p in cases {
            assert!(
                immo(&t, p),
                "{:02X} {p:02X?}: {:?}",
                t.ecu_address,
                tier_of(&t, p)
            );
        }
    }
    // Owner-approved remote learning stays Tier 1 on both addresses.
    for t in [BMT, BMT_ALT] {
        assert_eq!(tier_of(&t, &[0x31, 0xC7, 0, 0, 0]).unwrap(), Tier::Service);
        assert_eq!(tier_of(&t, &[0x31, 0xC8, 0, 0, 0]).unwrap(), Tier::Service);
    }
}

/// Owner decision: undocumented requests outside the immobilizer line stay Expert.
#[test]
fn owner_decision_undocumented_requests_stay_expert() {
    let bmt: &[&[u8]] = &[
        &[0x31],
        &[0x3B, 0xD2],
        &[0x3B, 0xD2, 0x0E, 0x01],
        &[0x3B, 0xD2, 0x03, 0x01, 0x02, 0x01],
        &[0x21, 0xA1, 0x00],
        &[0x11, 0x01],
        &[0x85, 0x02],
        &[0x83, 0x03, 0, 0, 0, 0],
        &[0x23, 0x00, 0x10, 0x00, 0x04],
    ];
    for p in bmt {
        assert_eq!(tier_of(&BMT, p).unwrap(), Tier::Expert, "{p:02X?}");
    }
    let sirius: &[&[u8]] = &[
        &[0x3D, 0x01, 0x00, 0x01, 0x00],
        &[0x38, 0x10, 0x00],
        &[0x2C, 0xF0, 0x01],
        &[0x31, 0xA0],
        &[0x31, 0xA0, 0xA1],
        &[0x23, 0x00, 0x10, 0x00, 0x04],
    ];
    for p in sirius {
        assert_eq!(tier_of(&SIRIUS, p).unwrap(), Tier::Expert, "{p:02X?}");
    }
}

/// Finding 3 and exploit `sirius_immobilizer_variants`, narrowed by security review M1: on a
/// Sirius, writes to the immobilizer frame LIDs and every `.. A0 A3` routine variant are
/// forbidden. Output control `30` and routines `31`-`33` / `38`-`3A` whose byte 2 happens to be
/// A0/A2/AE/BD are other number spaces (`30 A0` is the fast radiator fan, `31 A2 00 00 00`
/// "SRBLI start test sav", `31 A2 01 00 00` "Status tests OBDFC"): normal tiers.
#[test]
fn sirius_immobilizer_lids_and_backup_reset_variants_are_forbidden() {
    let cases: &[&[u8]] = &[
        &[0x31, 0xA0, 0xA3],
        &[0x31, 0xA0, 0xA3, 0xFF],
        &[0x32, 0xA0, 0xA3],
        &[0x33, 0xA0, 0xA3],
        &[0x38, 0xA0, 0xA3],
        &[0x39, 0xA0, 0xA3],
        &[0x3A, 0xA0, 0xA3],
        &[0x2E, 0xA0, 0x01],
        &[0x2E, 0xA2, 0x01],
        &[0x2F, 0xAE, 0x01],
        &[0x3B, 0xBD, 0x01],
        &[0x3B, 0xA2, 0x01],
    ];
    for p in cases {
        assert!(immo(&SIRIUS, p), "{p:02X?}: {:?}", tier_of(&SIRIUS, p));
    }
    // Not immobilizer: output control and routines on those byte values.
    for (p, t) in [
        (&[0x30, 0xA0, 0x00, 0x00][..], Tier::Service),
        (&[0x30, 0xBD, 0x01], Tier::Service),
        (&[0x30, 0xA2, 0x01], Tier::Service),
        (&[0x30, 0xAE, 0x01, 0x02], Tier::Service),
        (&[0x31, 0xA2, 0x00, 0x00, 0x00], Tier::Expert),
        (&[0x31, 0xA2, 0x01, 0x00, 0x00], Tier::Expert),
        (&[0x31, 0xAE], Tier::Expert),
        (&[0x32, 0xBD], Tier::Expert),
        (&[0x33, 0xA2], Tier::Expert),
        (&[0x38, 0xA2, 0x00, 0x00], Tier::Expert),
    ] {
        assert_eq!(tier_of(&SIRIUS, p).unwrap(), t, "{p:02X?}");
        let r = safety::assess(&SIRIUS, p).unwrap();
        assert_eq!(r.tier, t, "{p:02X?}");
    }
    assert!(safety::assess(&SIRIUS, &[0x30, 0xA0, 0x01])
        .unwrap()
        .warnings
        .contains(&safety::Warning::MovesActuator));
    // Documented Sirius service actions are unchanged.
    assert_eq!(
        tier_of(&SIRIUS, &[0x31, 0xA0, 0xA0]).unwrap(),
        Tier::Service
    );
    assert_eq!(
        tier_of(&SIRIUS, &[0x32, 0xA0, 0x00]).unwrap(),
        Tier::Service
    );
    assert_eq!(
        tier_of(&SIRIUS, &[0x30, 0xD0, 0x01]).unwrap(),
        Tier::Service
    );
    // Reading the immobilizer frames stays Tier 0.
    assert_eq!(tier_of(&SIRIUS, &[0x21, 0xA0]).unwrap(), Tier::Read);
}

/// Exploit `other_addresses_get_no_bmt_rules`: an ECU whose definition is a BMT/UCH/decoder
/// gets the BMT rules at any address.
#[test]
fn immobilizer_class_definition_gets_bmt_rules_at_any_address() {
    let g = DefinitionGuard::new(EcuClass::Immobilizer);
    for a in [0x25u8, 0x27, 0x40, 0x00] {
        let t = Target {
            protocol: Protocol::Kwp,
            ecu_address: a,
        };
        for p in [
            &[0x31, 0xC2, 0x00][..],
            &[0x2E, 0x10, 0x01],
            &[0x21, 0xA4, 0x01],
            &[0x3D, 0x00, 0x10, 0x01],
            &[0x3B, 0xD2, 0x02, 0x01],
        ] {
            assert!(
                matches!(tier_of_in(&g, &t, p), Err(ref d) if d.reason == DenyReason::Immobilizer),
                "{a:02X} {p:02X?}"
            );
        }
        assert_eq!(
            tier_of_in(&g, &t, &[0x31, 0xC7, 0, 0, 0]).unwrap(),
            Tier::Service
        );
        // Control: a generic definition at that address keeps the generic rules.
        assert_eq!(
            tier_of_in(&DefinitionGuard::default(), &t, &[0x31, 0xC2, 0x00]).unwrap(),
            Tier::Expert
        );
    }
    let r = assess_in(
        &g,
        &Target {
            protocol: Protocol::Kwp,
            ecu_address: 0x40,
        },
        &[0x2C, 1],
    );
    assert!(r.unwrap_err().reason.is_forbidden());
    assert!(safety::forbidden(&BMT_ALT, &[0x2E, 1, 2]).is_some());
}

/// The Sirius LID-write rule follows a Sirius definition to any address and is limited to it
/// (security review M1: on other ECUs A0/A2/AE/BD mean other things). `.. A0 A3` routines
/// stay refused on every non-immobilizer ECU.
#[test]
fn sirius_lid_rules_follow_the_definition_only() {
    let t = Target {
        protocol: Protocol::Kwp,
        ecu_address: 0x40,
    };
    let sirius = DefinitionGuard::new(EcuClass::Sirius);
    for p in [
        &[0x3B, 0xA0, 0x01][..],
        &[0x2E, 0xAE, 0x00],
        &[0x2F, 0xBD, 0x01],
    ] {
        assert!(
            tier_of_in(&sirius, &t, p)
                .unwrap_err()
                .reason
                .is_forbidden(),
            "{p:02X?}"
        );
        // A generic ECU at 0x40: the LID rule does not apply.
        assert!(!immo(&t, p), "{p:02X?}: {:?}", tier_of(&t, p));
    }
    assert_eq!(tier_of(&t, &[0x3B, 0xA0, 0x01]).unwrap(), Tier::ConfigWrite);
    assert_eq!(tier_of(&t, &[0x2E, 0xAE, 0x00]).unwrap(), Tier::Expert);
    for g in [&sirius, &DefinitionGuard::default()] {
        assert_eq!(
            tier_of_in(g, &t, &[0x30, 0xA0, 0x01]).unwrap(),
            Tier::Service
        );
        assert_eq!(
            tier_of_in(g, &t, &[0x31, 0xAE, 0x00]).unwrap(),
            Tier::Expert
        );
        assert_eq!(tier_of_in(g, &t, &[0x38, 0xBD]).unwrap(), Tier::Expert);
    }
    assert!(immo(&t, &[0x32, 0xA0, 0xA3]));
    assert!(immo(&t, &[0x31, 0xA0, 0xA3]));
}

/// Owner decision 2026-11-22 (final): nothing outside the immobilizer line is forbidden.
/// Security access, transfer and other sessions are Expert with specific warnings. Step 7
/// owner decision: on the BMT (immobilizer decoder) transfer `34`-`37` is part of the
/// immobilizer line (it can overwrite the key-code EEPROM) and is forbidden there.
#[test]
fn owner_final_decision_security_transfer_and_sessions_are_expert() {
    use kline_core::safety::{assess, Warning};
    let transfer: [(&[u8], Warning); 4] = [
        (&[0x34, 0x00, 0x00], Warning::MayBrickEcu),
        (&[0x35], Warning::MayBrickEcu),
        (&[0x36, 0x01], Warning::MayBrickEcu),
        (&[0x37], Warning::MayBrickEcu),
    ];
    for t in [SIRIUS, BMT] {
        for (p, w) in [
            (&[0x27, 0x01][..], Warning::SecurityAccess),
            (&[0x10, 0x85], Warning::MayLeaveProgrammingSession),
            (&[0x10, 0xC0], Warning::MayLeaveProgrammingSession),
        ]
        .into_iter()
        .chain(if t == SIRIUS {
            transfer.to_vec()
        } else {
            Vec::new()
        }) {
            let r = assess(&t, p).unwrap();
            assert_eq!(r.tier, Tier::Expert, "{p:02X?}");
            assert!(r.warnings.contains(&w), "{p:02X?} {r:?}");
        }
    }
    for (p, _) in transfer {
        assert_eq!(
            safety::forbidden(&BMT, p),
            Some(DenyReason::Immobilizer),
            "{p:02X?}"
        );
    }
    let r = assess(&SIRIUS, &[0x3D, 0x01, 0x00, 0x01, 0x00]).unwrap();
    assert!(r.warnings.contains(&Warning::MayBrickEcu));
    // Link frames stay reserved for the session machine.
    assert_eq!(
        safety::forbidden(&SIRIUS, &[0x82]),
        Some(DenyReason::ProtocolReserved { service: 0x82 })
    );
}

/// Finding 5: the definition name guard is part of the core policy.
#[test]
fn named_immobilizer_request_is_refused_by_the_core() {
    let mut g = DefinitionGuard::new(EcuClass::Generic);
    g.flag(vec![0x3B, 0x50], "Ecriture code antidemarrage".into());
    let t = Target {
        protocol: Protocol::Kwp,
        ecu_address: 0x51,
    };
    let d = tier_of_in(&g, &t, &[0x3B, 0x50, 0x12]).unwrap_err();
    assert_eq!(
        d.reason,
        DenyReason::ImmobilizerNamed {
            names: vec!["Ecriture code antidemarrage".into()]
        }
    );
    assert!(d.reason.is_forbidden());
    // Reads are decided on bytes only (byte rules primary).
    assert_eq!(tier_of_in(&g, &t, &[0x21, 0x50]).unwrap(), Tier::Read);
    // Control: another request is untouched.
    assert_eq!(
        tier_of_in(&g, &t, &[0x3B, 0x51, 0x12]).unwrap(),
        Tier::ConfigWrite
    );
}

#[cfg(feature = "spec")]
mod with_spec {
    use super::*;
    use kline_spec::EcuDef;

    fn def(target: &str, function: &str) -> EcuDef {
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Target Name="{target}" xmlns="http://www-diag.renault.com/2002/ECU">
<Function Name="{function}" Address="64"/>
<Datas><Data Name="Lamp"><Bits><Scaled/></Bits></Data></Datas>
<Requests Endian="Big">
<Request Name="Lamp test"><Sent><SentBytes>30D001</SentBytes></Sent><Received MinBytes="1"/></Request>
<Request Name="Code cle write"><Sent><SentBytes>3B6000</SentBytes></Sent><Received MinBytes="1"/></Request>
</Requests>
</Target>"#
        );
        EcuDef::from_xml_bytes(xml.as_bytes()).unwrap()
    }

    #[test]
    fn definition_class_is_detected_from_target_and_function_names() {
        for (t, f) in [
            ("BMT test", "Body"),
            ("Test", "UCH"),
            ("Test", "UCBIC/BFR"),
            ("Decodeur test", "Body"),
            ("Boitier anti-démarrage", "X"),
        ] {
            assert_eq!(
                kline_core::spec::definition_guard(&def(t, f)).class,
                EcuClass::Immobilizer,
                "{t} / {f}"
            );
        }
        assert_eq!(
            kline_core::spec::definition_guard(&def("SIRIUS32 test", "Injection")).class,
            EcuClass::Sirius
        );
        for (t, f) in [
            ("S2000 test", "Injection"),
            ("ABS", "ABS"),
            ("Touch", "Clutch"),
        ] {
            assert_eq!(
                kline_core::spec::definition_guard(&def(t, f)).class,
                EcuClass::Generic,
                "{t} / {f}"
            );
        }
    }

    #[test]
    fn definition_guard_names_flagged_requests() {
        let g = kline_core::spec::definition_guard(&def("Test", "Lights"));
        let t = Target {
            protocol: Protocol::Kwp,
            ecu_address: 0x40,
        };
        assert!(tier_of_in(&g, &t, &[0x3B, 0x60, 0x00])
            .unwrap_err()
            .reason
            .is_forbidden());
        assert_eq!(
            tier_of_in(&g, &t, &[0x30, 0xD0, 0x01]).unwrap(),
            Tier::Service
        );
    }
}

#[cfg(feature = "approved-routines")]
mod with_session {
    use super::*;
    use crate::common::*;
    use kline_core::session::{Session, SessionError};
    use kline_core::{InitKind, KTiming, LinkConfig};

    fn cfg(addr: u8) -> LinkConfig {
        LinkConfig::new(
            addr,
            KTiming {
                init: InitKind::KwpFiveBaud { multipoint: false },
                ..KTiming::default()
            },
        )
    }

    fn denied(r: Result<impl core::fmt::Debug, SessionError>) -> DenyReason {
        match r {
            Err(SessionError::Denied(d)) => d.reason,
            other => panic!("not denied: {other:?}"),
        }
    }

    /// Finding 5: `Session` applies the definition guard to every challenge.
    #[test]
    fn session_challenges_apply_the_definition_guard() {
        let mut s = Session::new(open(&cfg(0x40), 0xD0, 0x8F), 0);
        // Control: without a guard the expert request is approvable.
        assert!(s.expert_challenge(&[0x31, 0xC2, 0x00]).is_ok());
        let mut g = DefinitionGuard::new(EcuClass::Immobilizer);
        g.flag(vec![0x30, 0x10], "Commande antidemarrage".into());
        s.set_definition_guard(g);
        assert_eq!(
            denied(s.expert_challenge(&[0x31, 0xC2, 0x00])),
            DenyReason::Immobilizer
        );
        assert_eq!(
            denied(s.expert_challenge(&[0x3D, 0x00, 0x10, 0x01])),
            DenyReason::Immobilizer
        );
        assert!(matches!(
            denied(s.service_challenge(&[0x30, 0x10, 0x01])),
            DenyReason::ImmobilizerNamed { .. }
        ));
        assert!(matches!(
            denied(s.expert_challenge(&[0x30, 0x10, 0x01, 0x02, 0x03, 0x04, 0x05])),
            DenyReason::ImmobilizerNamed { .. }
        ));
        // Remote learning stays approvable on an immobilizer-class definition.
        assert!(s.service_challenge(&[0x31, 0xC7, 0, 0, 0]).is_ok());
    }
}

/// Security review H1: if the BMT firmware masks the `3B D2` index to its low nibble,
/// `3B D2 x2 ..` would write AP1CLE (index 2). Forbidden on an immobilizer ECU; other
/// indices above 0x0F stay Expert.
#[test]
fn bmt_config_index_nibble_alias_of_ap1cle_is_forbidden() {
    let immo_def = DefinitionGuard::new(EcuClass::Immobilizer);
    let at_40 = Target {
        protocol: Protocol::Kwp,
        ecu_address: 0x40,
    };
    for idx in [0x02u8, 0x12, 0x22, 0x82, 0xF2] {
        for p in [vec![0x3B, 0xD2, idx, 0x01], vec![0x3B, 0xD2, idx]] {
            assert!(immo(&BMT, &p), "{p:02X?}: {:?}", tier_of(&BMT, &p));
            assert!(immo(&BMT_ALT, &p), "{p:02X?}");
            assert!(
                matches!(tier_of_in(&immo_def, &at_40, &p), Err(ref d) if d.reason == DenyReason::Immobilizer),
                "{p:02X?}"
            );
        }
    }
    for idx in [0x10u8, 0x13, 0x21, 0x83, 0xF3, 0xFF] {
        assert_eq!(
            tier_of(&BMT, &[0x3B, 0xD2, idx, 0x01]).unwrap(),
            Tier::Expert,
            "{idx:02X}"
        );
    }
    assert_eq!(
        tier_of(&BMT, &[0x3B, 0xD2, 0x01, 0x01]).unwrap(),
        Tier::ConfigWrite
    );
    // Not an immobilizer ECU: 3B D2 12 is an ordinary config write.
    assert_eq!(
        tier_of(&SIRIUS, &[0x3B, 0xD2, 0x12, 0x01]).unwrap(),
        Tier::ConfigWrite
    );
}
