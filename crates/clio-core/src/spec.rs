//! `EcuDef` / `<K>` -> [`LinkConfig`] mapping (feature `spec`, std only).
//!
//! Missing XML values fall back to the defaults of the DDT2000 protocol class that would read
//! the file (RE: KWP2000Diag4 ISO8Ecu / KWP2000Ecu constructors). These are the monopoint
//! values; the fixed Multipoint values are applied by `LinkConfig::effective_timing` only under
//! `InitVariant::MultiEcu`. The baud rate is always
//! [`consts::DEFAULT_BAUD`]; the XML value is in `KLine::baud_rate` if a caller wants it.

use clio_spec::{EcuDef, KLine, KProtocol};
use thiserror::Error;

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

/// Names of the requests of `ecu` that `payload` would send (its `SentBytes` up to the first
/// sent data item are a prefix of `payload`) and whose request name, or the name, description
/// or mnemonic of one of its data items, contains an immobilizer keyword
/// ([`crate::safety::immobilizer_keyword`]). Empty: no such request.
///
/// The two owner-approved BMT remote-learning routines (exact `31C7000000` / `31C8000000` on
/// [`crate::safety::BMT_ADDRESS`]) are exempt: their BMT names contain "CLE" (remote key).
/// Additive guard for generic ECUs (coordinator scope addition 2026-11-22); the address-based
/// policy in [`crate::safety`] still applies first.
pub fn immobilizer_named_requests(
    ecu: &EcuDef,
    target: &crate::safety::Target,
    payload: &[u8],
) -> Vec<String> {
    use crate::safety::{immobilizer_keyword, BMT_ADDRESS, BMT_REMOTE_LEARNING};
    if target.ecu_address == BMT_ADDRESS && BMT_REMOTE_LEARNING.iter().any(|r| r[..] == *payload) {
        return Vec::new();
    }
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
    ecu.requests
        .iter()
        .filter(|r| {
            let sent = &r.sent_bytes.0;
            let fixed = r
                .sent_items
                .iter()
                .map(|i| (i.first_byte as usize).saturating_sub(1))
                .min()
                .unwrap_or(sent.len())
                .min(sent.len());
            fixed > 0 && payload.starts_with(&sent[..fixed])
        })
        .filter(|r| {
            flagged(&r.name)
                || r.sent_items
                    .iter()
                    .chain(r.received_items.iter())
                    .any(|i| flagged(&i.name))
        })
        .map(|r| r.name.clone())
        .collect()
}
