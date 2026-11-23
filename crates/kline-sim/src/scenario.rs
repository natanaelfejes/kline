//! Scenario values and the spec-driven positive response builder.

use std::collections::BTreeMap;

use kline_spec::{EcuDef, Request, Value};

use crate::SimError;

/// The values the simulated ECU reports, per `<Data>` name (a `<DataItem>` names its Data).
/// Items without a value are sent as raw 0.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scenario {
    values: BTreeMap<String, Value>,
}

impl Scenario {
    pub fn new() -> Scenario {
        Scenario::default()
    }

    pub fn set(mut self, data: &str, value: Value) -> Scenario {
        self.values.insert(data.to_string(), value);
        self
    }

    pub fn number(self, data: &str, v: f64) -> Scenario {
        self.set(data, Value::Number(v))
    }

    pub fn text(self, data: &str, t: &str) -> Scenario {
        self.set(data, Value::Text(t.to_string()))
    }

    pub fn get(&self, data: &str) -> Option<&Value> {
        self.values.get(data)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.values.iter()
    }

    /// Every name must be a `<Data>` of `ecu` and every value must encode exactly
    /// (`DataDef::encode_exact`: off-grid numbers are refused, not rounded).
    pub fn check(&self, ecu: &EcuDef) -> Result<(), SimError> {
        for (name, v) in &self.values {
            let d = ecu
                .data(name)
                .ok_or_else(|| SimError::UnknownData(name.clone()))?;
            d.encode_exact(v).map_err(|e| SimError::Encode {
                data: name.clone(),
                source: e,
            })?;
        }
        Ok(())
    }
}

/// End (exclusive, 0-based) of every received item of `req`.
fn items_end(ecu: &EcuDef, req: &Request) -> Result<usize, SimError> {
    let mut end = 0;
    for item in &req.received_items {
        let d = ecu
            .data(&item.name)
            .ok_or_else(|| SimError::UnknownData(item.name.clone()))?;
        let span = (d.bits_count as usize + item.bit_offset as usize).div_ceil(8);
        end = end.max((item.first_byte as usize).saturating_sub(1) + span);
    }
    Ok(end)
}

/// Positive response payload (starting at the response SID) for `sent`, answering every
/// request in `reqs` at once (several definitions can share the same bytes, e.g. BMT `2180`).
///
/// - byte 1: `sent[0] + 0x40`;
/// - requests with received items: bytes 2 .. (first item - 1) echo `sent` (as far as it
///   goes), the rest is 0 up to `max(MinBytes, end of the last item)`, then every item with a
///   scenario value is inserted with `encode_exact` + `insert`;
/// - SID `21`: byte 2 is always `sent[1]` (the local-identifier echo), also over an item;
/// - requests without received items: `sent[1..]` echoed, padded with 0 to MinBytes
///   (assumption, NOT VERIFIED: e.g. `10 A0` -> `50 A0`).
pub fn positive_response(
    ecu: &EcuDef,
    reqs: &[&Request],
    sent: &[u8],
    scenario: &Scenario,
) -> Result<Vec<u8>, SimError> {
    positive_response_with(ecu, reqs, sent, scenario, 0x40)
}

/// [`positive_response`] with the response SID `sent[0] + sid_offset` (0 for the Bosch 5.3
/// ABS, which answers `51` with `51 ..`: car capture 2026-11-23).
pub fn positive_response_with(
    ecu: &EcuDef,
    reqs: &[&Request],
    sent: &[u8],
    scenario: &Scenario,
    sid_offset: u8,
) -> Result<Vec<u8>, SimError> {
    let sid = *sent.first().ok_or(SimError::EmptyRequest)?;
    let rsid = sid
        .checked_add(sid_offset)
        .ok_or(SimError::NoPositiveSid(sid))?;
    let mut len = 1;
    let mut echo_end = sent.len();
    for r in reqs {
        len = len.max(r.min_bytes as usize).max(items_end(ecu, r)?);
        if let Some(first) = r.received_items.iter().map(|i| i.first_byte).min() {
            echo_end = echo_end.min((first as usize).saturating_sub(1));
        }
    }
    let echo_end = echo_end.max(1);
    len = len.max(echo_end);
    let mut frame = vec![0u8; len];
    frame[0] = rsid;
    frame[1..echo_end].copy_from_slice(&sent[1..echo_end]);
    for r in reqs {
        for item in &r.received_items {
            let Some(v) = scenario.get(&item.name) else {
                continue;
            };
            let d = ecu
                .data(&item.name)
                .ok_or_else(|| SimError::UnknownData(item.name.clone()))?;
            let enc = |e| SimError::Encode {
                data: item.name.clone(),
                source: e,
            };
            let raw = d.encode_exact(v).map_err(enc)?;
            d.insert(&raw, &mut frame, item, ecu.endian).map_err(enc)?;
        }
    }
    // KWP ReadDataByLocalIdentifier always echoes the identifier at byte 2, even when the
    // definition maps byte 2 as an item (Sirius 32 `21 81` LocalIvinOK); kline-spec checks it
    // (docs/spec-semantics.md, response validation step 5).
    if sid == 0x21 && sent.len() >= 2 && frame.len() >= 2 {
        frame[1] = sent[1];
    }
    Ok(frame)
}

/// Does `payload` address `req`? Bytes covered by a sent data item may differ; all other bytes
/// of `SentBytes` must match, and the length must match (or be at least as long with
/// `<VariableLength/>`).
pub fn request_matches(ecu: &EcuDef, req: &Request, payload: &[u8]) -> bool {
    let sent = &req.sent_bytes.0;
    if sent.is_empty() {
        return false;
    }
    let len_ok = if req.variable_length {
        payload.len() >= sent.len()
    } else {
        payload.len() == sent.len()
    };
    if !len_ok {
        return false;
    }
    let mut free = vec![false; sent.len()];
    for item in &req.sent_items {
        let bits = ecu.data(&item.name).map_or(8, |d| d.bits_count as usize);
        let span = (bits + item.bit_offset as usize).div_ceil(8);
        let start = (item.first_byte as usize).saturating_sub(1);
        for f in free.iter_mut().skip(start).take(span) {
            *f = true;
        }
    }
    sent.iter()
        .zip(payload)
        .zip(&free)
        .all(|((s, p), &f)| f || s == p)
}
