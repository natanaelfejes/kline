//! `EcuDef` / `<K>` -> [`LinkConfig`] mapping (feature `spec`, std only).
//!
//! Missing XML values fall back to the defaults of the DDT2000 protocol class that would read
//! the file (RE: KWP2000Diag4 ISO8Ecu / KWP2000Ecu constructors). These are the monopoint
//! values; the fixed Multipoint values are applied by `LinkConfig::effective_timing` only under
//! `InitVariant::MultiEcu`. The baud rate is always
//! [`consts::DEFAULT_BAUD`]; the XML value is in `KLine::baud_rate` if a caller wants it.

use kline_spec::{EcuDef, KLine, KProtocol};
use thiserror::Error;

use crate::safety::{DefinitionGuard, EcuClass};
use crate::timing::{consts, InitKind, KTiming, LinkConfig};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SpecMapError {
    #[error("definition has no <K> section")]
    NoKLine,
    #[error("definition has no <Function Address>")]
    NoAddress,
    #[error("<K> declares neither ISO8 nor KWP init")]
    UnknownProtocol,
}

/// ISO8Ecu defaults for absent XML values. RE: KWP2000Diag4 ISO8Ecu constructor.
pub const ISO8_CLASS_DEFAULTS: KTiming = KTiming {
    baud: consts::DEFAULT_BAUD,
    init: InitKind::Iso8FiveBaud,
    // Not set by ISO8Ecu: IOTBD32V3 default (unused, ISO8 has no ~KW2 handshake).
    kw2_delay_ms: 25,
    first_request_ms: 200,
    inter_byte_ms: 2,
    reply_to_request_ms: 100,
    request_to_reply_timeout_ms: 100,
    tester_present_ms: 3000,
    on_error_ms: 1000,
    kw2_timeout_ms: 1000,
};

/// KWP2000Ecu defaults for absent XML values. RE: KWP2000Diag4 KWP2000Ecu constructor.
pub const KWP_CLASS_DEFAULTS: KTiming = KTiming {
    baud: consts::DEFAULT_BAUD,
    init: InitKind::KwpFiveBaud { multipoint: false },
    kw2_delay_ms: 25,
    first_request_ms: 20,
    inter_byte_ms: 0,
    reply_to_request_ms: 5,
    request_to_reply_timeout_ms: 300,
    tester_present_ms: 3000,
    on_error_ms: 300,
    kw2_timeout_ms: 300,
};

/// Timing for a `<K>` section.
pub fn k_timing(k: &KLine) -> Result<KTiming, SpecMapError> {
    let multipoint = k.kwp.as_ref().and_then(|w| w.multipoint).unwrap_or(false);
    let (init, iso8, mut t) = match k.protocol() {
        KProtocol::Iso8 => (InitKind::Iso8FiveBaud, k.iso8.as_ref(), ISO8_CLASS_DEFAULTS),
        KProtocol::KwpSlowInit => (
            InitKind::KwpFiveBaud { multipoint },
            k.kwp.as_ref().and_then(|w| w.iso8.as_ref()),
            KWP_CLASS_DEFAULTS,
        ),
        KProtocol::KwpFastInit => (InitKind::KwpFast { multipoint }, None, KWP_CLASS_DEFAULTS),
        KProtocol::Unknown => return Err(SpecMapError::UnknownProtocol),
    };
    t.init = init;
    let set = |dst: &mut u32, v: Option<u32>| {
        if let Some(v) = v {
            *dst = v;
        }
    };
    if let Some(i) = iso8 {
        set(&mut t.kw2_timeout_ms, i.kw2_timeout);
        set(&mut t.first_request_ms, i.first_request_delay);
    }
    if let Some(w) = &k.kwp {
        set(&mut t.kw2_delay_ms, w.kw2_delay);
    }
    set(&mut t.inter_byte_ms, k.inter_byte_delay);
    set(&mut t.reply_to_request_ms, k.reply_to_request_delay);
    set(
        &mut t.request_to_reply_timeout_ms,
        k.request_to_reply_timeout,
    );
    set(&mut t.tester_present_ms, k.tester_present_delay);
    set(&mut t.on_error_ms, k.on_error_delay);
    Ok(t)
}

/// Link config for a definition: address, timing, expected key bytes.
pub fn link_config(ecu: &EcuDef) -> Result<LinkConfig, SpecMapError> {
    let k = ecu.k.as_ref().ok_or(SpecMapError::NoKLine)?;
    let address = ecu
        .function
        .as_ref()
        .map(|f| f.address)
        .ok_or(SpecMapError::NoAddress)?;
    let mut cfg = LinkConfig::new(address, k_timing(k)?);
    cfg.expected_keywords = k.key_words();
    Ok(cfg)
}

/// Substrings (folded: lowercase, accents removed, only letters and digits) of a definition's
/// `Target@Name` or `<Function Name>` that mark it as a BMT/UCH/immobilizer decoder
/// (owner decision 2026-11-22: "any ECU whose loaded definition is a BMT/UCH/decoder"). The
/// Clio BMT definitions are named "BMT ..." with function "UCBIC/BFR".
pub const IMMOBILIZER_ECU_SUBSTRINGS: &[&str] = &[
    "bmt",
    "ucbic",
    "decodeur",
    "decoder",
    "antidemarr",
    "immobili",
    "wegfahr",
];
/// Folded substring of a Sirius injection definition name (e.g. "SIRIUS32 - EMS3132 ...").
pub const SIRIUS_ECU_SUBSTRING: &str = "sirius";
/// Whole words with the same meaning: `uch` (unite centrale habitacle), `immo`.
pub const IMMOBILIZER_ECU_WORDS: &[&str] = &["uch", "immo"];

fn folded_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut w = String::new();
    for c in s.chars().chain(core::iter::once(' ')) {
        match crate::safety::fold_char(c) {
            Some(f) => w.push(f),
            None if !w.is_empty() => words.push(core::mem::take(&mut w)),
            None => {}
        }
    }
    words
}

/// [`EcuClass::Immobilizer`] for a BMT/UCH/decoder definition, [`EcuClass::Sirius`] for a
/// Sirius definition, else [`EcuClass::Generic`].
pub fn ecu_class(ecu: &EcuDef) -> EcuClass {
    let names = [
        Some(ecu.name.as_str()),
        ecu.function.as_ref().map(|f| f.name.as_str()),
    ];
    let hit = names.iter().flatten().any(|n| {
        let words = folded_words(n);
        let compact: String = words.concat();
        IMMOBILIZER_ECU_SUBSTRINGS
            .iter()
            .any(|k| compact.contains(k))
            || words
                .iter()
                .any(|w| IMMOBILIZER_ECU_WORDS.contains(&w.as_str()))
    });
    let sirius = names
        .iter()
        .flatten()
        .any(|n| folded_words(n).concat().contains(SIRIUS_ECU_SUBSTRING));
    if hit {
        EcuClass::Immobilizer
    } else if sirius {
        EcuClass::Sirius
    } else {
        EcuClass::Generic
    }
}

/// The policy's view of `ecu` ([`DefinitionGuard`]): its class ([`ecu_class`]) and every
/// request whose request name, or the name, description, comment or mnemonic of one of its
/// data items, contains an immobilizer keyword ([`crate::safety::immobilizer_keyword`]),
/// flagged with its fixed sent bytes (up to the first sent data item). A sent item whose
/// `<List>` has a text with such a keyword flags the fixed bytes plus that value (when the item
/// is the whole byte right after them; otherwise the whole request). `Session` applies it to
/// every request once it is set with `Session::set_definition_guard`.
pub fn definition_guard(ecu: &EcuDef) -> DefinitionGuard {
    use crate::safety::immobilizer_keyword;
    let mut g = DefinitionGuard::new(ecu_class(ecu));
    let flagged = |name: &str| -> bool {
        if immobilizer_keyword(name).is_some() {
            return true;
        }
        ecu.data(name).is_some_and(|d| {
            [&d.description, &d.comment, &d.mnemonic]
                .iter()
                .any(|t| t.as_deref().and_then(immobilizer_keyword).is_some())
        })
    };
    for r in &ecu.requests {
        let sent = &r.sent_bytes.0;
        let fixed = r
            .sent_items
            .iter()
            .map(|i| (i.first_byte as usize).saturating_sub(1))
            .min()
            .unwrap_or(sent.len())
            .min(sent.len());
        let hit = flagged(&r.name)
            || r.sent_items
                .iter()
                .chain(r.received_items.iter())
                .any(|i| flagged(&i.name));
        if fixed > 0 && hit {
            g.flag(sent[..fixed].to_vec(), r.name.clone());
            continue;
        }
        // Enumerated sent parameters (step 7: BMT "sortie a activer" lists 0xA6 "Ligne
        // Transpondeur" and 0xAB "Led Verlog"): a list text with an immobilizer keyword flags
        // the fixed bytes plus that value when the item is the whole byte right after them,
        // else the whole request.
        for i in &r.sent_items {
            let Some(d) = ecu.data(&i.name) else {
                continue;
            };
            let values: Vec<i64> = d
                .list
                .iter()
                .filter(|l| immobilizer_keyword(&l.text).is_some())
                .map(|l| l.value)
                .collect();
            if values.is_empty() || fixed == 0 {
                continue;
            }
            let whole_byte = i.first_byte as usize == fixed + 1
                && i.bit_offset == 0
                && d.bits_count == 8
                && !d.is_bytes;
            if whole_byte {
                for v in values.into_iter().filter_map(|v| u8::try_from(v).ok()) {
                    let mut p = sent[..fixed].to_vec();
                    p.push(v);
                    g.flag(p, r.name.clone());
                }
            } else {
                g.flag(sent[..fixed].to_vec(), r.name.clone());
            }
        }
    }
    g
}

/// Names of the requests of `ecu` that `payload` would send (their fixed sent bytes are a
/// prefix of `payload`) and that [`definition_guard`] flags. Empty: no such request. The two
/// owner-approved BMT remote-learning routines (exact `31C7000000` / `31C8000000` on an
/// immobilizer ECU, [`crate::safety::is_immobilizer_ecu`]) are exempt: their BMT names contain
/// "CLE" (remote key).
pub fn immobilizer_named_requests(
    ecu: &EcuDef,
    target: &crate::safety::Target,
    payload: &[u8],
) -> Vec<String> {
    definition_guard(ecu).immobilizer_names(target, payload)
}
