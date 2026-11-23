//! The Bosch 5.3 ABS forbidden list against DDT2000's Bosch 5.3 definitions (`spec` feature,
//! reads ddt-xml/, skipped when absent). Owner decision 2026-11-23: factory fill and bleed
//! are forbidden by name, and every `41` / `71` on the ABS is forbidden.
#![cfg(feature = "spec")]

mod common;

use kline_core::safety::{
    bosch_abs_fill_bleed_names, classify, DenyReason, Protocol, Target, Tier,
    BOSCH_ABS_FILL_BLEED_DOWNLOADS, BOSCH_ABS_FILL_BLEED_DOWNLOAD_HEADER,
    BOSCH_ABS_FILL_BLEED_FRAMES, BOSCH_ABS_KEYWORDS,
};
use kline_spec::EcuDef;

const ABS: Target = Target {
    protocol: Protocol::Iso8,
    ecu_address: 0x01,
    keywords: Some(BOSCH_ABS_KEYWORDS),
};

fn load(name: &str) -> Option<EcuDef> {
    let bytes = common::ddt_file(name)?;
    Some(EcuDef::from_xml_bytes(&bytes).unwrap())
}

fn is_fill_or_bleed(name: &str) -> bool {
    name.starts_with("MES_PURGE") || name.starts_with("MES_REMPLISSAGE")
}

/// The hard-coded names and bytes equal X24_ABS_Bosch5.3, both ways: every MES_PURGE_* /
/// MES_REMPLISSAGE* request is in the table with its exact bytes, every table entry is in the
/// XML, and the download header picks exactly DOWNLOAD_PURGE and DOWNLOAD_remplissage_usine.
#[test]
fn fill_and_bleed_table_matches_the_x24_definition() {
    let Some(def) = load("X24_ABS_Bosch5.3.xml") else {
        return;
    };
    let mut from_xml: Vec<(String, Vec<u8>)> = def
        .requests
        .iter()
        .filter(|r| is_fill_or_bleed(&r.name))
        .map(|r| (r.name.clone(), r.sent_bytes.0.clone()))
        .collect();
    let mut table: Vec<(String, Vec<u8>)> = BOSCH_ABS_FILL_BLEED_FRAMES
        .iter()
        .map(|(n, b)| (n.to_string(), b.to_vec()))
        .collect();
    from_xml.sort();
    table.sort();
    assert_eq!(table, from_xml);
    let mut downloads: Vec<&str> = def
        .requests
        .iter()
        .filter(|r| r.sent_bytes.0.first() == Some(&0x41))
        .filter(|r| {
            r.sent_bytes
                .0
                .starts_with(&BOSCH_ABS_FILL_BLEED_DOWNLOAD_HEADER)
        })
        .map(|r| r.name.as_str())
        .collect();
    downloads.sort();
    assert_eq!(downloads, BOSCH_ABS_FILL_BLEED_DOWNLOADS);
    // The refusal names each of them.
    for r in &def.requests {
        let names = bosch_abs_fill_bleed_names(&r.sent_bytes.0);
        if is_fill_or_bleed(&r.name) || downloads.contains(&r.name.as_str()) {
            assert!(names.contains(&r.name.as_str()), "{}: {names:?}", r.name);
            match classify(&ABS, &r.sent_bytes.0) {
                Err(d) => assert_eq!(
                    d.reason,
                    DenyReason::BoschAbsRoutine {
                        service: r.sent_bytes.0[0],
                        names
                    },
                    "{}",
                    r.name
                ),
                ok => panic!("{}: {ok:?}", r.name),
            }
        } else {
            assert!(names.is_empty(), "{}: {names:?}", r.name);
        }
    }
}

/// Every request of the Bosch 5.3 definitions at the ABS: `51`/`58` Tier 0, `45` Tier 1, `36`
/// a link frame, every `41`/`71` forbidden, nothing else in the files.
#[test]
fn every_bosch_53_request_has_its_tier() {
    for file in ["X24_ABS_Bosch5.3.xml", "J64_Bosch5.3_4X4_V2.xml"] {
        let Some(def) = load(file) else { continue };
        let (mut routines, mut seen) = (0, 0);
        for r in &def.requests {
            let p = &r.sent_bytes.0;
            let got = classify(&ABS, p);
            seen += 1;
            match p[0] {
                0x51 | 0x58 => assert_eq!(got, Ok(Tier::Read), "{file} {}", r.name),
                0x45 => assert_eq!(got, Ok(Tier::Service), "{file} {}", r.name),
                0x36 => assert_eq!(
                    got.unwrap_err().reason,
                    DenyReason::ProtocolReserved { service: 0x36 }
                ),
                0x41 | 0x71 => {
                    routines += 1;
                    assert!(
                        matches!(got, Err(ref d) if matches!(d.reason, DenyReason::BoschAbsRoutine { .. })),
                        "{file} {}: {got:?}",
                        r.name
                    );
                }
                s => panic!("{file} {}: unexpected service {s:02X}", r.name),
            }
        }
        assert!(routines > 0 && seen > routines, "{file}");
    }
}
