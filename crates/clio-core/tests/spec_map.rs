//! `spec` feature: EcuDef -> LinkConfig mapping.
#![cfg(feature = "spec")]

mod common;

use clio_core::spec::{link_config, SpecMapError};
use clio_core::{InitKind, InitVariant, KTiming, LinkConfig};
use clio_spec::EcuDef;

fn fixture(name: &str) -> LinkConfig {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let ecu = EcuDef::from_xml_bytes(&std::fs::read(&path).unwrap()).unwrap();
    link_config(&ecu).unwrap()
}

#[test]
fn multipoint_declared_and_effective_timing() {
    let mut cfg = fixture("kwp_multipoint.xml");
    assert_eq!(cfg.ecu_address, 0x7A);
    assert_eq!(cfg.tool_address, 0xF1);
    assert_eq!(cfg.variant, InitVariant::SingleEcu);
    assert_eq!(cfg.expected_keywords, Some((0xD0, 0x8F)));
    let declared = KTiming {
        baud: 10_400,
        init: InitKind::KwpFiveBaud { multipoint: true },
        kw2_delay_ms: 40,
        first_request_ms: 90,
        inter_byte_ms: 3,
        reply_to_request_ms: 6,
        request_to_reply_timeout_ms: 800,
        tester_present_ms: 2500,
        on_error_ms: 350,
        kw2_timeout_ms: 900,
    };
    assert_eq!(cfg.timing, declared);
    // Single ECU (default): Multipoint cleared, declared values, plain address.
    assert_eq!(
        cfg.effective_timing(),
        KTiming {
            init: InitKind::KwpFiveBaud { multipoint: false },
            ..declared
        }
    );
    assert_eq!(cfg.five_baud_address_byte(), 0x7A);
    // Several ECUs: DDT2000's fixed Multipoint values.
    cfg.variant = InitVariant::MultiEcu;
    assert_eq!(
        cfg.effective_timing(),
        KTiming {
            baud: 10_400,
            init: InitKind::KwpFiveBaud { multipoint: true },
            kw2_delay_ms: 25,
            first_request_ms: 20,
            inter_byte_ms: 7,
            reply_to_request_ms: 6,
            request_to_reply_timeout_ms: 300,
            tester_present_ms: 3000,
            on_error_ms: 350,
            kw2_timeout_ms: 300,
        }
    );
    assert_eq!(cfg.five_baud_address_byte(), 0xFA);
    assert_eq!(cfg.effective_timing().kw2_read_window_ms(), 400);
}

#[test]
fn iso8_class_defaults_and_baud() {
    let mut cfg = fixture("iso8_sparse.xml");
    assert_eq!(cfg.five_baud_address_byte(), 0x7A);
    assert_eq!(
        cfg.timing,
        KTiming {
            // XML says 10417; DDT2000 effectively runs 10400.
            baud: 10_400,
            init: InitKind::Iso8FiveBaud,
            kw2_delay_ms: 25,
            first_request_ms: 200,
            inter_byte_ms: 2,
            reply_to_request_ms: 7,
            request_to_reply_timeout_ms: 100,
            tester_present_ms: 3000,
            on_error_ms: 1000,
            kw2_timeout_ms: 1000,
        }
    );
    assert_eq!(cfg.effective_timing(), cfg.timing);
    // The variant does not change an ISO8 link.
    cfg.variant = InitVariant::MultiEcu;
    assert_eq!(cfg.effective_timing(), cfg.timing);
    assert_eq!(cfg.five_baud_address_byte(), 0x7A);
    assert_eq!(cfg.timing.kw2_read_window_ms(), 1100);
}

#[test]
fn fast_init_mapping() {
    let cfg = fixture("kwp_fast.xml");
    assert_eq!(cfg.timing.init, InitKind::KwpFast { multipoint: false });
    assert_eq!(cfg.timing.inter_byte_ms, 1);
    assert_eq!(cfg.timing.request_to_reply_timeout_ms, 250);
    assert_eq!(cfg.expected_keywords, Some((0xD0, 0x8F)));
}

#[test]
fn missing_sections() {
    let ecu = EcuDef::from_xml_bytes(
        br#"<Target Name="x" xmlns="http://www-diag.renault.com/2002/ECU"><Requests/></Target>"#,
    )
    .unwrap();
    assert_eq!(link_config(&ecu), Err(SpecMapError::NoKLine));
}

#[test]
fn renault_definitions() {
    let cases = [
        (
            "SIRIUS32___EMS3132___Soft_RE1_maj.xml",
            0x7A,
            InitKind::KwpFiveBaud { multipoint: true },
            (0xD0, 0x8F),
            0xFA,
            300,
        ),
        (
            "BMT_X65_ph1_p.xml",
            0x26,
            InitKind::KwpFiveBaud { multipoint: true },
            (0xD0, 0x8F),
            0xA6,
            300,
        ),
        (
            "Safir_2.xml",
            0x7A,
            InitKind::Iso8FiveBaud,
            (0xC2, 0x85),
            0x7A,
            500,
        ),
        (
            "Safir_1.xml",
            0x7A,
            InitKind::Iso8FiveBaud,
            (0x1C, 0x86),
            0x7A,
            500,
        ),
    ];
    for (file, addr, init, kw, five_baud, kw2_timeout) in cases {
        let Some(bytes) = common::ddt_file(file) else {
            continue;
        };
        let mut cfg = link_config(&EcuDef::from_xml_bytes(&bytes).unwrap()).unwrap();
        assert_eq!(cfg.ecu_address, addr, "{file}");
        assert_eq!(cfg.timing.init, init, "{file}");
        assert_eq!(cfg.expected_keywords, Some(kw), "{file}");
        assert_eq!(
            cfg.five_baud_address_byte(),
            addr,
            "{file}: single ECU sends plain"
        );
        assert_eq!(cfg.effective_timing().inter_byte_ms, 0, "{file}");
        assert_eq!(cfg.timing.baud, 10_400, "{file}");
        assert_eq!(cfg.timing.kw2_timeout_ms, kw2_timeout, "{file}");
        assert_eq!(cfg.timing.first_request_ms, 20, "{file}");
        assert_eq!(cfg.timing.reply_to_request_ms, 5, "{file}");
        assert_eq!(cfg.timing.request_to_reply_timeout_ms, 300, "{file}");
        assert_eq!(cfg.timing.tester_present_ms, 3000, "{file}");
        assert_eq!(cfg.timing.on_error_ms, 300, "{file}");
        cfg.variant = InitVariant::MultiEcu;
        assert_eq!(cfg.five_baud_address_byte(), five_baud, "{file}: multi ECU");
        let ib = if init.multipoint() { 7 } else { 0 };
        assert_eq!(cfg.effective_timing().inter_byte_ms, ib, "{file}");
    }
}
