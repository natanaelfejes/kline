//! DDT2000's AutoIdent matching (the author's DDT2000 behaviour notes "AutoIdent").
//!
//! RE: DDT2000xmlSelector XPath literals `[(number(@DiagVersion) = "N") and
//! (normalize-space(@Supplier) = normalize-space("S")) and ...]`, and DiagScanXml 0x5005e9c0 /
//! 0x5005dc50 match levels `MatchExact`, `MatchUnique`, `MatchSoft`, `MatchDiag`,
//! `MatchSoftDiag` ("Identification exact match", "Only one Ecu in function", "Finding
//! identification with different Soft version", "... different diag version", "... different
//! Diag and Soft version").

use serde::Serialize;

use crate::model::{AutoIdent, EcuDef};

/// The four AutoIdent keys of an ECU's identification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ident {
    pub diag_version: u32,
    pub supplier: String,
    pub soft: String,
    pub version: String,
}

impl Ident {
    /// From a Renault `21 80` reply (`61 80 ...`, positions 1-based from the SID): diagnostic
    /// version byte 8 (decimal), supplier bytes 9-11 (ASCII), program ("Soft") bytes 17-18 and
    /// software version bytes 19-20 (uppercase hex). DDT2000 does this in `ReqIdent.xsl`, which is
    /// not in the dump; the layout is taken from the Sirius 32 / S2000 `Identification` items
    /// (`N° diag` 8, `N° fournisseur` 9 ascii, `N° programme` 17, `N° version du logiciel` 19)
    /// and agrees with their AutoIdent values (Supplier "001", Soft "00E1", Version "7600").
    /// `None` when the reply is shorter than 20 bytes or not `61 80`.
    pub fn from_renault_2180(reply: &[u8]) -> Option<Ident> {
        if reply.len() < 20 || reply[0] != 0x61 || reply[1] != 0x80 {
            return None;
        }
        let hex = |b: &[u8]| -> String { b.iter().map(|x| format!("{x:02X}")).collect() };
        let (s, _, _) = encoding_rs::WINDOWS_1252.decode(&reply[8..11]);
        Some(Ident {
            diag_version: u32::from(reply[7]),
            supplier: s.into_owned(),
            soft: hex(&reply[16..18]),
            version: hex(&reply[18..20]),
        })
    }
}

/// DDT2000's match levels, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchLevel {
    /// All four keys equal.
    Exact,
    /// Only one definition for this function: taken whatever the ident.
    Unique,
    /// Same DiagVersion, Supplier and Soft; Version differs ("different Soft version").
    Soft,
    /// Same Supplier, Soft and Version; DiagVersion differs.
    Diag,
    /// Same Supplier and Soft; DiagVersion and Version differ ("estimate").
    SoftDiag,
}

/// XPath `normalize-space()`: trim, and collapse inner whitespace runs to one space.
pub fn normalize_space(s: &str) -> String {
    s.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

/// XPath `number(@DiagVersion) = N`: the attribute as a number (NaN never matches).
fn diag_eq(a: &AutoIdent, n: u32) -> bool {
    a.diag_version
        .trim()
        .parse::<f64>()
        .is_ok_and(|x| x == f64::from(n))
}

fn str_eq(a: &str, b: &str) -> bool {
    normalize_space(a) == normalize_space(b)
}

fn level_of(a: &AutoIdent, id: &Ident) -> Option<MatchLevel> {
    let d = diag_eq(a, id.diag_version);
    let s = str_eq(&a.supplier, &id.supplier);
    let p = str_eq(&a.soft, &id.soft);
    let v = str_eq(&a.version, &id.version);
    match (d, s, p, v) {
        (true, true, true, true) => Some(MatchLevel::Exact),
        (true, true, true, false) => Some(MatchLevel::Soft),
        (false, true, true, true) => Some(MatchLevel::Diag),
        (_, true, true, _) => Some(MatchLevel::SoftDiag),
        _ => None,
    }
}

/// Best definition among `candidates` (all for the same function) for `id`, with its level,
/// following DDT2000's ladder: an exact AutoIdent match anywhere; else the only candidate;
/// else Soft, Diag, SoftDiag in that order. Ties go to the first candidate in order.
pub fn best_match(candidates: &[&EcuDef], id: &Ident) -> Option<(usize, MatchLevel)> {
    let best_at = |want: MatchLevel| {
        candidates
            .iter()
            .position(|d| d.auto_idents.iter().any(|a| level_of(a, id) == Some(want)))
    };
    if let Some(i) = best_at(MatchLevel::Exact) {
        return Some((i, MatchLevel::Exact));
    }
    if candidates.len() == 1 {
        return Some((0, MatchLevel::Unique));
    }
    [MatchLevel::Soft, MatchLevel::Diag, MatchLevel::SoftDiag]
        .into_iter()
        .find_map(|l| best_at(l).map(|i| (i, l)))
}
