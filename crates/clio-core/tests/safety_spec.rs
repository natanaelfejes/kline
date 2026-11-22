//! Every request of the car's definitions against the safety policy (`spec` feature, reads
//! ddt-xml/, skipped when absent). Expected allowed sets are written out by hand here, so a
//! policy change that unlocks anything else fails this test.
#![cfg(feature = "spec")]

mod common;

use clio_core::safety::{classify, Protocol, Target, Tier, BMT_REMOTE_LEARNING};
use clio_core::spec::link_config;
use clio_spec::EcuDef;

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
        // Output control, clear DTC, adaptation reset, stop functions. The 31 A0 xx template
        // (31A000) and the other routines stay denied.
        service: &[
            "30D0000A", "30D00100", "30D01100", "140000", "11FF", "32A000",
        ],
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
