//! Every request of the car's airbag definitions (AB70_X65.xml, AB80_X65_X90_V1.xml; `spec`
//! feature, read from ddt-xml/, skipped when absent) against the airbag rules, with the tier the
//! command inventory of 2026-11-23 (COMMAND-INVENTORY.md section 3) gives each one, where the
//! owner decision of 2026-11-23 ("do not touch the airbag yet": no fault clear, no output
//! control, no session other than 10 81, no firing-line or crash-data write) is stricter than
//! the inventory's recommendation.
#![cfg(feature = "spec")]

mod common;

use kline_core::safety::{classify_in, DenyReason, Protocol, Target, Tier};
use kline_core::spec::definition_guard;
use kline_spec::EcuDef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    Read,
    ConfigWrite,
    Expert,
    Forbidden,
}

use Want::{ConfigWrite, Expert, Forbidden};

/// Every non-read request of both files and the enumerated values of their sent parameters
/// (inventory ids AB70-01..14, AB80-01..33), exact bytes in hex.
const NON_READS: &[(&str, &str, Want)] = &[
    // AB70_X65.xml
    ("AB70-01", "14FF00", Forbidden), // inventory: Tier 1; owner: no airbag fault clear yet
    ("AB70-02", "3BD287284D194D194D194D1900", Forbidden),
    ("AB70-03", "3BD000", Forbidden),
    ("AB70-04", "3BD0FF", Forbidden),
    ("AB70-05", "3BC0FFFFFF", ConfigWrite),
    ("AB70-06", "3BB0FFFFFF", ConfigWrite),
    ("AB70-07", "3BD100", Expert),
    ("AB70-08", "3BD1F0", Expert),
    ("AB70-09", "3BD1FF", Expert),
    ("AB70-10", "230000B60019", Expert),
    ("AB70-11", "1080", Forbidden),
    ("AB70-12", "10A0", Forbidden), // inventory: "not Tier 0"; owner: only 10 81
    ("AB70-13", "10B0", Forbidden), // inventory: Expert; owner: only 10 81
    ("AB70-14", "10C0", Forbidden), // inventory: Expert; owner: only 10 81
    // AB80_X65_X90_V1.xml
    ("AB80-01", "3BD528", Expert),
    ("AB80-02", "3BD53C", Expert),
    ("AB80-03", "3BD550", Expert),
    ("AB80-04", "3BD564", Expert),
    ("AB80-05", "3BD578", Expert),
    ("AB80-06", "3BD58C", Expert),
    ("AB80-07", "3BD5A0", Expert),
    ("AB80-08", "3BD5B4", Expert),
    ("AB80-09", "3BD5C8", Expert),
    ("AB80-10", "308000", Forbidden),
    ("AB80-11", "308001", Forbidden), // inventory: Expert; owner: any 30
    ("AB80-12", "308012", Forbidden), // inventory: Expert; owner: any 30
    ("AB80-13", "3BD419", Expert),
    ("AB80-14", "3BD1F0", Expert),
    ("AB80-15", "3BD1FF", Expert),
    ("AB80-16", "3BD60F200000", Forbidden),
    ("AB80-17", "3BD20000000000000000000000", Forbidden),
    ("AB80-18", "3BD30000000000000000000000", Forbidden),
    ("AB80-19", "3BB0000000", ConfigWrite),
    ("AB80-20", "3BC0000000", ConfigWrite),
    ("AB80-21", "3BD055", Forbidden), // inventory: Expert; owner: passenger inhibit writes
    ("AB80-22", "3BD0AA", Forbidden), // inventory: Expert; owner: passenger inhibit writes
    ("AB80-23", "230000B60019", Expert),
    ("AB80-24", "14FF00", Forbidden),
    ("AB80-25", "3BE000000000", Forbidden),
    ("AB80-26", "3BE022041998", Forbidden),
    ("AB80-27", "3BD7", Forbidden),
    ("AB80-28", "3BD000", Forbidden),
    ("AB80-29", "3BD0FF", Forbidden),
    ("AB80-30", "1080", Forbidden),
    ("AB80-31", "10A0", Forbidden),
    ("AB80-32", "10B0", Forbidden),
    ("AB80-33", "10C0", Forbidden),
];

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn got(def: &EcuDef, p: &[u8]) -> Want {
    let t = Target {
        protocol: Protocol::Kwp,
        ecu_address: 0x2C,
        keywords: Some((0xD0, 0x8F)),
    };
    match classify_in(&definition_guard(def), &t, p) {
        Ok(Tier::Read) => Want::Read,
        Ok(Tier::ConfigWrite) => ConfigWrite,
        Ok(tier) => panic!("{p:02X?}: unexpected {tier:?}"),
        Err(d) => match d.reason {
            DenyReason::SafetySystemForbidden { .. } => Forbidden,
            DenyReason::ServiceNotAllowed { .. } | DenyReason::ParametersNotAllowed { .. } => {
                Expert
            }
            r => panic!("{p:02X?}: {r:?}"),
        },
    }
}

#[test]
fn every_airbag_request_has_its_inventory_tier() {
    for (file, prefix) in [("AB70_X65.xml", "AB70"), ("AB80_X65_X90_V1.xml", "AB80")] {
        let Some(bytes) = common::ddt_file(file) else {
            continue;
        };
        let def = EcuDef::from_xml_bytes(&bytes).unwrap();
        assert_eq!(
            def.k.as_ref().and_then(|k| k.key_words()),
            Some((0xD0, 0x8F)),
            "{file}"
        );
        let rows: Vec<_> = NON_READS
            .iter()
            .filter(|(id, _, _)| id.starts_with(prefix))
            .collect();
        // Every request of the file: a read, or one of the inventory rows.
        for r in &def.requests {
            let p = &r.sent_bytes.0;
            let h: String = p.iter().map(|b| format!("{b:02X}")).collect();
            if p[0] == 0x21 {
                assert_eq!(p.len(), 2, "{file} {}", r.name);
                assert_eq!(got(&def, p), Want::Read, "{file} {}", r.name);
            } else {
                assert!(
                    rows.iter().any(|(_, b, _)| *b == h),
                    "{file} {} {h}: not in the inventory table",
                    r.name
                );
            }
        }
        // Every inventory row (template and enumerated values) gets its tier.
        for (id, b, want) in rows {
            assert_eq!(got(&def, &hex(b)), *want, "{file} {id} {b}");
        }
    }
}
