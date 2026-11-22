//! DDT2000 semantics (default `Semantics::Ddt2000`), on a hand-written fixture.
//! Every expectation cites the RE evidence in the author's DDT2000 behaviour notes.

mod common;

use clio_spec::{EcuDef, ParseWarning, Raw, Semantics, SpecError, Value};
use common::{display_of, fixture, number_of};

fn ecu() -> EcuDef {
    EcuDef::from_xml_bytes(&fixture("ddt2000_ecu.xml")).expect("fixture parses")
}

fn block_frame() -> Vec<u8> {
    let mut f = vec![0x61, 0x01];
    f.push(0xFF); // 3 signed zero
    f.push(0x80); // 4 nib signed (top nibble 8)
    f.extend([0xFF, 0xF0]); // 5-6 twelve signed (0xFFF)
    f.extend([0x34, 0x12]); // 7-8 twelve le
    f.extend([0x0A, 0x0B, 0x0C, 0x0D, 0x0E]); // 9-13 big unsigned
    f.push(0x01); // 14 list and scaled
    f.push(0x80); // 15 one bit scaled (MSB)
    f.push(0xC8); // 16 plain
    f.push(0x03); // 17 pad
    f.extend(10324u16.to_be_bytes()); // 18-19 round
    f.push(24); // 20 volts
    f.push(0x05); // 21 flags
    f.push(0x07); // 22 state (not in list)
    f.extend([0xAB, 0xCD]); // 23-24 raw
    f.extend([b'C', b'a', 0xE9]); // 25-27 text (cp1252 e-acute)
    f
}

#[test]
fn default_semantics_is_ddt2000() {
    let e = ecu();
    assert_eq!(e.semantics, Semantics::Ddt2000);
    assert!(e.datas.iter().all(|d| d.semantics == Semantics::Ddt2000));
}

#[test]
fn signed_is_vb_cbool_and_sign_extends_at_field_width() {
    let e = ecu();
    assert!(
        !e.data("signed zero").unwrap().signed,
        "CBool(\"0\") is False"
    );
    let items = e
        .request("Block")
        .unwrap()
        .decode_checked(&e, &block_frame())
        .unwrap();
    assert_eq!(number_of(&items, "signed zero"), 255.0);
    assert_eq!(number_of(&items, "nib signed"), -8.0);
    assert_eq!(number_of(&items, "twelve signed"), -1.0);
}

#[test]
fn little_endian_is_lsb_based() {
    let e = ecu();
    let items = e
        .request("Block")
        .unwrap()
        .decode_checked(&e, &block_frame())
        .unwrap();
    // DiagLibs2 LittleEndian: b2 b1 >> 0, masked to 12 bits.
    assert_eq!(number_of(&items, "twelve le"), 564.0);
}

#[test]
fn kinds_list_scaled_one_bit_and_byte_arrays() {
    let e = ecu();
    let items = e
        .request("Block")
        .unwrap()
        .decode_checked(&e, &block_frame())
        .unwrap();
    let get = |n: &str| items.iter().find(|i| i.name == n).unwrap().clone();
    // Bits >= 32 unsigned: byte array, uppercase hex, never scaled.
    let big = get("big unsigned");
    assert_eq!(big.value, Some(Value::Hex("0A0B0C0D0E".into())));
    assert_eq!(
        big.raw,
        Some(Raw::Bytes(clio_spec::HexBytes(vec![10, 11, 12, 13, 14])))
    );
    // List wins over Scaled: label, no unit.
    let ls = get("list and scaled");
    assert_eq!(ls.display.as_deref(), Some("one"));
    assert_eq!(ls.unit, None);
    // 1-bit item: never scaled (raw 1, not 1 * 5 + 1).
    assert_eq!(display_of(&items, "one bit scaled").as_deref(), Some("1"));
    // A numeric item without Scaled is still a number (Step 1), shown in decimal.
    assert_eq!(display_of(&items, "plain").as_deref(), Some("200"));
}

#[test]
fn display_follows_vb_format() {
    let e = ecu();
    let items = e
        .request("Block")
        .unwrap()
        .decode_checked(&e, &block_frame())
        .unwrap();
    assert_eq!(display_of(&items, "pad").as_deref(), Some("03"));
    assert_eq!(display_of(&items, "round").as_deref(), Some("1032"));
    assert_eq!(display_of(&items, "volts").as_deref(), Some("01.50000"));
    assert_eq!(display_of(&items, "flags").as_deref(), Some("00000101"));
    assert_eq!(display_of(&items, "state").as_deref(), Some("##7##"));
    assert_eq!(display_of(&items, "raw").as_deref(), Some("ABCD"));
    assert_eq!(display_of(&items, "text").as_deref(), Some("Ca\u{e9}"));
}

#[test]
fn first_byte_and_min_bytes_default_to_one_with_warnings() {
    let (e, report) =
        EcuDef::from_xml_bytes_with_report(&fixture("ddt2000_ecu.xml")).expect("parses");
    let r = e.request("No first byte").unwrap();
    assert_eq!(r.received_items[0].first_byte, 1);
    assert_eq!(e.request("Block").unwrap().min_bytes, 1);
    let defaulted: Vec<(String, String)> = report
        .warnings
        .iter()
        .filter_map(|w| match w {
            ParseWarning::Defaulted { name, value, .. } => Some((name.clone(), value.clone())),
            _ => None,
        })
        .collect();
    assert!(
        defaulted.contains(&("FirstByte".into(), "1".into())),
        "{defaulted:?}"
    );
    assert!(
        defaulted.contains(&("MinBytes".into(), "1".into())),
        "{defaulted:?}"
    );
    // ddt4all semantics keep the strict behaviour.
    assert!(matches!(
        EcuDef::from_xml_bytes_as(&fixture("ddt2000_ecu.xml"), Semantics::Ddt4all),
        Err(SpecError::MissingFirstByte { .. })
    ));
}

#[test]
fn span_over_four_bytes_is_rejected_at_load() {
    let xml = br#"<Target Name="t" xmlns="http://www-diag.renault.com/2002/ECU"><Datas>
<Data Name="w"><Bits count="32" signed="1"/></Data></Datas><Requests>
<Request Name="r"><Sent><SentBytes>2101</SentBytes></Sent><Received MinBytes="7">
<DataItem Name="w" FirstByte="3" BitOffset="1"/></Received></Request></Requests></Target>"#;
    assert!(matches!(
        EcuDef::from_xml_bytes(xml),
        Err(SpecError::SpanTooLong { span: 5, .. })
    ));
    assert!(EcuDef::from_xml_bytes_as(xml, Semantics::Ddt4all).is_ok());
}

#[test]
fn little_endian_insert_inverts_extract() {
    let e = ecu();
    let r = e.request("Write le").unwrap();
    let frame = r
        .build(&e, &[("twelve le", Value::Number(2748.0))])
        .unwrap();
    assert_eq!(frame, vec![0x3B, 0x04, 0xC0, 0xAB, 0x00]);
}

#[test]
fn failures_and_devices_are_parsed() {
    let e = ecu();
    let f = e.failures.as_ref().unwrap();
    assert_eq!((f.name.as_str(), f.id), ("Flags", 1));
    assert!(f.is_flags());
    assert_eq!(e.devices.len(), 2);
    assert_eq!(e.devices[1].device_type.as_deref(), Some("2"));
    assert!(e.devices[0].data[0].failure_flag);
    assert!(!e.devices[0].data[1].failure_flag);
}
