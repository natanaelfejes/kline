//! Shared helpers for the clio-sim integration tests.
#![allow(dead_code)]

use std::collections::BTreeMap;

use clio_core::frame::Reply;
use clio_core::session::SessionOutcome;
use clio_core::LinkConfig;
use clio_sim::driver::Driver;
use clio_sim::ecu::{EcuProfile, SpecEcu};
use clio_sim::journal::Journal;
use clio_sim::line::VirtualLine;
use clio_sim::scenario::Scenario;
use clio_spec::{EcuDef, Value};

pub fn fixture(name: &str) -> EcuDef {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    EcuDef::from_xml_bytes(&std::fs::read(&path).unwrap()).unwrap()
}

pub fn sirius() -> EcuDef {
    fixture("sirius_min.xml")
}

pub fn bmt() -> EcuDef {
    fixture("bmt_min.xml")
}

pub fn safir() -> EcuDef {
    fixture("safir_min.xml")
}

/// Link config as the CLI would build it: clio-core's mapping of the definition.
pub fn cfg(def: &EcuDef) -> LinkConfig {
    clio_core::spec::link_config(def).unwrap()
}

/// The values the Sirius fixture ECU reports. All on the Step grids.
pub fn sirius_scenario() -> Scenario {
    Scenario::new()
        .text("Calibration", "SIM-0001")
        .set("Supplier code", Value::Hex("0A1B".into()))
        .number("Engine speed", 850.0)
        .number("Vehicle speed", 0.0)
        .number("Injection time", 2400.0)
        .number("Battery voltage", 13.875)
        .number("Idle setpoint", 800.0)
        .text("Idle switch", "closed")
        .number("Coolant temperature", 88.0)
        .number("Air temperature", -5.5)
        .number("Throttle position", 4.5)
}

pub fn sirius_ecu() -> SpecEcu {
    SpecEcu::new(EcuProfile::sirius32(), sirius(), sirius_scenario()).unwrap()
}

/// Driver over an in-process line, plus the ECU's journal.
pub fn drive(ecu: SpecEcu) -> (Driver<VirtualLine>, Journal) {
    let j = ecu.journal();
    (Driver::new(VirtualLine::with_ecu(ecu)), j)
}

pub fn positive(o: &SessionOutcome) -> Vec<u8> {
    match o {
        SessionOutcome::Reply(Reply::Positive(p)) => p.clone(),
        other => panic!("expected a positive reply, got {other:?}"),
    }
}

/// decode_checked of `payload` as the reply to the request `name`, as name -> value.
pub fn decode(def: &EcuDef, name: &str, payload: &[u8]) -> BTreeMap<String, Value> {
    let req = def.request(name).unwrap();
    req.decode_checked(def, payload)
        .unwrap()
        .into_iter()
        .map(|d| (d.name.clone(), d.value.expect("value")))
        .collect()
}

/// The scenario values of the items of request `name`.
pub fn expected(def: &EcuDef, name: &str, sc: &Scenario) -> BTreeMap<String, Value> {
    def.request(name)
        .unwrap()
        .received_items
        .iter()
        .map(|i| (i.name.clone(), sc.get(&i.name).unwrap().clone()))
        .collect()
}

/// Directory with the (gitignored) Renault definitions; `CLIO_DDT_XML` overrides it.
pub fn ddt_file(name: &str) -> Option<Vec<u8>> {
    let dir = match std::env::var_os("CLIO_DDT_XML") {
        Some(p) => std::path::PathBuf::from(p),
        None => std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ddt-xml"),
    };
    let path = dir.join(name);
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
