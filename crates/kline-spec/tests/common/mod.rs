//! Shared test helpers.
#![allow(dead_code)]

use std::path::PathBuf;

use kline_spec::EcuDef;

/// Directory with the (gitignored) Renault definition files. `KLINE_DDT_XML` overrides the
/// default `<repo>/ddt-xml`.
pub fn ddt_xml_dir() -> PathBuf {
    match std::env::var_os("KLINE_DDT_XML") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ddt-xml"),
    }
}

/// Bytes of a Renault file, or `None` (with a SKIPPED message) when it is not available.
/// Rule 1 of docs/SAFETY.md: these files never enter git, so tests skip instead of failing.
pub fn ddt_file(name: &str) -> Option<Vec<u8>> {
    let path = ddt_xml_dir().join(name);
    match std::fs::read(&path) {
        Ok(b) => Some(b),
        Err(e) => {
            eprintln!(
                "SKIPPED: {} not available ({e}); put the DDT2000 XML files in ddt-xml/",
                path.display()
            );
            None
        }
    }
}

/// A Renault file parsed with ddt4all semantics (the golden tests pin ddt4all output).
pub fn ddt_ecu(name: &str) -> Option<EcuDef> {
    ddt_ecu_as(name, kline_spec::Semantics::Ddt4all)
}

pub fn ddt_ecu_as(name: &str, sem: kline_spec::Semantics) -> Option<EcuDef> {
    let bytes = ddt_file(name)?;
    Some(EcuDef::from_xml_bytes_as(&bytes, sem).unwrap_or_else(|e| panic!("{name}: {e}")))
}

pub fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

pub fn display_of(items: &[kline_spec::DecodedItem], name: &str) -> Option<String> {
    items
        .iter()
        .find(|i| i.name == name)
        .unwrap_or_else(|| panic!("no decoded item {name:?}"))
        .display
        .clone()
}

pub fn number_of(items: &[kline_spec::DecodedItem], name: &str) -> f64 {
    match &items.iter().find(|i| i.name == name).expect(name).value {
        Some(kline_spec::Value::Number(x)) => *x,
        other => panic!("{name}: expected a number, got {other:?}"),
    }
}

pub fn raw_of(items: &[kline_spec::DecodedItem], name: &str) -> Option<kline_spec::Raw> {
    items
        .iter()
        .find(|i| i.name == name)
        .expect(name)
        .raw
        .clone()
}
