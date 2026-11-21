//! Core logic on hand-written fixtures (no Renault files needed).
//!
//! The expected display strings of `read_block_decodes` were also produced by ddt4all on the
//! same frame (crosscheck_ddt4all.py, FIXTURE section): all 13 items matched.

mod common;

use clio_spec::{Addressing, DataItem, EcuDef, Endian, HexBytes, KProtocol, Raw, SpecError, Value};
use common::{display_of, fixture, number_of, raw_of};

fn ecu() -> EcuDef {
    EcuDef::from_xml_bytes(&fixture("minimal_ecu.xml")).expect("fixture parses")
}

/// The frame used by `read_block_decodes` (also used by the Python crosscheck).
pub const READ_BLOCK_FRAME: [u8; 18] = [
    0x61, 0x01, // response SID + local id
    0xB6, // byte 3 = 1011_0110
    0x34, 0x12, // bytes 4-5, little endian 0x1234
    0x12, 0x34, // bytes 6-7, big endian 0x1234
    0xF6, // byte 8, signed -10
    0xFE, 0xFF, // bytes 9-10, little endian signed 0xFFFE = -2
    0x02, // byte 11, list value 2 (two labels: last wins)
    0xAB, 0xCD, // bytes 12-13, raw hex
    b'E', b'7', b'J', 0xFF, // bytes 14-17 ASCII with an invalid UTF-8 byte
    0x01, // byte 18
];

#[test]
fn model_fields() {
    let e = ecu();
    assert_eq!(e.name, "Fixture ECU");
    assert_eq!(
        e.description.as_deref(),
        Some("Hand-written fixture\nsecond line")
    );
    assert_eq!(e.projects, ["x99", "y01"]);
    let f = e.function.as_ref().unwrap();
    assert_eq!((f.name.as_str(), f.address), ("Test function", 122));
    let ps = e.power_supply.as_ref().unwrap();
    assert!(ps.ground && ps.vbat && !ps.after_contact);
    let k = e.k.as_ref().unwrap();
    assert_eq!(k.protocol(), KProtocol::KwpFastInit);
    assert_eq!(k.key_words(), Some((241, 122)));
    assert_eq!(k.kwp.as_ref().unwrap().multipoint, Some(false));
    assert_eq!(
        (
            k.inter_byte_delay,
            k.reply_to_request_delay,
            k.request_to_reply_timeout
        ),
        (Some(1), Some(5), Some(300))
    );
    assert_eq!(
        (k.tester_present_delay, k.on_error_delay),
        (Some(2000), Some(100))
    );
    let can = e.can.as_ref().unwrap();
    assert_eq!(
        (can.baud_rate, can.send_id, can.receive_id),
        (Some(500000), Some(2016), Some(2024))
    );
    assert_eq!(e.auto_idents.len(), 2);
    assert_eq!(e.auto_idents[1].version, "8000");
    assert_eq!(e.datas.len(), 14);
    assert_eq!(e.requests.len(), 4);

    let nib = e.data("nibble").unwrap();
    assert_eq!(nib.comment.as_deref(), Some("4 bits at BitOffset 2"));
    assert_eq!(nib.mnemonic.as_deref(), Some("NIB"));
    assert_eq!(
        e.data("flag msb").unwrap().description.as_deref(),
        Some("1 bit, list")
    );
    // Bytes count="0,5": decimal comma, 4 bits in 1 byte (ddt4all rule).
    let half = e.data("half").unwrap();
    assert_eq!(
        (half.bits_count, half.bytes_count, half.is_bytes),
        (4, 1, true)
    );
    let ident = e.data("ident").unwrap();
    assert_eq!(
        (ident.bits_count, ident.bytes_count, ident.ascii),
        (32, 4, true)
    );
    let ts = e.data("temp signed").unwrap();
    assert!(ts.signed);
    assert_eq!(ts.bits_count, 8); // default
    let sc = ts.scaled.as_ref().unwrap();
    assert_eq!((sc.step, sc.offset, sc.divide_by), (0.5, 0.0, 1.0));
    assert_eq!(sc.format.as_deref(), Some("#0.0"));
    assert_eq!(ts.unit(), Some("°C"));

    let rb = e.request("Read Block").unwrap();
    assert!(rb.manual_send && !rb.variable_length && !rb.dossier_maintenabilite);
    assert_eq!(rb.deny_access, ["Plant", "AfterSales"]);
    assert_eq!(rb.min_bytes, 18);
    let le = rb
        .received_items
        .iter()
        .find(|i| i.name == "word le")
        .unwrap();
    assert_eq!(le.endian, Some(Endian::Little));
    let sm = e.request("Set Mode").unwrap();
    assert!(sm.variable_length && sm.dossier_maintenabilite);
    assert_eq!(sm.reply_bytes, Some(HexBytes(vec![0x7B, 0x02])));
    assert!(!sm.sent_items[0].reference);
    assert!(rb.received_items[0].reference);
    let same: Vec<_> = e
        .request_by_bytes("2101")
        .unwrap()
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(same, ["Read Block", "Same Bytes"]);
    assert!(e.request("no such request").is_none());
}

#[test]
fn read_block_decodes() {
    let e = ecu();
    let items = e
        .request("Read Block")
        .unwrap()
        .decode(&e, &READ_BLOCK_FRAME)
        .unwrap();
    let d = |n| display_of(&items, n);
    // Big endian: BitOffset counts from the MSB.
    assert_eq!(d("flag msb").as_deref(), Some("ON"));
    assert_eq!(d("nibble").as_deref(), Some("13")); // bits 2..5 of 1011_0110 = 1101
    assert_eq!(d("half").as_deref(), Some("06")); // bits 4..7 = 0110, unscaled -> hex
                                                  // Little endian: BitOffset counts from the LSB (bit 1 of 1011_0110 = 1).
    assert_eq!(d("le bit").as_deref(), Some("1"));
    // 16 bit: same value both ways round. 0x1234 * 0.5 - 10 = 2320
    assert_eq!(raw_of(&items, "word le"), Some(Raw::Int(0x1234)));
    assert_eq!(raw_of(&items, "word be"), Some(Raw::Int(0x1234)));
    assert_eq!(d("word le").as_deref(), Some("2320"));
    assert_eq!(d("word be").as_deref(), Some("2320"));
    // Signed 8 bit: 0xF6 = -10 -> -5.0, Format #0.0
    assert_eq!(number_of(&items, "temp signed"), -5.0);
    assert_eq!(d("temp signed").as_deref(), Some("-5.0"));
    // Signed little endian 16 bit: FE FF -> 0xFFFE -> -2
    assert_eq!(number_of(&items, "word signed le"), -2.0);
    assert_eq!(d("word signed le").as_deref(), Some("-2"));
    // List with a repeated Value: last label wins (ddt4all dict).
    assert_eq!(d("mode").as_deref(), Some("deux"));
    // Unscaled Bytes: lowercase hex, like ddt4all.
    assert_eq!(d("raw hex").as_deref(), Some("abcd"));
    assert_eq!(
        items.iter().find(|i| i.name == "raw hex").unwrap().value,
        Some(Value::Hex("abcd".into()))
    );
    // ASCII: invalid UTF-8 bytes dropped (Python errors='ignore'), raw kept as bytes.
    assert_eq!(d("ident").as_deref(), Some("E7J"));
    assert_eq!(
        raw_of(&items, "ident"),
        Some(Raw::Bytes(HexBytes(b"E7J\xFF".to_vec())))
    );
    // 1 / 100000 printed like Python str(float).
    assert_eq!(d("ratio").as_deref(), Some("1e-05"));
    // DivideBy 0: no value (ddt4all returns None), raw still reported.
    assert_eq!(d("broken"), None);
    assert_eq!(raw_of(&items, "broken"), Some(Raw::Int(1)));
    // Units come from <Scaled Unit>.
    let unit = items
        .iter()
        .find(|i| i.name == "word le")
        .unwrap()
        .unit
        .clone();
    assert_eq!(unit.as_deref(), Some("rpm"));
}

#[test]
fn short_frame_gives_none_per_item() {
    let e = ecu();
    let items = e
        .request("Read Block")
        .unwrap()
        .decode(&e, &READ_BLOCK_FRAME[..5])
        .unwrap();
    assert_eq!(display_of(&items, "word le").as_deref(), Some("2320")); // bytes 4-5 present
    assert_eq!(display_of(&items, "word be"), None);
    assert_eq!(raw_of(&items, "ident"), None);
    // FirstByte missing (0) is never decodable.
    let d = e.data("nibble").unwrap();
    let item = DataItem {
        name: "nibble".into(),
        first_byte: 0,
        bit_offset: 0,
        reference: false,
        endian: None,
    };
    assert_eq!(d.extract(&READ_BLOCK_FRAME, &item, Endian::Big), None);
}

#[test]
fn display_formats_match_python() {
    let e = ecu();
    let ratio = e.data("ratio").unwrap();
    let ts = e.data("temp signed").unwrap();
    let show = |d: &clio_spec::DataDef, raw: u8| d.interpret(&[raw]).unwrap().1;
    assert_eq!(show(ratio, 0), "0");
    assert_eq!(show(ratio, 10), "0.0001"); // Python switches to exponent below 1e-4
    assert_eq!(show(ratio, 25), "0.00025");
    assert_eq!(show(ratio, 5), "5e-05");
    assert_eq!(show(ratio, 123), "0.00123");
    assert_eq!(show(ts, 0x01), "0.5");
    assert_eq!(show(ts, 0x7F), "63.5");
    assert_eq!(show(ts, 0x80), "-64.0");
}

#[test]
fn encode_and_build_requests() {
    let e = ecu();
    let ws = e.request("Write Setpoint").unwrap();
    // 1500 rpm / Step 2 = raw 750 = 0x02EE at bytes 3-4.
    let frame = ws
        .build(&e, &[("setpoint", Value::Number(1500.0))])
        .unwrap();
    assert_eq!(frame, [0x3B, 0x01, 0x02, 0xEE]);
    // Off-grid input rounds to the nearest raw step (1503 / 2 = 751.5 -> 752 = 0x02F0;
    // ddt4all would truncate to 751).
    let frame = ws
        .build(&e, &[("setpoint", Value::Number(1503.0))])
        .unwrap();
    assert_eq!(frame, [0x3B, 0x01, 0x02, 0xF0]);
    // Untouched parameters keep SentBytes.
    assert_eq!(ws.build(&e, &[]).unwrap(), [0x3B, 0x01, 0x00, 0x00]);

    let sm = e.request("Set Mode").unwrap();
    let frame = sm
        .build(
            &e,
            &[
                ("mode", Value::Text("one".into())),
                ("flag msb", Value::Text("ON".into())),
            ],
        )
        .unwrap();
    assert_eq!(frame, [0x3B, 0x82, 0x01]); // flag sets only the MSB of byte 2
    let frame = sm.build(&e, &[("mode", Value::Hex("2".into()))]).unwrap();
    assert_eq!(frame, [0x3B, 0x02, 0x02]);

    // Errors are typed, never silent.
    assert!(matches!(
        ws.build(&e, &[("setpoint", Value::Number(1e9))]),
        Err(SpecError::Encode { .. })
    ));
    assert!(matches!(
        ws.build(&e, &[("setpoint", Value::Number(-2.0))]),
        Err(SpecError::Encode { .. })
    ));
    assert!(matches!(
        sm.build(&e, &[("mode", Value::Text("three".into()))]),
        Err(SpecError::Encode { .. })
    ));
    assert!(matches!(
        sm.build(&e, &[("mode", Value::Hex("1FF".into()))]),
        Err(SpecError::Encode { .. })
    ));
    assert!(matches!(
        sm.build(&e, &[("mode", Value::Hex("xyz".into()))]),
        Err(SpecError::InvalidHex(_))
    ));
    assert!(matches!(
        ws.build(&e, &[("mode", Value::Number(1.0))]),
        Err(SpecError::UnknownSentItem { .. })
    ));
}

#[test]
fn insert_is_inverse_of_extract() {
    let e = ecu();
    let rb = e.request("Read Block").unwrap();
    let mut frame = [0u8; 18];
    for item in &rb.received_items {
        let d = e.data(&item.name).unwrap();
        let raw = d.raw_from_bytes(&d.extract(&READ_BLOCK_FRAME, item, e.endian).unwrap());
        d.insert(&raw, &mut frame, item, e.endian).unwrap();
    }
    // Byte 3 carries four overlapping items; every other byte belongs to one item.
    assert_eq!(&frame[2..], &READ_BLOCK_FRAME[2..]);

    // Signed values: encode(-5.0) for "temp signed" gives raw 0xF6.
    let ts = e.data("temp signed").unwrap();
    assert_eq!(ts.encode(&Value::Number(-5.0)).unwrap(), Raw::Int(0xF6));
    assert!(ts.encode(&Value::Number(64.0)).is_err()); // raw 128 would decode as -128
                                                       // ASCII: padded with spaces / truncated, like ddt4all.
    let ident = e.data("ident").unwrap();
    assert_eq!(
        ident.encode(&Value::Text("AB".into())).unwrap(),
        Raw::Bytes(HexBytes(b"AB  ".to_vec()))
    );
    assert_eq!(
        ident.encode(&Value::Text("ABCDEF".into())).unwrap(),
        Raw::Bytes(HexBytes(b"ABCD".to_vec()))
    );
    // An item that runs past the frame end is an error.
    let item = rb
        .received_items
        .iter()
        .find(|i| i.name == "ident")
        .unwrap();
    let mut short = [0u8; 10];
    assert!(matches!(
        ident.insert(
            &Raw::Bytes(HexBytes(b"ABCD".to_vec())),
            &mut short,
            item,
            Endian::Big
        ),
        Err(SpecError::OutOfFrame { .. })
    ));
}

#[test]
fn json_export() {
    let e = ecu();
    let v: serde_json::Value = serde_json::to_value(&e).unwrap();
    assert_eq!(v["requests"][0]["sent_bytes"], "2101");
    assert_eq!(v["requests"][0]["received_items"][0]["ref"], true);
    assert_eq!(v["k"]["kwp"]["fast_init"]["kw1"], 241);
    assert_eq!(v["endian"], "Big");
    let items = e
        .request("Read Block")
        .unwrap()
        .decode(&e, &READ_BLOCK_FRAME)
        .unwrap();
    let j = serde_json::to_value(&items).unwrap();
    assert_eq!(j[4]["raw"], 0x1234);
    assert_eq!(j[4]["value"]["kind"], "number");
    assert_eq!(j[4]["value"]["value"], 2320.0);
    assert_eq!(j[10]["raw"], "45374AFF");
    assert_eq!(j[10]["value"]["kind"], "text");
}

#[test]
fn windows_1252_fixture() {
    let e = EcuDef::from_xml_bytes(&fixture("cp1252_ecu.xml")).unwrap();
    assert_eq!(e.name, "Démo 1252");
    let k = e.k.as_ref().unwrap();
    assert_eq!(k.protocol(), KProtocol::Iso8);
    assert_eq!(k.key_words(), Some((194, 133)));
    let r = e.request("Lecture état").unwrap();
    let items = r.decode(&e, &[0x73, 0x51, 200]).unwrap();
    assert_eq!(items[0].display.as_deref(), Some("85.00"));
    assert_eq!(items[0].unit.as_deref(), Some("°C"));
    // Exact binary ties round half to even, like Python '%.2f':
    // 1 * 0.625 - 40 = -39.375 -> "-39.38", 3 * 0.625 - 40 = -38.125 -> "-38.12".
    let t = e.data("température").unwrap();
    assert_eq!(t.interpret(&[1]).unwrap().1, "-39.38");
    assert_eq!(t.interpret(&[3]).unwrap().1, "-38.12");
}

#[test]
fn addressing_fixture() {
    let a = Addressing::from_xml_bytes(&fixture("addressing.xml")).unwrap();
    assert_eq!(a.tool_address, Some(241));
    assert_eq!(a.protocols.len(), 2);
    assert!(a.protocols[0].search && !a.protocols[1].search);
    assert_eq!(a.protocols[1].multipoint, Some(true));
    assert_eq!(a.protocols[1].init_mode, Some(2));
    let ecm = a.function(122).unwrap();
    assert_eq!(ecm.names[1].text, "Injection électronique");
    assert_eq!(ecm.names[1].lang.as_deref(), Some("fr"));
    assert_eq!(
        (ecm.xid, ecm.rid, ecm.iso8_address),
        (Some(2016), Some(2024), None)
    );
    assert_eq!(ecm.protocols[0].name, "ISO8");
    assert_eq!(a.function(38).unwrap().iso8_address, Some(174));
    assert!(a.function(1).is_none());
}

#[test]
fn rejects_malformed_input() {
    let wrong_ns = br#"<xml><Target Name="x" xmlns="urn:other"/></xml>"#;
    assert!(matches!(
        EcuDef::from_xml_bytes(wrong_ns),
        Err(SpecError::WrongDocument(_))
    ));
    let bad_endian = br#"<Target xmlns="http://www-diag.renault.com/2002/ECU"><Requests Endian="Middle"/></Target>"#;
    assert!(matches!(
        EcuDef::from_xml_bytes(bad_endian),
        Err(SpecError::InvalidValue { .. })
    ));
    let bad_hex = br#"<Target xmlns="http://www-diag.renault.com/2002/ECU"><Requests><Request Name="r"><Sent><SentBytes>215</SentBytes></Sent></Request></Requests></Target>"#;
    assert!(matches!(
        EcuDef::from_xml_bytes(bad_hex),
        Err(SpecError::InvalidHex(_))
    ));
    let bad_kw = br#"<Target xmlns="http://www-diag.renault.com/2002/ECU"><K><ISO8><KW1 Value="300"/></ISO8></K></Target>"#;
    assert!(matches!(
        EcuDef::from_xml_bytes(bad_kw),
        Err(SpecError::InvalidValue { .. })
    ));
    // Declared UTF-8 but contains a lone 0xE9 (windows-1252 e acute).
    let mislabeled = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><Target xmlns=\"http://www-diag.renault.com/2002/ECU\" Name=\"D\xE9mo\"/>";
    assert!(matches!(
        EcuDef::from_xml_bytes(mislabeled),
        Err(SpecError::Encoding(_))
    ));
    let unknown = b"<?xml version=\"1.0\" encoding=\"klingon\"?><Target/>";
    assert!(matches!(
        EcuDef::from_xml_bytes(unknown),
        Err(SpecError::UnknownEncoding(_))
    ));
    // Minimal valid document: everything optional is empty.
    let minimal = br#"<Target xmlns="http://www-diag.renault.com/2002/ECU" Name="m"/>"#;
    let e = EcuDef::from_xml_bytes(minimal).unwrap();
    assert!(e.k.is_none() && e.requests.is_empty() && e.datas.is_empty());
    assert_eq!(e.endian, Endian::Big);
}
