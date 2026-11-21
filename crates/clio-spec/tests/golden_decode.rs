//! Golden decodes of synthetic frames for the requests this project reads from the car.
//!
//! Every expected display string below was cross-checked by running the ddt4all reference
//! decoder on the same bytes (`python3 -I crates/clio-spec/tests/crosscheck_ddt4all.py
//! --ddt4all-src <ddt4all>/src`, GOLDEN section, run 2026-11-21 against ddt4all e109cd2).
//! Recorded ddt4all output (excerpt):
//! ```text
//! Safir_2.xml 'P2 - Ctrl entrees' frame=735100900008000000000000000BB8C80064C868A00100000A
//!   'depression altimètrique': '1.01'      'estompage de couple': 'TEMPORISE'
//!   'etat antivol': 'PROTEGE'              'position papillon': 'pied levé'
//!   'pression collecteur absolue mesuree': '0.84'   'regime moteur moyen': '3000'
//!   'temperature d air mesuree': '25.00'   'temperature eau mesuree': '85.00'
//!   'tension batterie': '12.99'            'tension sonde O2': '0.6'
//!   'vitesse du vehicule': 'VALIDE'
//! Safir_2.xml 'P3 - Ctrl sorties' frame=7352000000000000000000000000000000000000F0032003E878800000
//!   'avance à allumage': '9'   'facteur moyen correctin de richesse': '128'
//!   'régime de consigne pour régulation ralenti': '800'   'temps injection réel': '4.00'
//!   'valeur RCO théorique pour régulation ralenti': '-0.06'
//! BMT_X65_ph1_p.xml 'ETATS TIR' frame=61A385020300
//!   'CODAPV': '3'  'DCODCOND': '1'  'LO': '1'  'TIR RF OK': '1'  'TIR RF RECU': '1'
//!   'TIR RF VAL': '0'  'APPTIR': '0'  'CPE -TIR': '0'
//! ```
//! (all 71 items of the three frames matched; the same frames are built in the script.)

mod common;

use clio_spec::{Raw, SpecError};
use common::{ddt_ecu, display_of, number_of, raw_of};

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn safir2_p2_ctrl_entrees() {
    let Some(ecu) = ddt_ecu("Safir_2.xml") else {
        return;
    };
    let req = ecu.request("P2 - Ctrl entrees").unwrap();
    assert_eq!(req.sent_bytes.to_hex(), "3351");
    let mut f = [0u8; 25];
    f[0] = 0x73; // positive response to 0x33
    f[1] = 0x51;
    f[3] = 0x90; // byte 4: position papillon (2 bits @0) = 2, estompage (bit @3) = 1
    f[5] = 0x08; // byte 6: etat antivol (bit @4) = 1
    f[13] = 0x0B; // bytes 14-15: regime moteur moyen = 3000
    f[14] = 0xB8;
    f[15] = 200; // byte 16: pression collecteur
    f[17] = 100; // byte 18: tension sonde O2
    f[18] = 200; // byte 19: temperature eau
    f[19] = 104; // byte 20: temperature air
    f[20] = 160; // byte 21: tension batterie
    f[21] = 1; // byte 22: vitesse du vehicule (a List: valid flag)
    f[24] = 10; // byte 25: depression altimetrique
    let items = req.decode_checked(&ecu, &f).unwrap();
    assert_eq!(items.len(), 39);

    // coolant: 200 * 0.625 - 40 = 85.0 C, Format #0.00
    assert!(close(number_of(&items, "temperature eau mesuree"), 85.0));
    assert_eq!(
        display_of(&items, "temperature eau mesuree").as_deref(),
        Some("85.00")
    );
    assert_eq!(
        raw_of(&items, "temperature eau mesuree"),
        Some(Raw::Int(200))
    );
    // air: 104 * 0.625 - 40 = 25.0 C
    assert!(close(number_of(&items, "temperature d air mesuree"), 25.0));
    assert_eq!(
        display_of(&items, "temperature d air mesuree").as_deref(),
        Some("25.00")
    );
    // battery: 160 * 0.0312 + 8 = 12.992 V, shown with 2 decimals
    assert!(close(number_of(&items, "tension batterie"), 12.992));
    assert_eq!(
        display_of(&items, "tension batterie").as_deref(),
        Some("12.99")
    );
    let unit = items
        .iter()
        .find(|i| i.name == "tension batterie")
        .unwrap()
        .unit
        .clone();
    assert_eq!(unit.as_deref(), Some("V"));
    // rpm: 16 bit big endian 0x0BB8 = 3000
    assert!(close(number_of(&items, "regime moteur moyen"), 3000.0));
    assert_eq!(
        display_of(&items, "regime moteur moyen").as_deref(),
        Some("3000")
    );
    assert_eq!(raw_of(&items, "regime moteur moyen"), Some(Raw::Int(3000)));
    // MAP: (200 * 3.7 + 103) / 1000 = 0.843 bar
    assert!(close(
        number_of(&items, "pression collecteur absolue mesuree"),
        0.843
    ));
    assert_eq!(
        display_of(&items, "pression collecteur absolue mesuree").as_deref(),
        Some("0.84")
    );
    // O2: 100 * 6 / 1000 = 0.6 V, no Format -> Python str(float)
    assert_eq!(
        display_of(&items, "tension sonde O2").as_deref(),
        Some("0.6")
    );
    // altitude: (10 * -3.7 + 1046.5) / 1000 = 1.0095
    assert!(close(number_of(&items, "depression altimètrique"), 1.0095));
    assert_eq!(
        display_of(&items, "depression altimètrique").as_deref(),
        Some("1.01")
    );
    // lists and bit offsets (MSB = BitOffset 0)
    assert_eq!(
        display_of(&items, "position papillon").as_deref(),
        Some("pied levé")
    );
    assert_eq!(
        display_of(&items, "estompage de couple").as_deref(),
        Some("TEMPORISE")
    );
    assert_eq!(
        display_of(&items, "etat antivol").as_deref(),
        Some("PROTEGE")
    );
    assert_eq!(
        display_of(&items, "vitesse du vehicule").as_deref(),
        Some("VALIDE")
    );
    assert_eq!(
        display_of(&items, "compresseur de climatisation").as_deref(),
        Some("inactif")
    );
}

#[test]
fn safir2_p3_ctrl_sorties() {
    let Some(ecu) = ddt_ecu("Safir_2.xml") else {
        return;
    };
    let req = ecu.request("p3 - ctrl sorties").unwrap(); // case-insensitive lookup
    assert_eq!(req.sent_bytes.to_hex(), "3352");
    let mut f = [0u8; 29];
    f[0] = 0x73;
    f[1] = 0x52;
    f[20] = 0xF0; // byte 21: RCO ralenti, signed -> -16
    f[21] = 0x03; // bytes 22-23: regime de consigne = 800
    f[22] = 0x20;
    f[23] = 0x03; // bytes 24-25: temps injection = 1000
    f[24] = 0xE8;
    f[25] = 120; // byte 26: avance
    f[26] = 128; // byte 27: facteur richesse
    let items = req.decode_checked(&ecu, &f).unwrap();
    assert_eq!(items.len(), 19);
    // injection time: 1000 * 4 / 1000 = 4.0 ms (16 bit x 4 us)
    assert!(close(number_of(&items, "temps injection réel"), 4.0));
    assert_eq!(
        display_of(&items, "temps injection réel").as_deref(),
        Some("4.00")
    );
    assert_eq!(raw_of(&items, "temps injection réel"), Some(Raw::Int(1000)));
    // advance: 120 * 0.375 - 36 = 9 degrees
    assert!(close(number_of(&items, "avance à allumage"), 9.0));
    assert_eq!(
        display_of(&items, "avance à allumage").as_deref(),
        Some("9")
    );
    // idle target 800 rpm
    assert_eq!(
        display_of(&items, "régime de consigne pour régulation ralenti").as_deref(),
        Some("800")
    );
    // signed 8 bit: 0xF0 = -16 -> -16 * 0.392 / 100 = -0.06272
    let rco = "valeur RCO théorique pour régulation ralenti";
    assert!(close(number_of(&items, rco), -0.06272));
    assert_eq!(display_of(&items, rco).as_deref(), Some("-0.06"));
    assert_eq!(raw_of(&items, rco), Some(Raw::Int(0xF0)));
    assert_eq!(
        display_of(&items, "facteur moyen correctin de richesse").as_deref(),
        Some("128")
    );

    // A truncated frame is rejected by the checked decode (MinBytes 29)...
    assert!(matches!(
        req.decode_checked(&ecu, &f[..23]),
        Err(SpecError::TooShort { min: 29, len: 23 })
    ));
    // ...and the unchecked one yields None for the items beyond it, like ddt4all.
    let short = req.decode_unchecked(&ecu, &f[..23]).unwrap();
    assert_eq!(display_of(&short, "temps injection réel"), None);
    assert_eq!(raw_of(&short, "temps injection réel"), None);
    assert_eq!(display_of(&short, rco).as_deref(), Some("-0.06"));
}

#[test]
fn bmt_etats_tir() {
    let Some(ecu) = ddt_ecu("BMT_X65_ph1_p.xml") else {
        return;
    };
    let req = ecu.request("ETATS TIR").unwrap();
    assert_eq!(req.sent_bytes.to_hex(), "21A3");
    // byte 3 = 1000_0101: DCODCOND (@0, MSB) = 1, TIR RF OK (@5) = 1, TIR RF VAL (@6) = 0,
    // TIR RF RECU (@7, LSB) = 1. byte 4 = 0000_0010: LO (@6) = 1. byte 5: CODAPV (2 bits @6) = 3.
    let f = [0x61, 0xA3, 0x85, 0x02, 0x03, 0x00];
    let items = req.decode_checked(&ecu, &f).unwrap();
    assert_eq!(items.len(), 13);
    for (name, want) in [
        ("DCODCOND", "1"),
        ("TIR RF OK", "1"),
        ("TIR RF VAL", "0"),
        ("TIR RF RECU", "1"),
        ("APPTIR", "0"),
        ("CODIRAPP1", "0"),
        ("CODIRAPP2", "0"),
        ("LO", "1"),
        ("LF", "0"),
        ("CPE -TIR", "0"),
        ("CODAPV", "3"),
    ] {
        assert_eq!(display_of(&items, name).as_deref(), Some(want), "{name}");
    }
    assert_eq!(raw_of(&items, "CODAPV"), Some(Raw::Int(3)));
}

#[test]
fn checked_decode_rejects_invalid_responses() {
    let Some(ecu) = ddt_ecu("BMT_X65_ph1_p.xml") else {
        return;
    };
    let req = ecu.request("ETATS TIR").unwrap(); // 21 A3, MinBytes 6
    let ok = [0x61, 0xA3, 0x85, 0x02, 0x03, 0x00];
    // ISO 14230 NRC 0x12 subFunctionNotSupported-invalidFormat.
    assert!(matches!(
        req.decode_checked(&ecu, &[0x7F, 0x21, 0x12]),
        Err(SpecError::NegativeResponse {
            sid: 0x21,
            nrc: 0x12
        })
    ));
    // The answer to a different local identifier (21 A4).
    let mut other = ok;
    other[1] = 0xA4;
    assert!(matches!(
        req.decode_checked(&ecu, &other),
        Err(SpecError::ReplyMismatch {
            position: 2,
            expected: 0xA3,
            got: 0xA4
        })
    ));
    // The answer to a different service.
    let mut other = ok;
    other[0] = 0x73;
    assert!(matches!(
        req.decode_checked(&ecu, &other),
        Err(SpecError::UnexpectedSid {
            expected: 0x61,
            got: 0x73
        })
    ));
    assert!(matches!(
        req.decode_checked(&ecu, &ok[..5]),
        Err(SpecError::TooShort { min: 6, len: 5 })
    ));
    // Format="00" item: the raw integer is available for the caller to format.
    let items = req.decode_checked(&ecu, &ok).unwrap();
    let codapv = items.iter().find(|i| i.name == "CODAPV").unwrap();
    assert_eq!(codapv.raw_int(), Some(3));
}
