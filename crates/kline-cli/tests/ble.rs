//! `kline --transport ble read`: through a kline-bridge running on kline-sim (feature `ble`).
#![cfg(feature = "ble")]

#[path = "../../kline-host/tests/support/ble_sim.rs"]
mod ble_sim;
mod common;

use common::*;
use kline_bridge::gate::Role;
use kline_host::exit;
use kline_sim::line::VirtualLine;

fn opts() -> Opts {
    Opts {
        ble: Some(Box::new(|| {
            Ok(Box::new(ble_sim::SimBridge::new(Role::Agent)) as Box<dyn kline_host::ble::BleLink>)
        })),
        ..Opts::default()
    }
}

fn sirius_xml() -> String {
    format!("{}/bridge_sirius.xml", ble_sim::FIXTURES)
}

fn unused_line() -> VirtualLine {
    VirtualLine::with_ecu(sirius_ecu())
}

#[test]
fn read_over_ble_decodes_with_the_definition() {
    let xml = sirius_xml();
    let r = run_line_with(
        unused_line(),
        &[
            "--json",
            "--transport",
            "ble",
            "read",
            "--xml",
            &xml,
            "21A9",
            "21AA",
        ],
        opts(),
    );
    assert_eq!(r.code, exit::OK, "{}\n{}", r.stdout, r.stderr);
    assert!(!r.opened, "no serial port is opened");
    let reads = r.event("read");
    assert_eq!(reads.len(), 2);
    assert_eq!(reads[0]["transport"], "ble");
    assert_eq!(reads[0]["request"], "21A9");
    let it = items(&reads[0]["decoded"][0]);
    assert_eq!(it["Vehicle speed"], serde_json::json!(0.0));
}

#[test]
fn non_reads_are_refused_before_the_link_is_used() {
    let xml = sirius_xml();
    let r = run_line_with(
        unused_line(),
        &[
            "--json",
            "--transport",
            "ble",
            "read",
            "--xml",
            &xml,
            "140000",
        ],
        opts(),
    );
    assert_eq!(r.code, exit::REFUSED, "{}\n{}", r.stdout, r.stderr);
}

#[test]
fn other_commands_are_serial_only() {
    let xml = sirius_xml();
    let r = run_line_with(
        unused_line(),
        &["--transport", "ble", "connect", "--xml", &xml],
        opts(),
    );
    assert_eq!(r.code, exit::USAGE, "{}\n{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("supports `read` only"), "{}", r.stderr);
    assert!(!r.opened);
}

#[test]
fn without_a_radio_backend_the_error_says_so() {
    let xml = sirius_xml();
    let r = run_line_with(
        unused_line(),
        &["--transport", "ble", "read", "--xml", &xml, "21A9"],
        Opts::default(),
    );
    assert_eq!(r.code, exit::PORT, "{}\n{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("no BLE radio backend"), "{}", r.stderr);
}
