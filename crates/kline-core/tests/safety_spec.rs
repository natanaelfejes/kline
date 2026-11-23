//! Every request of the car's definitions against the safety policy (`spec` feature, reads
//! ddt-xml/, skipped when absent). Expected allowed sets are written out by hand here, so a
//! policy change that unlocks anything else fails this test.
#![cfg(feature = "spec")]

mod common;

use kline_core::safety::{classify, Protocol, Target, Tier, BMT_REMOTE_LEARNING};
use kline_core::spec::link_config;
use kline_spec::EcuDef;

struct Case {
    file: &'static str,
    /// SentBytes (uppercase hex) per allowed tier. Everything else must be denied.
    read: &'static [&'static str],
    service: &'static [&'static str],
    config_write: &'static [&'static str],
    /// Number of requests in the file, to notice parser or file changes.
    requests: usize,
}

const SAFIR_READS: &[&str] = &["21", "3350", "3351", "3352", "3354"];

const CASES: [Case; 4] = [
    Case {
        file: "SIRIUS32___EMS3132___Soft_RE1_maj.xml",
        read: &[
            "10A0", "2180", "2181", "2182", "21A0", "21A1", "21A2", "21A3", "21A4", "21A5", "21A6",
            "21A7", "21A8", "21A9", "21AA", "21AB", "21AC", "21AD", "21AE", "21AF", "21BD", "21C6",
            "21C7",
        ],
        // Output control, clear DTC, adaptation reset, stop functions. "Fonctions
        // particulieres" 31A000 is `31 fn 00` with its default function 0xA0, injector
        // inhibition (final critique F02); the other routines stay denied. The Output Control
        // templates 30D0000A/30D00100/30D01100 carry the placeholder id D0, which is not in the
        // 13-id allowlist: Expert (not classified), audit 2026-11-23.
        service: &["140000", "11FF", "32A000", "31A000"],
        config_write: &["3BD010"],
        requests: 34,
    },
    Case {
        file: "BMT_X65_ph1_p.xml",
        // Policy v2: 21A4 (transponder status) is a read.
        read: &[
            "2180", "21A0", "21A1", "21A2", "21A3", "21A4", "21A7", "21A8",
        ],
        // 31000000 (actuator template, output 00) and 3100000000 (routine launch template)
        // stay denied as sent; only filled-in outputs 0xA0..=0xB3 are Tier 1.
        service: &["31C7000000", "31C8000000", "14FF00"],
        config_write: &["3BD20000"],
        requests: 15,
    },
    Case {
        file: "Safir_1.xml",
        read: SAFIR_READS,
        service: &[],
        config_write: &[],
        requests: 15,
    },
    Case {
        file: "Safir_2.xml",
        read: SAFIR_READS,
        service: &[],
        config_write: &[],
        requests: 15,
    },
];

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

#[test]
fn every_request_classified() {
    for case in CASES {
        let Some(bytes) = common::ddt_file(case.file) else {
            continue;
        };
        let ecu = EcuDef::from_xml_bytes(&bytes).unwrap();
        let cfg = link_config(&ecu).unwrap();
        let target = Target {
            protocol: if cfg.timing.init.is_iso8() {
                Protocol::Iso8
            } else {
                Protocol::Kwp
            },
            ecu_address: cfg.ecu_address,
        };
        assert_eq!(ecu.requests.len(), case.requests, "{}", case.file);
        eprintln!(
            "== {} ({:?} @ {:#04x})",
            case.file, target.protocol, target.ecu_address
        );
        let mut seen = Vec::new();
        for r in &ecu.requests {
            let b = &r.sent_bytes.0;
            let h = hex(b);
            let verdict = classify(&target, b);
            let label = match &verdict {
                Ok(t) => format!("{t:?}"),
                Err(d) => format!("DENY {:?}", d.reason),
            };
            eprintln!("  {h:<12} {label:<45} {}", r.name);
            let expected = if case.read.contains(&h.as_str()) {
                Some(Tier::Read)
            } else if case.service.contains(&h.as_str()) {
                Some(Tier::Service)
            } else if case.config_write.contains(&h.as_str()) {
                Some(Tier::ConfigWrite)
            } else {
                None
            };
            assert_eq!(
                verdict.as_ref().ok().copied(),
                expected,
                "{}: {h} ({}) got {label}",
                case.file,
                r.name
            );
            seen.push(h);
        }
        for a in case
            .read
            .iter()
            .chain(case.service)
            .chain(case.config_write)
        {
            assert!(
                seen.iter().any(|s| s == a),
                "{}: {a} not in file",
                case.file
            );
        }
    }
}

#[test]
fn bmt_remote_learning_is_tier1_and_in_definition() {
    let Some(bytes) = common::ddt_file("BMT_X65_ph1_p.xml") else {
        return;
    };
    let ecu = EcuDef::from_xml_bytes(&bytes).unwrap();
    let cfg = link_config(&ecu).unwrap();
    let target = Target {
        protocol: Protocol::Kwp,
        ecu_address: cfg.ecu_address,
    };
    for routine in BMT_REMOTE_LEARNING {
        let reqs = ecu.request_by_bytes(&hex(&routine)).unwrap();
        assert_eq!(reqs.len(), 1, "{routine:02X?} in BMT definition");
        assert_eq!(classify(&target, &routine).unwrap(), Tier::Service);
    }
    let tp = ecu.request_by_bytes("21A4").unwrap();
    assert_eq!(tp.len(), 1);
    assert_eq!(classify(&target, &tp[0].sent_bytes.0).unwrap(), Tier::Read);
}

/// Step 7: in both BMT definitions the name guard flags the immobilizer outputs of
/// "COMMANDE DES ACTUATEURS" by their list texts (0xA6 "Ligne Transpondeur", 0xAB "Led
/// Verlog"), not the other outputs, and not the two remote-learning routines.
#[test]
fn bmt_definition_guard_flags_immobilizer_actuator_outputs() {
    for file in ["BMT_X65_ph1_p.xml", "BMT_SAGEM.xml"] {
        let Some(bytes) = common::ddt_file(file) else {
            continue;
        };
        let ecu = EcuDef::from_xml_bytes(&bytes).unwrap();
        let t = Target {
            protocol: Protocol::Kwp,
            ecu_address: 0x26,
        };
        let named = |p: &[u8]| kline_core::spec::immobilizer_named_requests(&ecu, &t, p);
        assert!(!named(&[0x31, 0xA6, 0x00, 0x01]).is_empty(), "{file}");
        assert!(!named(&[0x31, 0xAB, 0x00, 0x01]).is_empty(), "{file}");
        assert!(named(&[0x31, 0xA0, 0x00, 0x01]).is_empty(), "{file}");
        assert!(named(&[0x31, 0xB3, 0x00, 0x01]).is_empty(), "{file}");
        for r in BMT_REMOTE_LEARNING {
            assert!(named(&r).is_empty(), "{file} {r:02X?}");
        }
    }
}

/// Security review L3 with the coordinator's constraint: "LANCEMENT DES ROUTINES" (SentBytes
/// `3100000000`, fixed prefix `31`) has the items "MSB/LSB Code manuel", which the new
/// "code manuel" keyword hits. Its flag must not refuse the remote learning, the actuator tests
/// or any other `31 xx`: only its whole template is flagged.
#[test]
fn bmt_code_manuel_routine_template_does_not_block_other_31_requests() {
    for file in ["BMT_X65_ph1_p.xml", "BMT_SAGEM.xml"] {
        let Some(bytes) = common::ddt_file(file) else {
            continue;
        };
        let ecu = EcuDef::from_xml_bytes(&bytes).unwrap();
        let g = kline_core::spec::definition_guard(&ecu);
        for addr in [0x26u8, 0x40] {
            let t = Target {
                protocol: Protocol::Kwp,
                ecu_address: addr,
            };
            let tier = |p: &[u8]| kline_core::safety::tier_of_in(&g, &t, p);
            assert_eq!(
                kline_core::spec::immobilizer_named_requests(&ecu, &t, &[0x31, 0, 0, 0, 0]),
                vec!["LANCEMENT DES ROUTINES".to_string()],
                "{file}"
            );
            for r in BMT_REMOTE_LEARNING {
                assert_eq!(
                    tier(&r).unwrap(),
                    Tier::Service,
                    "{file} {addr:02X} {r:02X?}"
                );
            }
            for out in 0xA0u8..=0xB3 {
                let p = [0x31, out, 0x00, 0x01];
                if [0xA6, 0xAB, 0xAC].contains(&out) {
                    assert!(
                        tier(&p).unwrap_err().reason.is_forbidden(),
                        "{file} {p:02X?}"
                    );
                } else if [0xA4, 0xA5].contains(&out) {
                    assert_eq!(
                        tier(&p).unwrap(),
                        Tier::Expert,
                        "{file} {addr:02X} {p:02X?}"
                    );
                } else {
                    assert_eq!(
                        tier(&p).unwrap(),
                        Tier::Service,
                        "{file} {addr:02X} {p:02X?}"
                    );
                }
            }
            assert_eq!(
                tier(&[0x31, 0xC1, 0x00, 0x00, 0x00]).unwrap_err().reason,
                kline_core::safety::DenyReason::Immobilizer,
                "{file}"
            );
            // Undocumented routines stay Expert, not refused by the name guard.
            assert_eq!(tier(&[0x31, 0x10, 0x00, 0x00, 0x00]).unwrap(), Tier::Expert);
        }
    }
}

/// `SIRIUS_OUTPUT_IDS` equals the values of the definition's "Output Temporary Control List"
/// (skipped without ddt-xml/).
#[test]
fn sirius_output_ids_match_the_definition() {
    let Some(bytes) = common::ddt_file("SIRIUS32___EMS3132___Soft_RE1_maj.xml") else {
        eprintln!("skipped: ddt-xml/SIRIUS32___EMS3132___Soft_RE1_maj.xml absent");
        return;
    };
    let text = String::from_utf8_lossy(&bytes);
    let start = text
        .find("<Data Name=\"Output Temporary Control List\">")
        .expect("list present");
    let body = &text[start..];
    let body = &body[..body.find("</Data>").expect("list end")];
    let mut ids: Vec<u8> = body
        .split("Value=\"")
        .skip(1)
        .map(|t| t.split('"').next().unwrap().parse().unwrap())
        .collect();
    ids.sort_unstable();
    let mut allow = kline_core::safety::SIRIUS_OUTPUT_IDS.to_vec();
    allow.sort_unstable();
    assert_eq!(ids, allow);
}
