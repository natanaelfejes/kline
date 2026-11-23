//! `--transport ble` in a build without the `ble` feature: a clear usage error, no port opened.
#![cfg(not(feature = "ble"))]

mod common;

use common::*;
use kline_host::exit;

#[test]
fn ble_transport_needs_the_feature() {
    let xml = xml("sirius_cli.xml");
    let (r, j) = run(
        sirius_ecu(),
        &["--transport", "ble", "read", "--xml", &xml, "21A9"],
    );
    assert_eq!(r.code, exit::USAGE, "{}\n{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("cargo feature `ble`"), "{}", r.stderr);
    assert!(!r.opened);
    j.assert_no_writes();
}
