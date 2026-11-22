//! Name-based immobilizer guard for ECU definitions the policy does not know by address
//! (coordinator scope addition 2026-11-22): Tier 1-3 requests whose name or data items look
//! immobilizer / transponder / key related are refused by the CLI before any approval.

use clio_core::safety::{self, Protocol, Target};

#[test]
fn immobilizer_keywords_are_recognised() {
    for name in [
        "Ecriture code antidemarrage",
        "ANTI-DÉMARRAGE",
        "Apprentissage transpondeur",
        "Transponder status",
        "IMMO reset",
        "Etat VERLOG",
        "Verrou logiciel",
        "APPTRP",
        "AP1CLE",
        "Code clé",
        "APPRENT CLE 2 TEL",
        "Key code learning",
        "Wegfahrsperre",
    ] {
        assert!(
            safety::immobilizer_keyword(name).is_some(),
            "{name:?} should be flagged"
        );
    }
}

#[test]
fn ordinary_names_are_not_flagged() {
    for name in [
        "Trame 10 - parametres 2",
        "Engine speed",
        "Reinit flags de pannes",
        "Output Control",
        "Clear faults",
        "Keyless",
        "Cleaning valve",
        "Coolant temperature",
    ] {
        assert_eq!(safety::immobilizer_keyword(name), None, "{name:?}");
    }
}

#[test]
fn approvals_flag_matches_the_feature() {
    assert_eq!(
        safety::APPROVALS_COMPILED,
        cfg!(feature = "approved-routines")
    );
}

#[cfg(feature = "spec")]
mod with_spec {
    use super::*;
    use clio_spec::EcuDef;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Target Name="Guard fixture" xmlns="http://www-diag.renault.com/2002/ECU">
<Function Name="Body" Address="38"/>
<Datas>
<Data Name="Code antidemarrage"><Bytes count="2"/></Data>
<Data Name="Lamp"><Bits><Scaled/></Bits></Data>
</Datas>
<Requests Endian="Big">
<Request Name="APPRENT CLE 2 TEL"><Sent><SentBytes>31C8000000</SentBytes></Sent><Received MinBytes="1"/></Request>
<Request Name="Write setting"><Sent><SentBytes>3B5000</SentBytes><DataItem Name="Code antidemarrage" FirstByte="3"/></Sent><Received MinBytes="1"/></Request>
<Request Name="Lamp test"><Sent><SentBytes>30A001</SentBytes><DataItem Name="Lamp" FirstByte="3"/></Sent><Received MinBytes="1"/></Request>
</Requests>
</Target>"#;

    fn def() -> EcuDef {
        EcuDef::from_xml_bytes(XML.as_bytes()).unwrap()
    }

    fn t(addr: u8) -> Target {
        Target {
            protocol: Protocol::Kwp,
            ecu_address: addr,
        }
    }

    #[test]
    fn requests_with_immobilizer_data_items_are_named() {
        let hits =
            clio_core::spec::immobilizer_named_requests(&def(), &t(0x51), &[0x3B, 0x50, 0x12]);
        assert_eq!(hits, vec!["Write setting".to_string()]);
    }

    #[test]
    fn unrelated_requests_are_not_named() {
        let hits =
            clio_core::spec::immobilizer_named_requests(&def(), &t(0x51), &[0x30, 0xA0, 0x01]);
        assert!(hits.is_empty(), "{hits:?}");
    }

    #[test]
    fn owner_approved_bmt_remote_learning_is_exempt_only_on_the_bmt() {
        let p = [0x31, 0xC8, 0x00, 0x00, 0x00];
        assert!(clio_core::spec::immobilizer_named_requests(&def(), &t(0x26), &p).is_empty());
        assert_eq!(
            clio_core::spec::immobilizer_named_requests(&def(), &t(0x51), &p),
            vec!["APPRENT CLE 2 TEL".to_string()]
        );
    }
}
