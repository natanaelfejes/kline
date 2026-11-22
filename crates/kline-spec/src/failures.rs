//! Faults the way DDT2000's DiagFailures does it for `<Failures Id="1">` (failure flags), and
//! the choice of the clear request. See the author's DDT2000 behaviour notes "Failures".

use serde::Serialize;

use crate::codec::DecodedItem;
use crate::model::{EcuDef, Request};

/// The raw `14 FF 00` DDT2000's global clear sends when the definition has no request named
/// `ClearDiagnosticInformation.*` (RE: DiagFailures 0x24054d70, literal `"14FF00"`).
pub const DDT2000_CLEAR_FALLBACK: [u8; 3] = [0x14, 0xFF, 0x00];

/// Request names DDT2000 tries, in order, before the prefix search (RE: DiagFailures
/// 0x24054d70).
pub const DDT2000_CLEAR_NAMES: [&str; 2] = [
    "ClearDiagnosticInformation.Manual",
    "ClearDiagnosticInformation.All",
];

/// Prefix of the third step of DDT2000's clear-request search.
pub const DDT2000_CLEAR_PREFIX: &str = "ClearDiagnosticInformation.";

/// Where a clear payload comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClearSource {
    /// A definition request DDT2000 would also pick by name.
    NamedRequest { name: String },
    /// The definition's own clear (first request whose SentBytes start with `14`), which
    /// DDT2000's global clear does not look for. Ours.
    DefinitionRequest { name: String },
    /// DDT2000's built-in `14 FF 00`.
    Ddt2000Fallback,
}

/// One clear choice: the exact bytes and where they come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClearChoice {
    #[serde(serialize_with = "crate::model::ser_hex")]
    pub payload: Vec<u8>,
    pub source: ClearSource,
}

/// What kline would send to clear faults, and what DDT2000's global clear would send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClearPlan {
    /// Ours (default): DDT2000's named requests if present, else the definition's own `14 ..`
    /// request, else `14 FF 00`.
    pub ours: ClearChoice,
    /// DDT2000 DiagFailures: `.Manual`, `.All`, the first `ClearDiagnosticInformation.*`
    /// (DDT2000 sends every one that matches; we report the first), else `14 FF 00`.
    pub ddt2000: ClearChoice,
}

impl ClearPlan {
    /// The two choices send different bytes (Sirius 32: ours `14 00 00` "Reinit flags de
    /// pannes", DDT2000 `14 FF 00`).
    pub fn differs(&self) -> bool {
        self.ours.payload != self.ddt2000.payload
    }
}

/// A set fault flag, with the devices that list it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActiveFault {
    pub request: String,
    pub item: String,
    pub devices: Vec<String>,
    pub display: Option<String>,
    pub raw: Option<u64>,
}

impl EcuDef {
    /// Names of the data items marked `FailureFlag="1"` in `<Devices>`.
    pub fn failure_flag_items(&self) -> Vec<&str> {
        let mut v: Vec<&str> = Vec::new();
        for d in &self.devices {
            for dd in &d.data {
                if dd.failure_flag && !v.contains(&dd.name.as_str()) {
                    v.push(&dd.name);
                }
            }
        }
        v
    }

    /// For `<Failures Id="1">`: the requests whose replies carry failure-flag items, in
    /// document order (DiagFailures "Reading flags and infos"). Empty for other failure models.
    pub fn failure_flag_requests(&self) -> Vec<&Request> {
        if !self.failures.as_ref().is_some_and(|f| f.is_flags()) {
            return Vec::new();
        }
        let flags = self.failure_flag_items();
        self.requests
            .iter()
            .filter(|r| {
                r.received_items
                    .iter()
                    .any(|i| flags.contains(&i.name.as_str()))
            })
            .collect()
    }

    /// The set flags among `items` (decoded from `request`): failure-flag items whose raw value
    /// is non-zero. The non-zero rule is inferred (DiagFailures' device display logic is not
    /// decoded, the author's DDT2000 behaviour notes).
    pub fn active_faults(&self, request: &str, items: &[DecodedItem]) -> Vec<ActiveFault> {
        let flags = self.failure_flag_items();
        items
            .iter()
            .filter(|i| flags.contains(&i.name.as_str()))
            .filter(|i| i.raw_int().is_some_and(|v| v != 0))
            .map(|i| ActiveFault {
                request: request.to_string(),
                item: i.name.clone(),
                devices: self
                    .devices
                    .iter()
                    .filter(|d| d.data.iter().any(|dd| dd.failure_flag && dd.name == i.name))
                    .map(|d| d.name.clone())
                    .collect(),
                display: i.display.clone(),
                raw: i.raw_int(),
            })
            .collect()
    }

    fn ddt2000_named_clear(&self) -> Option<&Request> {
        DDT2000_CLEAR_NAMES
            .iter()
            .find_map(|n| self.requests.iter().find(|r| r.name == *n))
            .or_else(|| {
                self.requests
                    .iter()
                    .find(|r| r.name.starts_with(DDT2000_CLEAR_PREFIX))
            })
    }

    /// The clear request choice. Sending it is Tier 1 (kline-core) whichever is chosen.
    pub fn clear_plan(&self) -> ClearPlan {
        let named = self.ddt2000_named_clear().map(|r| ClearChoice {
            payload: r.sent_bytes.0.clone(),
            source: ClearSource::NamedRequest {
                name: r.name.clone(),
            },
        });
        let fallback = ClearChoice {
            payload: DDT2000_CLEAR_FALLBACK.to_vec(),
            source: ClearSource::Ddt2000Fallback,
        };
        let own = self
            .requests
            .iter()
            .find(|r| r.sent_bytes.0.first() == Some(&0x14))
            .map(|r| ClearChoice {
                payload: r.sent_bytes.0.clone(),
                source: ClearSource::DefinitionRequest {
                    name: r.name.clone(),
                },
            });
        ClearPlan {
            ours: named.clone().or(own).unwrap_or_else(|| fallback.clone()),
            ddt2000: named.unwrap_or(fallback),
        }
    }
}
