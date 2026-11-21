//! Every request of the car's definitions against the safety policy (`spec` feature, reads
//! ddt-xml/, skipped when absent). Expected allowed sets are written out by hand here, so a
//! policy change that unlocks anything else fails this test.
#![cfg(feature = "spec")]

mod common;

use clio_core::safety::{
    check, check_approved, Approval, ApprovedRoutine, DenyReason, Protocol, Target,
};
use clio_core::spec::link_config;
use clio_spec::EcuDef;

struct Case {
    file: &'static str,
    /// SentBytes (uppercase hex) that must be allowed. Everything else must be denied.
    allowed: &'static [&'static str],
    /// Number of requests in the file, to notice parser or file changes.
    requests: usize,
}

const CASES: [Case; 4] = [
    Case {
        file: "SIRIUS32___EMS3132___Soft_RE1_maj.xml",
        allowed: &[
            "10A0", "2180", "2181", "2182", "21A0", "21A1", "21A2", "21A3", "21A4", "21A5", "21A6",
            "21A7", "21A8", "21A9", "21AA", "21AB", "21AC", "21AD", "21AE", "21AF", "21BD", "21C6",
            "21C7",
        ],
        requests: 34,
    },
    Case {
        file: "BMT_X65_ph1_p.xml",
        // 21A4 (transponder states) is denied: immobilizer related.
        allowed: &["2180", "21A0", "21A1", "21A2", "21A3", "21A7", "21A8"],
        requests: 15,
    },
    Case {
        file: "Safir_1.xml",
        allowed: &["21", "3350", "3351", "3352", "3354"],
        requests: 15,
    },
    Case {
        file: "Safir_2.xml",
        allowed: &["21", "3350", "3351", "3352", "3354"],
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
        let mut seen_allowed = Vec::new();
        for r in &ecu.requests {
            let b = &r.sent_bytes.0;
            let h = hex(b);
            let verdict = check(&target, b);
            let label = match &verdict {
                Ok(_) => "ALLOW".to_string(),
                Err(d) => format!("DENY {:?}", d.reason),
            };
            eprintln!("  {h:<12} {label:<45} {}", r.name);
            if case.allowed.contains(&h.as_str()) {
                assert!(
                    verdict.is_ok(),
                    "{}: {h} ({}) must be allowed",
                    case.file,
                    r.name
                );
                seen_allowed.push(h);
            } else {
                assert!(
                    verdict.is_err(),
                    "{}: {h} ({}) must be denied",
                    case.file,
                    r.name
                );
            }
        }
        for a in case.allowed {
            assert!(
                seen_allowed.iter().any(|s| s == a),
                "{}: {a} not in file",
                case.file
            );
        }
    }
}

#[test]
fn bmt_routines_only_with_approval() {
    let Some(bytes) = common::ddt_file("BMT_X65_ph1_p.xml") else {
        return;
    };
    let ecu = EcuDef::from_xml_bytes(&bytes).unwrap();
    let cfg = link_config(&ecu).unwrap();
    let target = Target {
        protocol: Protocol::Kwp,
        ecu_address: cfg.ecu_address,
    };
    for routine in ApprovedRoutine::ALL {
        let reqs = ecu.request_by_bytes(&hex(routine.payload())).unwrap();
        assert_eq!(reqs.len(), 1, "{routine:?} in BMT definition");
        assert_eq!(
            check(&target, routine.payload()).unwrap_err().reason,
            DenyReason::ApprovalRequired
        );
        let approval =
            Approval::from_typed_confirmation(routine, routine.confirmation_phrase()).unwrap();
        assert_eq!(
            check_approved(&target, approval).unwrap().payload(),
            routine.payload()
        );
    }
    let tp = ecu.request_by_bytes("21A4").unwrap();
    assert_eq!(tp.len(), 1);
    assert_eq!(
        check(&target, &tp[0].sent_bytes.0).unwrap_err().reason,
        DenyReason::Immobilizer
    );
}
