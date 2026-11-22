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

/// Security review L3: more key words ("clef", "code manuel", German "Schluessel"), and a
/// request whose first sent item starts at byte 1 can be flagged. A flag always needs at least
/// two distinguishing bytes: a service byte alone never flags (a template like BMT
/// "LANCEMENT DES ROUTINES" `3100000000` with "MSB/LSB Code manuel" items must not refuse the
/// owner's `31C7000000`/`31C8000000` or the actuator tests `31 A0..B3 ..`).
#[test]
fn more_key_words_are_recognised() {
    for name in [
        "App seconde cléf TIR",
        "Clef 2",
        "CLEFS",
        "Emission du CODE MANUEL",
        "MSB Code manuel",
        "Schlüssel anlernen",
        "Schluessel",
        "SCHLUSSEL",
    ] {
        assert!(
            safety::immobilizer_keyword(name).is_some(),
            "{name:?} should be flagged"
        );
    }
    for name in ["Clefable", "Manuel", "Code defaut", "Schluss"] {
        assert_eq!(safety::immobilizer_keyword(name), None, "{name:?}");
    }
}

#[cfg(feature = "spec")]
mod prefixes {
    use kline_core::safety::{self, DefinitionGuard, EcuClass, Protocol, Target, Tier};
    use kline_spec::EcuDef;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Target Name="Prefix fixture" xmlns="http://www-diag.renault.com/2002/ECU">
<Function Name="Body" Address="81"/>
<Datas>
<Data Name="v"><Bits count="8"/></Data>
<Data Name="w"><Bits count="16"/></Data>
<Data Name="MSB Code manuel"><Bits count="8"/></Data>
<Data Name="Neutral item"><Description>Valeur du transpondeur</Description><Bits count="8"/></Data>
<Data Name="Plain"><Comment>sets the clef count</Comment><Bits count="8"/></Data>
<Data Name="Mnem"><Mnemonic>VERLOG_X</Mnemonic><Bits count="8"/></Data>
<Data Name="Etat verrou"><Bits count="8"/></Data>
</Datas>
<Requests Endian="Big">
<Request Name="Ecriture clef"><Sent><SentBytes>3B6000</SentBytes><DataItem Name="v" FirstByte="3"/></Sent><Received MinBytes="1"/></Request>
<Request Name="Emission du CODE MANUEL"><Sent><SentBytes>3B7100</SentBytes><DataItem Name="v" FirstByte="3"/></Sent><Received MinBytes="1"/></Request>
<Request Name="Schlüssel anlernen"><Sent><SentBytes>3B7200</SentBytes><DataItem Name="v" FirstByte="3"/></Sent><Received MinBytes="1"/></Request>
<Request Name="Ecriture code cle"><Sent><SentBytes>3B8000</SentBytes><DataItem Name="w" FirstByte="1"/></Sent><Received MinBytes="1"/></Request>
<Request Name="LANCEMENT DES ROUTINES"><Sent><SentBytes>3100000000</SentBytes><DataItem Name="v" FirstByte="2"/><DataItem Name="MSB Code manuel" FirstByte="4"/></Sent><Received MinBytes="1"/></Request>
<Request Name="Short code cle"><Sent><SentBytes>3B</SentBytes><DataItem Name="v" FirstByte="2"/></Sent><Received MinBytes="1"/></Request>
<Request Name="Write A"><Sent><SentBytes>3B9100</SentBytes><DataItem Name="Neutral item" FirstByte="3"/></Sent><Received MinBytes="1"/></Request>
<Request Name="Write B"><Sent><SentBytes>3B9200</SentBytes><DataItem Name="Plain" FirstByte="3"/></Sent><Received MinBytes="1"/></Request>
<Request Name="Write C"><Sent><SentBytes>3B9300</SentBytes><DataItem Name="Mnem" FirstByte="3"/></Sent><Received MinBytes="1"/></Request>
<Request Name="Lecture etat"><Sent><SentBytes>21B5</SentBytes></Sent><Received MinBytes="3"><DataItem Name="Etat verrou" FirstByte="3"/></Received></Request>
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

    fn names(addr: u8, p: &[u8]) -> Vec<String> {
        kline_core::spec::immobilizer_named_requests(&def(), &t(addr), p)
    }

    #[test]
    fn new_words_flag_requests_with_two_distinguishing_bytes() {
        assert_eq!(names(0x51, &[0x3B, 0x60, 0x01]), vec!["Ecriture clef"]);
        assert_eq!(
            names(0x51, &[0x3B, 0x71, 0x01]),
            vec!["Emission du CODE MANUEL"]
        );
        assert_eq!(names(0x51, &[0x3B, 0x72, 0x01]), vec!["Schlüssel anlernen"]);
        // Other identifiers of the same service are not touched.
        assert!(names(0x51, &[0x3B, 0x61, 0x01]).is_empty());
    }

    #[test]
    fn first_item_at_byte_one_flags_the_whole_template() {
        assert_eq!(names(0x51, &[0x3B, 0x80, 0x00]), vec!["Ecriture code cle"]);
        assert!(names(0x51, &[0x3B, 0x81, 0x00]).is_empty());
    }

    #[test]
    fn a_service_byte_alone_never_flags() {
        // "LANCEMENT DES ROUTINES" (fixed prefix `31`) is flagged by its full template only.
        assert_eq!(
            names(0x51, &[0x31, 0x00, 0x00, 0x00, 0x00]),
            vec!["LANCEMENT DES ROUTINES"]
        );
        let g = kline_core::spec::definition_guard(&def());
        let mut immo = g.clone();
        immo.class = EcuClass::Immobilizer;
        for addr in [0x26u8, 0x51] {
            let guard = if addr == 0x26 { &g } else { &immo };
            for p in [
                &[0x31, 0xC7, 0x00, 0x00, 0x00][..],
                &[0x31, 0xC8, 0x00, 0x00, 0x00],
            ] {
                assert_eq!(
                    safety::tier_of_in(guard, &t(addr), p).unwrap(),
                    Tier::Service,
                    "{addr:02X} {p:02X?}"
                );
            }
            assert_eq!(
                safety::tier_of_in(guard, &t(addr), &[0x31, 0xA0, 0x00, 0x01]).unwrap(),
                Tier::Service
            );
        }
        // A one-byte template ("Short code cle", SentBytes `3B`) cannot be flagged at all.
        assert!(names(0x51, &[0x3B, 0x01]).is_empty());
        assert!(names(0x51, &[0x3B]).is_empty());
        let mut direct = DefinitionGuard::default();
        direct.flag(vec![0x3B], "one byte".into());
        assert!(direct.immobilizer_names(&t(0x51), &[0x3B, 0x00]).is_empty());
    }

    /// Security review L8 (mutants M43, M44): the description, comment and mnemonic of a sent
    /// data item, and the names of received items, flag the request.
    #[test]
    fn item_texts_and_received_items_flag_the_request() {
        assert_eq!(names(0x51, &[0x3B, 0x91, 0x00]), vec!["Write A"]);
        assert_eq!(names(0x51, &[0x3B, 0x92, 0x00]), vec!["Write B"]);
        assert_eq!(names(0x51, &[0x3B, 0x93, 0x00]), vec!["Write C"]);
        // Received item "Etat verrou": the read stays Tier 0, anything longer is refused.
        let g = kline_core::spec::definition_guard(&def());
        assert_eq!(
            safety::tier_of_in(&g, &t(0x51), &[0x21, 0xB5]).unwrap(),
            Tier::Read
        );
        assert_eq!(names(0x51, &[0x21, 0xB5, 0x01]), vec!["Lecture etat"]);
        assert!(safety::tier_of_in(&g, &t(0x51), &[0x21, 0xB5, 0x01])
            .unwrap_err()
            .reason
            .is_forbidden());
    }
}
