//! The bridge, which has no definitions, decides every request of the car's real DDT2000
//! definitions exactly as the CLI does with those definitions loaded (owner decision
//! 2026-11-23: protection over BLE no weaker than the CLI). Reads `ddt-xml/` (gitignored,
//! SAFETY.md rule 1); skipped, not failed, when the files are absent.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use kline_bridge::gate;
use kline_bridge::guard_rules::NAME_GUARD_PREFIXES;
use kline_bridge::profile::{EcuId, ALL_ECUS};
use kline_core::safety::{tier_of_in, DefinitionGuard, Target, Tier};
use kline_spec::EcuDef;

/// The definitions the CLI loads for each of the car's ECUs (guard_rules.rs table).
fn car_files(ecu: EcuId) -> &'static [&'static str] {
    match ecu {
        EcuId::Sirius => &["SIRIUS32___EMS3132___Soft_RE1_maj.xml"],
        EcuId::Bmt => &["BMT_X65_ph1_p.xml", "BMT_SAGEM.xml"],
        EcuId::Abs => &["X24_ABS_Bosch5.3.xml", "J64_Bosch5.3_4X4_V2.xml"],
        EcuId::Airbag => &["AB70_X65.xml", "AB80_X65_X90_V1.xml"],
    }
}

fn defs_dir() -> Option<PathBuf> {
    let d = std::env::var_os("KLINE_DDT_XML")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ddt-xml"));
    let all = ALL_ECUS
        .iter()
        .flat_map(|e| car_files(*e).iter())
        .all(|f| d.join(f).exists());
    if all {
        Some(d)
    } else {
        eprintln!(
            "SKIPPED: {} lacks the car's DDT2000 XML files; put them in ddt-xml/",
            d.display()
        );
        None
    }
}

fn load(dir: &Path, ecu: EcuId) -> Vec<EcuDef> {
    car_files(ecu)
        .iter()
        .map(|f| EcuDef::from_xml_bytes(&std::fs::read(dir.join(f)).unwrap()).unwrap())
        .collect()
}

/// The CLI's guard with all candidate definitions of `ecu` loaded (they only add refusals).
fn cli_guard(defs: &[EcuDef]) -> DefinitionGuard {
    let mut g = DefinitionGuard::default();
    for d in defs {
        g.merge(kline_core::spec::definition_guard(d));
    }
    g
}

type Decision = Result<Tier, bool>;

fn decide(g: &DefinitionGuard, t: &Target, p: &[u8]) -> Decision {
    tier_of_in(g, t, p).map_err(|d| d.reason.is_forbidden())
}

#[test]
fn compiled_name_guard_is_the_definitions_guard() {
    let Some(dir) = defs_dir() else { return };
    for ecu in ALL_ECUS {
        let defs = load(&dir, ecu);
        let cli = cli_guard(&defs);
        let from_defs: BTreeSet<Vec<u8>> = cli.flagged().iter().map(|(p, _)| p.clone()).collect();
        let compiled: BTreeSet<Vec<u8>> = NAME_GUARD_PREFIXES
            .iter()
            .filter(|(a, _)| *a == ecu.address())
            .map(|(_, p)| p.to_vec())
            .collect();
        assert_eq!(compiled, from_defs, "{ecu:?}");
        assert_eq!(gate::guard(ecu).class, cli.class, "{ecu:?}");
        for d in &defs {
            assert_eq!(
                d.function.as_ref().map(|f| f.address),
                Some(ecu.address()),
                "{}",
                d.name
            );
        }
    }
}

#[test]
fn bridge_decides_every_definition_request_like_the_cli() {
    let Some(dir) = defs_dir() else { return };
    let mut checked = 0;
    for ecu in ALL_ECUS {
        let defs = load(&dir, ecu);
        let cli = cli_guard(&defs);
        let bridge = gate::guard(ecu);
        let t = gate::target(ecu);
        for d in &defs {
            // Each definition alone: the bridge refuses at least what the CLI refuses with it.
            let alone = kline_core::spec::definition_guard(d);
            for r in &d.requests {
                let sent = &r.sent_bytes.0;
                if sent.is_empty() {
                    continue;
                }
                let mut variants = vec![sent.clone()];
                let mut extra = sent.clone();
                extra.push(0x01);
                variants.push(extra);
                for p in variants {
                    let b = decide(&bridge, &t, &p);
                    assert_eq!(b, decide(&cli, &t, &p), "{ecu:?} {} {p:02X?}", r.name);
                    if decide(&alone, &t, &p) == Err(true) {
                        assert_eq!(b, Err(true), "{ecu:?} {} {p:02X?}", r.name);
                    }
                    checked += 1;
                }
            }
        }
    }
    eprintln!("{checked} requests compared");
    assert!(checked > 100);
}

/// The compiled link settings are the definitions' `<K>` values.
#[test]
fn link_settings_match_the_definitions() {
    let Some(dir) = defs_dir() else { return };
    for ecu in ALL_ECUS {
        let def = &load(&dir, ecu)[0];
        let from_def = kline_core::spec::link_config(def).unwrap();
        let ours = ecu.link_config();
        assert_eq!(ours.timing, from_def.timing, "{ecu:?}");
        assert_eq!(
            ours.expected_keywords, from_def.expected_keywords,
            "{ecu:?}"
        );
        assert_eq!(ours.ecu_address, from_def.ecu_address, "{ecu:?}");
    }
}
