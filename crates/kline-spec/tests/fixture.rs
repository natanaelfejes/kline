//! Core logic on hand-written fixtures (no Renault files needed).
//!
//! The expected display strings of `read_block_decodes` were also produced by ddt4all on the
//! same frame (crosscheck_ddt4all.py, FIXTURE section): all 13 items matched.

mod common;

use common::{display_of, fixture, number_of, raw_of};
use kline_spec::{
    Addressing, DataItem, EcuDef, Endian, HexBytes, KProtocol, ParseWarning, Raw, SpecError, Value,
};

/// These tests pin the ddt4all interpretation (docs/spec-semantics.md); DDT2000 semantics are
/// tested in tests/ddt2000.rs.
fn ddt4all(b: &[u8]) -> Result<EcuDef, kline_spec::SpecError> {
    EcuDef::from_xml_bytes_as(b, kline_spec::Semantics::Ddt4all)
}

fn ddt4all_report(b: &[u8]) -> Result<(EcuDef, kline_spec::ParseReport), kline_spec::SpecError> {
    EcuDef::from_xml_bytes_with_report_as(b, kline_spec::Semantics::Ddt4all)
}

fn ecu() -> EcuDef {
    ddt4all(&fixture("minimal_ecu.xml")).expect("fixture parses")
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
        .decode_checked(&e, &READ_BLOCK_FRAME)
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
        .decode_unchecked(&e, &READ_BLOCK_FRAME[..5])
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
    let show = |d: &kline_spec::DataDef, raw: u8| d.interpret(&[raw]).unwrap().1;
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
        .decode_checked(&e, &READ_BLOCK_FRAME)
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
    let e = ddt4all(&fixture("cp1252_ecu.xml")).unwrap();
    assert_eq!(e.name, "Démo 1252");
    let k = e.k.as_ref().unwrap();
    assert_eq!(k.protocol(), KProtocol::Iso8);
    assert_eq!(k.key_words(), Some((194, 133)));
    let r = e.request("Lecture état").unwrap();
    let items = r.decode_checked(&e, &[0x73, 0x51, 200]).unwrap();
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
        ddt4all(wrong_ns),
        Err(SpecError::WrongDocument(_))
    ));
    let bad_endian = br#"<Target xmlns="http://www-diag.renault.com/2002/ECU"><Requests Endian="Middle"/></Target>"#;
    assert!(matches!(
        ddt4all(bad_endian),
        Err(SpecError::InvalidValue { .. })
    ));
    let bad_hex = br#"<Target xmlns="http://www-diag.renault.com/2002/ECU"><Requests><Request Name="r"><Sent><SentBytes>215</SentBytes></Sent></Request></Requests></Target>"#;
    assert!(matches!(ddt4all(bad_hex), Err(SpecError::InvalidHex(_))));
    let bad_kw = br#"<Target xmlns="http://www-diag.renault.com/2002/ECU"><K><ISO8><KW1 Value="300"/></ISO8></K></Target>"#;
    assert!(matches!(
        ddt4all(bad_kw),
        Err(SpecError::InvalidValue { .. })
    ));
    // Declared UTF-8 but contains a lone 0xE9 (windows-1252 e acute).
    let mislabeled = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><Target xmlns=\"http://www-diag.renault.com/2002/ECU\" Name=\"D\xE9mo\"/>";
    assert!(matches!(ddt4all(mislabeled), Err(SpecError::Encoding(_))));
    let unknown = b"<?xml version=\"1.0\" encoding=\"klingon\"?><Target/>";
    assert!(matches!(
        ddt4all(unknown),
        Err(SpecError::UnknownEncoding(_))
    ));
    // Minimal valid document: everything optional is empty.
    let minimal = br#"<Target xmlns="http://www-diag.renault.com/2002/ECU" Name="m"/>"#;
    let e = ddt4all(minimal).unwrap();
    assert!(e.k.is_none() && e.requests.is_empty() && e.datas.is_empty());
    assert_eq!(e.endian, Endian::Big);
}

fn target(body: &str) -> Vec<u8> {
    format!(r#"<Target xmlns="http://www-diag.renault.com/2002/ECU" Name="t">{body}</Target>"#)
        .into_bytes()
}

#[test]
fn checked_decode_rejects_bad_responses() {
    let e = ecu();
    let rb = e.request("Read Block").unwrap(); // 21 01, MinBytes 18, first item at byte 3
    let check = |resp: &[u8]| rb.decode_checked(&e, resp);
    assert_eq!(check(&READ_BLOCK_FRAME).unwrap().len(), 13);
    assert!(matches!(
        check(&[0x7F, 0x21, 0x12]),
        Err(SpecError::NegativeResponse {
            sid: 0x21,
            nrc: 0x12
        })
    ));
    assert!(matches!(
        check(&[0x7F, 0x21]),
        Err(SpecError::TruncatedNegativeResponse { len: 2 })
    ));
    assert!(matches!(
        check(&[]),
        Err(SpecError::TooShort { min: 18, len: 0 })
    ));
    let mut wrong_sid = READ_BLOCK_FRAME;
    wrong_sid[0] = 0x62;
    assert!(matches!(
        check(&wrong_sid),
        Err(SpecError::UnexpectedSid {
            expected: 0x61,
            got: 0x62
        })
    ));
    assert!(matches!(
        check(&READ_BLOCK_FRAME[..17]),
        Err(SpecError::TooShort { min: 18, len: 17 })
    ));
    let mut wrong_echo = READ_BLOCK_FRAME;
    wrong_echo[1] = 0x05;
    assert!(matches!(
        check(&wrong_echo),
        Err(SpecError::ReplyMismatch {
            position: 2,
            expected: 0x01,
            got: 0x05
        })
    ));
    // Against the frame actually sent, the echo of 21 05 is right.
    assert_eq!(
        rb.decode_checked_against(&e, &[0x21, 0x05], &wrong_echo)
            .unwrap()
            .len(),
        13
    );
    // Requests without received items: SID and MinBytes only.
    let sm = e.request("Set Mode").unwrap();
    assert!(sm.decode_checked(&e, &[0x7B, 0x02]).unwrap().is_empty());
    assert!(matches!(
        sm.decode_checked(&e, &[0x7B]),
        Err(SpecError::TooShort { min: 2, len: 1 })
    ));
    // A SID without a positive response.
    let ff = ddt4all(&target(
        "<Requests><Request Name=\"r\"><Sent><SentBytes>FF00</SentBytes></Sent></Request></Requests>",
    ))
    .unwrap();
    assert!(matches!(
        ff.requests[0].decode_checked(&ff, &[0x3F]),
        Err(SpecError::NoPositiveSid { .. })
    ));
}

#[test]
fn missing_first_byte_is_an_error() {
    let xml = target(
        r#"<Datas><Data Name="d"><Bits count="8"/></Data></Datas><Requests><Request Name="r"><Sent><SentBytes>2101</SentBytes></Sent><Received MinBytes="3"><DataItem Name="d" BitOffset="0"/></Received></Request></Requests>"#,
    );
    assert!(matches!(
        ddt4all(&xml),
        Err(SpecError::MissingFirstByte { item }) if item == "d"
    ));
}

#[test]
fn misspelled_attribute_warns() {
    let xml = target(
        r#"<Datas><Data Name="speed"><Bits count="8"><Scaled Stepp="2" Unit="km/h"/></Bits></Data></Datas>"#,
    );
    let (e, report) = ddt4all_report(&xml).unwrap();
    // The misspelled Step falls back to the default...
    assert_eq!(e.data("speed").unwrap().scaled.as_ref().unwrap().step, 1.0);
    // ...and the report says so.
    assert_eq!(
        report.warnings,
        [ParseWarning::UnknownAttribute {
            path: r#"Target[@Name="t"]/Datas/Data[@Name="speed"]/Bits/Scaled"#.into(),
            name: "Stepp".into()
        }]
    );
    // An unknown element is reported once, not its subtree.
    let xml = target(r#"<Datas><Data Name="x"><Bitz count="8"><Scaled/></Bitz></Data></Datas>"#);
    let (_, report) = ddt4all_report(&xml).unwrap();
    assert_eq!(
        report.warnings,
        [ParseWarning::UnknownElement {
            path: r#"Target[@Name="t"]/Datas/Data[@Name="x"]/Bitz"#.into()
        }]
    );
    // Our hand-written fixture uses nothing the parser ignores.
    let (_, report) = ddt4all_report(&fixture("minimal_ecu.xml")).unwrap();
    assert_eq!(report.warnings, []);
}

#[test]
fn encode_exact_rejects_off_grid() {
    let e = ecu();
    let sp = e.data("setpoint").unwrap(); // 16 bit, Step 2
    assert_eq!(
        sp.encode_exact(&Value::Number(1502.0)).unwrap(),
        Raw::Int(751)
    );
    match sp.encode_exact(&Value::Number(1503.0)) {
        Err(SpecError::OffGrid {
            data,
            value,
            nearest,
        }) => assert_eq!(
            (data.as_str(), value, nearest),
            ("setpoint", 1503.0, 1504.0)
        ),
        other => panic!("expected OffGrid, got {other:?}"),
    }
    // encode keeps rounding.
    assert_eq!(sp.encode(&Value::Number(1503.0)).unwrap(), Raw::Int(752));
    // Non-number values are passed through to encode.
    let mode = e.data("mode").unwrap();
    assert_eq!(
        mode.encode_exact(&Value::Text("one".into())).unwrap(),
        Raw::Int(1)
    );
    // Fractional steps: 0.5 grid with offset -10.
    let wle = e.data("word le").unwrap();
    assert_eq!(
        wle.encode_exact(&Value::Number(2320.0)).unwrap(),
        Raw::Int(0x1234)
    );
    assert!(matches!(
        wle.encode_exact(&Value::Number(2320.25)),
        Err(SpecError::OffGrid { .. })
    ));
}

#[test]
fn little_endian_insert_is_true_inverse_not_ddt4all_set_value() {
    let e = ecu();
    let word = e.data("word le").unwrap(); // 16 bit
    let le = DataItem {
        name: "word le".into(),
        first_byte: 1,
        bit_offset: 0,
        reference: false,
        endian: Some(Endian::Little),
    };
    let mut frame = [0u8; 2];
    word.insert(&Raw::Int(0x1234), &mut frame, &le, Endian::Big)
        .unwrap();
    // ddt4all setValue writes 12 34 here (and reads it back as 0x3412).
    assert_eq!(frame, [0x34, 0x12]);
    assert_eq!(
        word.extract(&frame, &le, Endian::Big).unwrap(),
        [0x12, 0x34]
    );
    // Big endian stays in wire order.
    let be = DataItem {
        endian: Some(Endian::Big),
        ..le
    };
    let mut frame = [0u8; 2];
    word.insert(&Raw::Int(0x1234), &mut frame, &be, Endian::Big)
        .unwrap();
    assert_eq!(frame, [0x12, 0x34]);
}

#[test]
fn raw_int_accessor() {
    let e = ecu();
    let items = e
        .request("Read Block")
        .unwrap()
        .decode_checked(&e, &READ_BLOCK_FRAME)
        .unwrap();
    let get = |n: &str| items.iter().find(|i| i.name == n).unwrap();
    assert_eq!(get("word be").raw_int(), Some(0x1234));
    assert_eq!(get("flag msb").raw_int(), Some(1));
    assert_eq!(get("ident").raw_int(), None); // ASCII stays bytes
}

#[test]
fn first_byte_zero_is_rejected_at_parse_time() {
    let xml = target(
        r#"<Datas><Data Name="d"><Bits count="8"/></Data></Datas><Requests><Request Name="r"><Sent><SentBytes>2101</SentBytes></Sent><Received MinBytes="3"><DataItem Name="d" FirstByte="0"/></Received></Request></Requests>"#,
    );
    assert!(matches!(
        ddt4all(&xml),
        Err(SpecError::InvalidValue { what, value, .. }) if what == "attribute FirstByte" && value == "0"
    ));
}

#[test]
fn decode_checked_does_not_panic_on_hand_built_first_byte_zero() {
    let mut e = ecu();
    let mut r = e.request("Read Block").unwrap().clone();
    r.received_items[0].first_byte = 0;
    e.requests = vec![r.clone()];
    // No arithmetic on FirstByte any more; the item simply cannot be located.
    let items = r.decode_checked(&e, &READ_BLOCK_FRAME).unwrap();
    assert_eq!(items[0].raw, None);
}

#[test]
fn negative_response_for_another_sid_is_distinct() {
    let e = ecu();
    let rb = e.request("Read Block").unwrap(); // sends 21 01
    assert!(matches!(
        rb.decode_checked_against(&e, &[0x21, 0x01], &[0x7F, 0x22, 0x10]),
        Err(SpecError::NegativeResponseForOtherSid {
            expected: 0x21,
            got: 0x22,
            nrc: 0x10
        })
    ));
    assert!(matches!(
        rb.decode_checked_against(&e, &[0x21, 0x01], &[0x7F, 0x21, 0x10]),
        Err(SpecError::NegativeResponse {
            sid: 0x21,
            nrc: 0x10
        })
    ));
}

#[test]
fn report_follows_the_recursive_lookup_of_the_parser() {
    // The parser finds Sent/Received/DataItem/Bits at any depth below Request/Data.
    let xml = target(
        r#"<Datas><Data Name="d"><Wrap><Bits count="16"><Scaled Stepp="2"/></Bits></Wrap></Data></Datas><Requests><Request Name="r"><Group><Sent><SentBytes>2101</SentBytes></Sent></Group><Received MinBytes="3"><Wrapper><DataItem Name="d" FirstByte="3" Bogus="1"/></Wrapper></Received></Request></Requests>"#,
    );
    let (e, report) = ddt4all_report(&xml).unwrap();
    // What the parser reads...
    assert_eq!(e.data("d").unwrap().bits_count, 16);
    let r = e.request("r").unwrap();
    assert_eq!(r.sent_bytes.0, [0x21, 0x01]);
    assert_eq!(r.received_items.len(), 1);
    // ...is not reported as unread (its misspelled attributes are), only the wrappers are.
    let t = r#"Target[@Name="t"]"#;
    let unk = |path: String| ParseWarning::UnknownElement { path };
    let att = |path: String, name: &str| ParseWarning::UnknownAttribute {
        path,
        name: name.into(),
    };
    assert_eq!(
        report.warnings,
        [
            unk(format!(r#"{t}/Datas/Data[@Name="d"]/Wrap"#)),
            att(
                format!(r#"{t}/Datas/Data[@Name="d"]/Wrap/Bits/Scaled"#),
                "Stepp"
            ),
            unk(format!(r#"{t}/Requests/Request[@Name="r"]/Group"#)),
            unk(format!(
                r#"{t}/Requests/Request[@Name="r"]/Received/Wrapper"#
            )),
            att(
                format!(r#"{t}/Requests/Request[@Name="r"]/Received/Wrapper/DataItem[@Name="d"]"#),
                "Bogus"
            ),
        ]
    );
    // Not read, reported: a Scaled outside <Bits>, a DataItem directly under <Request>.
    let xml = target(
        r#"<Datas><Data Name="d"><Bitz><Scaled Step="2"/></Bitz></Data></Datas><Requests><Request Name="r"><DataItem Name="d" FirstByte="3"/></Request></Requests>"#,
    );
    let (e, report) = ddt4all_report(&xml).unwrap();
    assert!(e.data("d").unwrap().scaled.is_none());
    assert!(e.request("r").unwrap().received_items.is_empty());
    assert_eq!(report.warnings.len(), 2, "{:?}", report.warnings);
}

#[test]
fn encode_exact_reports_range_before_grid() {
    let e = ecu();
    let wle = e.data("word le").unwrap(); // 16 bit, Step .5, Offset -10: max 32757.5
                                          // Out of range AND off grid: an Encode error, not an OffGrid whose `nearest` cannot encode.
    let res = wle.encode_exact(&Value::Number(40000.25));
    assert!(matches!(res, Err(SpecError::Encode { .. })), "{res:?}");
    // In range and off grid: `nearest` is itself encodable.
    let Err(SpecError::OffGrid { nearest, .. }) = wle.encode_exact(&Value::Number(2320.25)) else {
        panic!("expected OffGrid");
    };
    assert!(wle.encode_exact(&Value::Number(nearest)).is_ok());
}

#[test]
fn allow_short_relaxes_only_min_bytes() {
    let e = ecu();
    let rb = e.request("Read Block").unwrap(); // MinBytes 18
    let short = &READ_BLOCK_FRAME[..10];
    assert!(matches!(
        rb.decode_checked(&e, short),
        Err(SpecError::TooShort { min: 18, len: 10 })
    ));
    let items = rb.decode_checked_allow_short(&e, short).unwrap();
    assert_eq!(items.len(), 13);
    assert!(items.iter().any(|i| i.raw.is_none()), "tail items are None");
    assert!(items.iter().any(|i| i.raw.is_some()), "head items decode");
    // SID, NRC and echo checks stay on.
    assert!(matches!(
        rb.decode_checked_allow_short(&e, &[0x7F, 0x21, 0x12]),
        Err(SpecError::NegativeResponse { .. })
    ));
    assert!(matches!(
        rb.decode_checked_allow_short(&e, &[0x7F]),
        Err(SpecError::TruncatedNegativeResponse { len: 1 })
    ));
    assert!(matches!(
        rb.decode_checked_allow_short(&e, &[0x62, 0x01]),
        Err(SpecError::UnexpectedSid { .. })
    ));
    assert!(matches!(
        rb.decode_checked_allow_short(&e, &[0x61, 0x05, 0x00]),
        Err(SpecError::ReplyMismatch { position: 2, .. })
    ));
    assert!(matches!(
        rb.decode_checked_allow_short(&e, &[0x61]),
        Err(SpecError::TooShort { min: 2, len: 1 })
    ));
}

#[test]
fn little_endian_12_bit_insert_round_trips() {
    let xml = target(r#"<Datas><Data Name="d"><Bits count="12"><Scaled/></Bits></Data></Datas>"#);
    let e = ddt4all(&xml).unwrap();
    let d = e.data("d").unwrap();
    let item = DataItem {
        name: "d".into(),
        first_byte: 1,
        bit_offset: 0,
        reference: false,
        endian: Some(Endian::Little),
    };
    // insert is the exact inverse of extract on every raw that extract can produce (the 12-bit
    // span value X comes back byte-swapped, ddt4all's odd result for LE items that are not a
    // multiple of 8 bits wide; docs/spec-semantics.md), and bits 12..16 are left untouched.
    for x in 0..0x1000u16 {
        let frame = (0xF000 | x).to_be_bytes();
        let raw = d.raw_from_bytes(&d.extract(&frame, &item, Endian::Big).unwrap());
        assert_eq!(raw, Raw::Int(u64::from(x.swap_bytes())), "x={x:#x}");
        let mut out = [0xFFu8, 0xFF];
        d.insert(&raw, &mut out, &item, Endian::Big).unwrap();
        assert_eq!(out, (0xF000 | x).to_be_bytes(), "x={x:#x}");
    }
    // A raw outside that set is refused instead of corrupting the neighbouring bits.
    let mut frame = [0u8; 2];
    assert!(matches!(
        d.insert(&Raw::Int(16), &mut frame, &item, Endian::Big),
        Err(SpecError::Encode { .. })
    ));
    assert_eq!(frame, [0, 0]);
}

#[test]
fn addressing_rejects_dtd() {
    let xml = br#"<?xml version="1.0"?><!DOCTYPE Functions [<!ENTITY a "x">]><Functions/>"#;
    assert!(matches!(
        Addressing::from_xml_bytes(xml),
        Err(SpecError::Xml(_))
    ));
}
