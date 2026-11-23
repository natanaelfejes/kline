//! Bosch 5.3 ABS (ISO8 at 0x01): the replies captured on the car on 2026-11-23 decode with the
//! hand-written fixture and, when present, with DDT2000's X24_ABS_Bosch5.3.xml.

mod common;

use kline_spec::{EcuDef, SpecError};

/// `51` reply (MES_IDENTIF), car capture 20261123T122151Z-X24_ABS_Bosch5_3-exec-6649.jsonl.
const CAR_51: [u8; 24] = [
    0x51, 0x05, 0x02, 0x16, 0x77, 0x01, 0x04, 0x64, 0x07, 0x20, 0x02, 0x49, 0x12, 0x02, 0x65, 0x21,
    0x67, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
/// `58` reply (MES_DIAG), car capture 20261123T122204Z-X24_ABS_Bosch5_3-exec-7004.jsonl.
const CAR_58: [u8; 16] = [
    0x58, 0x08, 0x00, 0x10, 0xBF, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

fn fixture() -> EcuDef {
    let p = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/bosch_abs_iso8.xml"
    );
    EcuDef::from_xml_bytes(&std::fs::read(p).unwrap()).unwrap()
}

fn shown(def: &EcuDef, req: &str, reply: &[u8]) -> Vec<(String, String)> {
    def.request(req)
        .unwrap()
        .decode_checked(def, reply)
        .unwrap_or_else(|e| panic!("{req}: {e}"))
        .into_iter()
        .map(|i| (i.name, i.display.unwrap_or_default()))
        .collect()
}

fn get<'a>(items: &'a [(String, String)], name: &str) -> &'a str {
    &items
        .iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("{name}"))
        .1
}

#[test]
fn iso8_reply_with_the_request_sid_is_positive() {
    let def = fixture();
    let id = shown(&def, "Identification", &CAR_51);
    assert_eq!(get(&id, "ECU type"), "ABS");
    assert_eq!(get(&id, "Supplier"), "Bosch");
    assert_eq!(get(&id, "Part number"), "7701046407");
    assert_eq!(get(&id, "Software"), "024912");
    assert_eq!(get(&id, "Supplier part number"), "0265216703");
    let f = shown(&def, "Fault memory", &CAR_58);
    assert_eq!(get(&f, "Fault code 1"), "front left sensor signal");
    assert_eq!(get(&f, "Reference speed 1"), "16");
    assert_eq!(get(&f, "Voltage 1"), "13.56"); // 0xBF * 0.071
    assert_eq!(get(&f, "Counter 1"), "1");
    assert_eq!(get(&f, "Fault code 2"), "none");
    // SID + 0x40 stays positive too (the Safir rule), any other SID does not.
    let mut plus40 = CAR_58;
    plus40[0] = 0x98;
    assert!(def
        .request("Fault memory")
        .unwrap()
        .decode_checked(&def, &plus40)
        .is_ok());
    let mut other = CAR_58;
    other[0] = 0x51;
    assert!(matches!(
        def.request("Fault memory")
            .unwrap()
            .decode_checked(&def, &other),
        Err(SpecError::UnexpectedSid {
            expected: 0x98,
            got: 0x51
        })
    ));
}

#[test]
fn kwp_reply_with_the_request_sid_stays_unexpected() {
    let xml = br#"<Target Name="t" xmlns="http://www-diag.renault.com/2002/ECU">
<K><KWP><ISO8><KW1 Value="208"/><KW2 Value="143"/></ISO8></KWP></K>
<Datas><Data Name="X"><Bits><Scaled/></Bits></Data></Datas><Requests>
<Request Name="R"><Sent><SentBytes>51</SentBytes></Sent><Received MinBytes="2"><DataItem Name="X" FirstByte="2"/></Received></Request>
</Requests></Target>"#;
    let def = EcuDef::from_xml_bytes(xml).unwrap();
    assert!(matches!(
        def.request("R")
            .unwrap()
            .decode_checked(&def, &[0x51, 0x01]),
        Err(SpecError::UnexpectedSid {
            expected: 0x91,
            got: 0x51
        })
    ));
}

/// The SID echo is a Bosch 5.3 ABS quirk: an ISO8 definition with other key bytes (Safir
/// `C2 85`) or none keeps the strict `SID + 0x40` rule (review of the ABS branch, finding 3).
#[test]
fn iso8_reply_with_the_request_sid_needs_the_bosch_key_bytes() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/bosch_abs_iso8.xml"
    ))
    .unwrap();
    let kw = r#"<KW1 Value="220"/><KW2 Value="131" Timeout="1500"/>"#;
    assert!(src.contains(kw));
    for other in [
        r#"<KW1 Value="194"/><KW2 Value="133" Timeout="1500"/>"#,
        r#"<KW1 Value="220"/><KW2 Value="132" Timeout="1500"/>"#,
        "",
    ] {
        let def = EcuDef::from_xml_bytes(src.replace(kw, other).as_bytes()).unwrap();
        assert!(
            matches!(
                def.request("Fault memory")
                    .unwrap()
                    .decode_checked(&def, &CAR_58),
                Err(SpecError::UnexpectedSid {
                    expected: 0x98,
                    got: 0x58
                })
            ),
            "{other}"
        );
        let mut plus40 = CAR_58;
        plus40[0] = 0x98;
        assert!(def
            .request("Fault memory")
            .unwrap()
            .decode_checked(&def, &plus40)
            .is_ok());
    }
}

#[test]
fn iso8_clear_plan_uses_the_raz_request() {
    let p = fixture().clear_plan();
    assert_eq!(p.ours.payload, [0x45]);
    assert_eq!(
        p.ours.source,
        kline_spec::ClearSource::DefinitionRequest {
            name: "Fault memory RAZ".into()
        }
    );
    assert_eq!(p.ddt2000.payload, kline_spec::DDT2000_CLEAR_FALLBACK);
}

#[test]
fn real_x24_decodes_the_car_replies() {
    let Some(def) = common::ddt_ecu_as("X24_ABS_Bosch5.3.xml", kline_spec::Semantics::Ddt2000)
    else {
        return;
    };
    let k = def.k.as_ref().unwrap();
    assert_eq!(k.protocol(), kline_spec::KProtocol::Iso8);
    assert_eq!(k.key_words(), Some((0xDC, 0x83)));
    assert_eq!(def.function.as_ref().unwrap().address, 0x01);
    let id = shown(&def, "MES_IDENTIF", &CAR_51);
    assert_eq!(get(&id, "Identification calculateur"), "ABS");
    assert_eq!(get(&id, "Identification fournisseur"), "BOSCH");
    assert_eq!(get(&id, "Identification MPR"), "7701046407");
    assert_eq!(get(&id, "Identification du logiciel"), "024912");
    assert_eq!(get(&id, "Référence BOSCH"), "0265216703");
    let f = shown(&def, "MES_DIAG", &CAR_58);
    assert_eq!(get(&f, "Défaut mémoire 1"), "signal capteur AVG");
    assert_eq!(get(&f, "Vitesse de référence 1"), "16."); // VB Format "###.##"
    assert_eq!(get(&f, "Tension 1"), "13.56");
    assert_eq!(get(&f, "compteur mémoire 1"), "1");
    assert_eq!(get(&f, "Défaut mémoire 2"), "Aucun");
    assert_eq!(get(&f, "Défaut mémoire 3"), "Aucun");
    assert_eq!(def.clear_plan().ours.payload, [0x45]);
}
