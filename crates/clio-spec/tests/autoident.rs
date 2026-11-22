//! DDT2000 AutoIdent ladder (DDT2000xmlSelector XPath, DiagScanXml match levels).

use clio_spec::{best_match, EcuDef, Ident, MatchLevel};

fn def(idents: &[(&str, &str, &str, &str)]) -> EcuDef {
    let ai: String = idents
        .iter()
        .map(|(d, s, p, v)| {
            format!(r#"<AutoIdent DiagVersion="{d}" Supplier="{s}" Soft="{p}" Version="{v}"/>"#)
        })
        .collect();
    let xml = format!(
        r#"<Target Name="t" xmlns="http://www-diag.renault.com/2002/ECU"><AutoIdents>{ai}</AutoIdents></Target>"#
    );
    EcuDef::from_xml_bytes(xml.as_bytes()).unwrap()
}

fn id(d: u32, s: &str, p: &str, v: &str) -> Ident {
    Ident {
        diag_version: d,
        supplier: s.into(),
        soft: p.into(),
        version: v.into(),
    }
}

#[test]
fn renault_2180_layout() {
    let mut r = vec![
        0x61, 0x80, 1, 2, 3, 4, 5, 0x0C, b'0', b'0', b'1', 0, 0, 0, 0, 0, 0x00, 0xE1, 0x92, 0x00,
        0, 0, 0, 0,
    ];
    let i = Ident::from_renault_2180(&r).unwrap();
    assert_eq!(i, id(12, "001", "00E1", "9200"));
    r.truncate(19);
    assert_eq!(Ident::from_renault_2180(&r), None);
}

#[test]
fn diag_version_is_numeric_and_strings_are_normalized_but_case_sensitive() {
    let a = def(&[(" 04", " 001 ", "00E1", "7600")]);
    let b = def(&[("4", "001", "00e1", "7600")]);
    let i = id(4, "001", "00E1", "7600");
    assert_eq!(best_match(&[&a, &b], &i), Some((0, MatchLevel::Exact)));
    // Lowercase "00e1" is not equal (XPath = is case-sensitive): b matches nothing exactly.
    assert_eq!(best_match(&[&b, &b], &i), None);
}

#[test]
fn ladder_order() {
    let soft = def(&[("4", "001", "00E1", "1111")]);
    let diag = def(&[("9", "001", "00E1", "7600")]);
    let both = def(&[("9", "001", "00E1", "2222")]);
    let other = def(&[("4", "002", "00E1", "7600")]);
    let i = id(4, "001", "00E1", "7600");
    assert_eq!(best_match(&[&other], &i), Some((0, MatchLevel::Unique)));
    assert_eq!(
        best_match(&[&other, &both, &diag, &soft], &i),
        Some((3, MatchLevel::Soft))
    );
    assert_eq!(
        best_match(&[&other, &both, &diag], &i),
        Some((2, MatchLevel::Diag))
    );
    assert_eq!(
        best_match(&[&other, &both], &i),
        Some((1, MatchLevel::SoftDiag))
    );
    assert_eq!(best_match(&[&other, &other], &i), None);
}
