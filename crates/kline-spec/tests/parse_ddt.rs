//! Parsing of the real DDT2000 files in ddt-xml/ (skipped when absent).

mod common;

use common::{ddt_ecu, ddt_file};
use kline_spec::{Addressing, Endian, KProtocol};

/// (file, requests, datas, data items (sent + received), auto idents)
///
/// Counted independently of this crate, once, with Python's ElementTree:
/// ```text
/// python3 -c "import xml.etree.ElementTree as ET,sys; NS='{http://www-diag.renault.com/2002/ECU}';
///   r=ET.parse(sys.argv[1]).getroot();
///   print(*[len(r.findall('.//'+NS+t)) for t in ('Request','Data','DataItem','AutoIdent')])" FILE
/// ```
/// No file has duplicate Request, Data or per-request DataItem names (also checked with
/// ElementTree), so these equal ddt4all's dict sizes; crosscheck_ddt4all.py confirms the
/// request and data counts against ddt4all itself.
const COUNTS: &[(&str, usize, usize, usize, usize)] = &[
    ("Safir_1.xml", 15, 151, 151, 0),
    ("Safir_2.xml", 15, 112, 112, 0),
    ("BMT_X65_ph1_p.xml", 15, 143, 143, 0),
    ("BMT_SAGEM.xml", 15, 139, 139, 0),
    ("SIRIUS32___EMS3132___Soft_RE1_maj.xml", 34, 724, 728, 15),
    ("S2000_Atmo___SoftA3.xml", 55, 892, 892, 37),
];

#[test]
fn all_files_parse_with_expected_counts() {
    for &(file, requests, datas, items, idents) in COUNTS {
        let Some(ecu) = ddt_ecu(file) else { continue };
        assert_eq!(ecu.requests.len(), requests, "{file} requests");
        assert_eq!(ecu.datas.len(), datas, "{file} datas");
        let n_items: usize = ecu
            .requests
            .iter()
            .map(|r| r.sent_items.len() + r.received_items.len())
            .sum();
        assert_eq!(n_items, items, "{file} data items");
        assert_eq!(ecu.auto_idents.len(), idents, "{file} auto idents");
        assert_eq!(ecu.endian, Endian::Big, "{file} Requests@Endian");
        // Every DataItem refers to an existing Data.
        for r in &ecu.requests {
            for i in r.sent_items.iter().chain(&r.received_items) {
                assert!(
                    ecu.data(&i.name).is_some(),
                    "{file}: {} -> {}",
                    r.name,
                    i.name
                );
            }
        }
        // The whole model exports to JSON.
        let json = serde_json::to_string(&ecu).expect("serialize");
        assert!(json.contains(&ecu.name));
    }
}

#[test]
fn safir2_k_section_and_identity() {
    let Some(ecu) = ddt_ecu("Safir_2.xml") else {
        return;
    };
    assert_eq!(ecu.name, "Safir 2");
    let f = ecu.function.as_ref().unwrap();
    assert_eq!((f.name.as_str(), f.address), ("Injection", 0x7A));
    assert!(ecu
        .description
        .as_deref()
        .unwrap()
        .starts_with("ESSENCE D7F X06"));
    let ps = ecu.power_supply.as_ref().unwrap();
    assert!(ps.ground && ps.vbat && ps.after_contact);
    let k = ecu.k.as_ref().unwrap();
    assert_eq!(k.baud_rate, Some(10417));
    assert_eq!(k.protocol(), KProtocol::Iso8);
    let iso8 = k.iso8.as_ref().unwrap();
    assert!(iso8.l_line);
    assert_eq!((iso8.kw1, iso8.kw2), (Some(194), Some(133)));
    assert_eq!(iso8.kw2_timeout, Some(500));
    assert_eq!(iso8.first_request_delay, Some(20));
    assert_eq!(k.key_words(), Some((0xC2, 0x85)));
    assert_eq!(k.inter_byte_delay, Some(0));
    assert_eq!(k.reply_to_request_delay, Some(5));
    assert_eq!(k.request_to_reply_timeout, Some(300));
    assert_eq!(k.tester_present_delay, Some(3000));
    assert_eq!(k.on_error_delay, Some(300));
    assert!(k.kwp.is_none());
    assert!(ecu.can.is_none());

    let p2 = ecu.request("P2 - Ctrl entrees").unwrap();
    assert_eq!(p2.sent_bytes.0, vec![0x33, 0x51]);
    assert_eq!(p2.min_bytes, 25);
    assert_eq!(
        p2.deny_access,
        ["Plant", "AfterSales", "Engineering", "Supplier"]
    );
    assert_eq!(p2.sent_items.len(), 1);
    assert_eq!(p2.sent_items[0].name, "mode du diagnostic P2");
    assert_eq!(p2.received_items.len(), 39);
    let generic = ecu.request("Requete generique").unwrap();
    assert!(generic.manual_send && generic.variable_length);
    let et2 = ecu.request("ET2 - Contenu espace mémoire").unwrap();
    let d1 = et2
        .received_items
        .iter()
        .find(|i| i.name == "DATA 1")
        .unwrap();
    assert_eq!((d1.first_byte, d1.bit_offset, d1.reference), (3, 7, true));
    assert_eq!(
        ecu.request_by_bytes("3352").unwrap()[0].name,
        "P3 - Ctrl sorties"
    );
}

#[test]
fn safir1_key_words() {
    let Some(ecu) = ddt_ecu("Safir_1.xml") else {
        return;
    };
    let iso8 = ecu.k.as_ref().unwrap().iso8.as_ref().unwrap();
    assert_eq!((iso8.kw1, iso8.kw2), (Some(28), Some(134)));
    // Safir 1 has little-endian 1-bit items.
    let n_le = ecu
        .requests
        .iter()
        .flat_map(|r| &r.received_items)
        .filter(|i| i.endian == Some(Endian::Little))
        .count();
    assert_eq!(n_le, 2);
}

#[test]
fn bmt_x65_k_section_and_windows1252() {
    let Some(ecu) = ddt_ecu("BMT_X65_ph1_p.xml") else {
        return;
    };
    assert_eq!(ecu.function.as_ref().unwrap().address, 0x26);
    assert_eq!(ecu.projects, ["x65"]);
    let k = ecu.k.as_ref().unwrap();
    assert_eq!(k.baud_rate, Some(10400));
    assert_eq!(k.protocol(), KProtocol::KwpSlowInit);
    let kwp = k.kwp.as_ref().unwrap();
    assert_eq!(kwp.multipoint, Some(true));
    assert_eq!(kwp.kw2_delay, Some(25));
    let iso8 = kwp.iso8.as_ref().unwrap();
    assert_eq!((iso8.kw1, iso8.kw2), (Some(208), Some(143)));
    assert_eq!(iso8.kw2_timeout, Some(300));
    assert_eq!(k.tester_present_delay, Some(3000));
    // windows-1252 declared: 0xE9 must come out as 'é'.
    assert!(ecu.data("ACC présent").is_some());
    let r = ecu.request("ETATS TIR").unwrap();
    assert!(r.dossier_maintenabilite && r.deny_access.is_empty());
    let both: Vec<_> = ecu
        .request_by_bytes("2180")
        .unwrap()
        .iter()
        .map(|r| r.name.clone())
        .collect();
    assert_eq!(both, ["IDENTIFICATION", "TYPE DE CARTE"]);
}

#[test]
fn s2000_fast_init_and_auto_idents() {
    let Some(ecu) = ddt_ecu("S2000_Atmo___SoftA3.xml") else {
        return;
    };
    let k = ecu.k.as_ref().unwrap();
    assert_eq!(k.protocol(), KProtocol::KwpFastInit);
    let kwp = k.kwp.as_ref().unwrap();
    assert_eq!(kwp.multipoint, Some(false));
    let fi = kwp.fast_init.as_ref().unwrap();
    assert_eq!((fi.kw1, fi.kw2), (Some(241), Some(122)));
    assert!(kwp.iso8.is_none());
    let first = &ecu.auto_idents[0];
    assert_eq!(
        (
            first.diag_version.as_str(),
            first.supplier.as_str(),
            first.soft.as_str(),
            first.version.as_str()
        ),
        ("4", "213", "00A3", "7000")
    );
    assert_eq!(ecu.projects.len(), 7);
}

#[test]
fn x65_addressing() {
    let Some(bytes) = ddt_file("x65_addressing.xml") else {
        return;
    };
    let a = Addressing::from_xml_bytes(&bytes).unwrap();
    assert_eq!(a.tool_address, Some(241));
    assert_eq!(a.protocols.len(), 7); // grep -c '<Protocol ' in the top-level ProtocolList
    assert_eq!(a.functions.len(), 10); // grep -c '<Function ' x65_addressing.xml
    let ecm = a.function(122).unwrap();
    assert_eq!(ecm.name, "ECM");
    assert_eq!((ecm.xid, ecm.rid), (Some(2016), Some(2024)));
    assert_eq!(ecm.protocols.len(), 2);
    let uch = a.function(38).unwrap();
    assert_eq!(uch.iso8_address, Some(174));
    let eps = a.function(4).unwrap();
    assert_eq!(eps.names[1].text, "Direction assistée"); // windows-1252 0xE9
    assert_eq!(eps.names[1].lang.as_deref(), Some("fr"));
}

/// Every element and attribute of the six files that the parser does not read, by local name
/// (`Element@attribute` for attributes). Changing the parser or the files changes this list.
/// The ignored elements are DDT2000's screen layout (`Categories`), `Language` and
/// `DateOfficialisation`; the attributes are publication metadata. No DDT2000 default is
/// applied anywhere in the six files (no `Defaulted` warning).
/// Cross-checked against an ElementTree listing of all element/attribute names in the files.
const UNKNOWN_SNAPSHOT: &[&str] = &[
    "Categories",
    "DateOfficialisation",
    "Language",
    "Target@email",
    "Target@href",
    "Target@upload",
];

#[test]
fn unknown_names_snapshot() {
    use kline_spec::{EcuDef, ParseWarning};
    use std::collections::BTreeSet;
    let mut seen = BTreeSet::new();
    let mut files = 0;
    for &(file, ..) in COUNTS {
        let Some(bytes) = ddt_file(file) else {
            continue;
        };
        files += 1;
        let (_, report) = EcuDef::from_xml_bytes_with_report(&bytes).unwrap();
        for w in report.warnings {
            let last = |p: &str| {
                let seg = p.rsplit('/').next().unwrap();
                seg.split('[').next().unwrap().to_string()
            };
            seen.insert(match w {
                ParseWarning::UnknownElement { path } => last(&path),
                ParseWarning::UnknownAttribute { path, name } => format!("{}@{name}", last(&path)),
                ParseWarning::Defaulted { path, name, .. } => {
                    format!("DEFAULT {}@{name}", last(&path))
                }
            });
        }
    }
    if files < COUNTS.len() {
        eprintln!(
            "SKIPPED: unknown_names_snapshot needs all {} files",
            COUNTS.len()
        );
        return;
    }
    let seen: Vec<&str> = seen.iter().map(String::as_str).collect();
    assert_eq!(seen, UNKNOWN_SNAPSHOT);
}
