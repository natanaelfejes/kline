//! `decode_checked` on realistic Sirius 32 (EMS3132) frames built from the definition.
//!
//! KWP2000 positive responses are `<SID+0x40> <local id> <data...>`; `FirstByte` counts from
//! the SID byte (1-based). Skipped when `ddt-xml/SIRIUS32___EMS3132___Soft_RE1_maj.xml` is absent.

mod common;

use clio_spec::{EcuDef, HexBytes, Raw, Request, SpecError};
use common::ddt_ecu;

const FILE: &str = "SIRIUS32___EMS3132___Soft_RE1_maj.xml";

fn req<'a>(ecu: &'a EcuDef, sent_hex: &str) -> &'a Request {
    ecu.requests
        .iter()
        .find(|r| r.sent_bytes.to_hex() == sent_hex)
        .unwrap_or_else(|| panic!("no request {sent_hex}"))
}

/// A frame of `min_bytes` bytes: `61 <lid>` then a counting pattern, so every item has data.
fn frame(r: &Request, lid: u8) -> Vec<u8> {
    let mut f: Vec<u8> = (0..r.min_bytes as usize).map(|i| i as u8).collect();
    f[0] = 0x61;
    f[1] = lid;
    f
}

fn raw_at(r: &Request, items: &[clio_spec::DecodedItem], first_byte: u32) -> Option<Raw> {
    let i = r
        .received_items
        .iter()
        .position(|d| d.first_byte == first_byte)
        .unwrap();
    items[i].raw.clone()
}

#[test]
fn trame_10_21a9_is_accepted() {
    let Some(ecu) = ddt_ecu(FILE) else { return };
    let r = req(&ecu, "21A9");
    assert_eq!(r.min_bytes, 26);
    let mut f = frame(r, 0xA9);
    f[2] = 0x0B; // bytes 3-4: engine speed raw
    f[3] = 0xB8;
    let items = r.decode_checked(&ecu, &f).unwrap();
    assert_eq!(items.len(), 18);
    assert_eq!(raw_at(r, &items, 3), Some(Raw::Int(0x0BB8)));
    assert!(items.iter().all(|i| i.raw.is_some()));
}

#[test]
fn trame_11_21aa_is_accepted() {
    let Some(ecu) = ddt_ecu(FILE) else { return };
    let r = req(&ecu, "21AA");
    let items = r.decode_checked(&ecu, &frame(r, 0xAA)).unwrap();
    assert_eq!(items.len(), 9);
    assert!(items.iter().all(|i| i.raw.is_some()));
}

#[test]
fn identification_2180_is_accepted() {
    let Some(ecu) = ddt_ecu(FILE) else { return };
    let r = req(&ecu, "2180");
    let items = r.decode_checked(&ecu, &frame(r, 0x80)).unwrap();
    assert_eq!(items.len(), 9);
}

#[test]
fn trame_vin_2181_maps_the_echo_byte_and_is_accepted() {
    let Some(ecu) = ddt_ecu(FILE) else { return };
    let r = req(&ecu, "2181");
    let mut f = frame(r, 0x81);
    f[2..19].copy_from_slice(b"VF1CB0A0H12345678");
    let items = r.decode_checked(&ecu, &f).unwrap();
    // The definition maps byte 2 (the local id) as a one-byte item "LocalIvinOK".
    assert_eq!(raw_at(r, &items, 2), Some(Raw::Int(0x81)));
    // VINr is 17 raw bytes (not flagged ascii in the file), bytes 3-19.
    assert_eq!(
        raw_at(r, &items, 3),
        Some(Raw::Bytes(HexBytes(b"VF1CB0A0H12345678".to_vec())))
    );
}

#[test]
fn trame_10_response_is_rejected_for_the_vin_request() {
    let Some(ecu) = ddt_ecu(FILE) else { return };
    let vin = req(&ecu, "2181");
    let t10 = req(&ecu, "21A9");
    let f = frame(t10, 0xA9); // 26 bytes, longer than MinBytes 21 of the VIN request
    assert_eq!(
        vin.decode_checked(&ecu, &f).unwrap_err(),
        SpecError::ReplyMismatch {
            position: 2,
            expected: 0x81,
            got: 0xA9
        }
    );
}

#[test]
fn unmapped_payload_before_first_item_is_not_compared_with_request_parameters() {
    let Some(ecu) = ddt_ecu(FILE) else { return };
    // Hypothetical request: 21 80 plus parameters, first mapped item at byte 7. Bytes 3-6 of the
    // response are unmapped payload, not an echo of the parameters AA BB CC.
    let mut r = req(&ecu, "2180").clone();
    for item in &mut r.received_items {
        item.first_byte += 4;
    }
    r.min_bytes += 4;
    let mut f = frame(&r, 0x80);
    f[2..6].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
    let sent = [0x21, 0x80, 0xAA, 0xBB, 0xCC];
    assert!(r.decode_checked_against(&ecu, &sent, &f).is_ok());
}

#[test]
fn wrong_echo_is_rejected_for_other_requests_too() {
    let Some(ecu) = ddt_ecu(FILE) else { return };
    let r = req(&ecu, "21A9");
    let f = frame(r, 0xAA);
    assert!(matches!(
        r.decode_checked(&ecu, &f),
        Err(SpecError::ReplyMismatch { position: 2, .. })
    ));
}
