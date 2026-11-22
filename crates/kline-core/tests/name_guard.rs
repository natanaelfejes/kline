//! Name-based immobilizer guard for ECU definitions the policy does not know by address
//! (coordinator scope addition 2026-11-22): Tier 1-3 requests whose name or data items look
//! immobilizer / transponder / key related are refused by the CLI before any approval.

use kline_core::safety::{self, Protocol, Target};

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
    use kline_spec::EcuDef;

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
            kline_core::spec::immobilizer_named_requests(&def(), &t(0x51), &[0x3B, 0x50, 0x12]);
        assert_eq!(hits, vec!["Write setting".to_string()]);
    }

    #[test]
    fn unrelated_requests_are_not_named() {
        let hits =
            kline_core::spec::immobilizer_named_requests(&def(), &t(0x51), &[0x30, 0xA0, 0x01]);
        assert!(hits.is_empty(), "{hits:?}");
    }

    #[test]
    fn owner_approved_bmt_remote_learning_is_exempt_only_on_the_bmt() {
        let p = [0x31, 0xC8, 0x00, 0x00, 0x00];
        assert!(kline_core::spec::immobilizer_named_requests(&def(), &t(0x26), &p).is_empty());
        assert_eq!(
            kline_core::spec::immobilizer_named_requests(&def(), &t(0x51), &p),
            vec!["APPRENT CLE 2 TEL".to_string()]
        );
    }
}

/// Step 7: the texts of an enumerated sent parameter are scanned too. Only the list value that
/// names an immobilizer output is flagged (fixed bytes + that value), so the other outputs of
/// the same actuator request stay usable.
#[cfg(feature = "spec")]
mod list_texts {
    use kline_core::safety::{Protocol, Target};
    use kline_spec::EcuDef;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Target Name="Body box" xmlns="http://www-diag.renault.com/2002/ECU">
<Function Name="Body" Address="81"/>
<Datas>
<Data Name="sortie"><Bits count="8"><List><Item Value="160" Text="Moteur condamnation"/><Item Value="166" Text="Ligne Transpondeur"/><Item Value="171" Text="Led Verlog"/></List></Bits></Data>
<Data Name="param"><Bits count="8"/></Data>
<Data Name="mode"><Bits count="4"><List><Item Value="1" Text="Transpondeur"/></List></Bits></Data>
</Datas>
<Requests Endian="Big">
<Request Name="COMMANDE DES ACTUATEURS"><Sent><SentBytes>31000000</SentBytes><DataItem Name="sortie" FirstByte="2"/><DataItem Name="param" FirstByte="3"/></Sent><Received MinBytes="1"/></Request>
<Request Name="Nibble"><Sent><SentBytes>3B4000</SentBytes><DataItem Name="mode" FirstByte="3"/></Sent><Received MinBytes="1"/></Request>
</Requests>
</Target>"#;

    fn names(p: &[u8]) -> Vec<String> {
        let def = EcuDef::from_xml_bytes(XML.as_bytes()).unwrap();
        let t = Target {
            protocol: Protocol::Kwp,
            ecu_address: 0x51,
        };
        kline_core::spec::immobilizer_named_requests(&def, &t, p)
    }

    #[test]
    fn immobilizer_list_values_are_flagged_with_their_byte() {
        assert_eq!(
            names(&[0x31, 0xA6, 0x00, 0x01]),
            vec!["COMMANDE DES ACTUATEURS"]
        );
        assert_eq!(
            names(&[0x31, 0xAB, 0x00, 0xFF]),
            vec!["COMMANDE DES ACTUATEURS"]
        );
        assert!(names(&[0x31, 0xA0, 0x00, 0x01]).is_empty());
        assert!(names(&[0x31, 0xC7, 0x00, 0x00, 0x00]).is_empty());
        // Not a whole byte at the first parameter position (4-bit item): no exact byte to
        // match, so the whole request is flagged.
        assert_eq!(names(&[0x3B, 0x40, 0x00]), vec!["Nibble"]);
    }
}
