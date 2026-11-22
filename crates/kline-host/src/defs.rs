//! Which ECU definition(s) a command uses, and what the link and the policy need from them.
//!
//! `--ecu sirius|bmt` are shortcuts for the car's files in the definitions directory
//! (`--defs`, `$KLINE_DDT_XML` (deprecated fallback `$CLIO_DDT_XML`), default `~/.local/share/kline/ecus`). `--xml` takes any
//! DDT2000 ECU definition (repeatable: candidates for `ident`); `--address` overrides its
//! `<Function Address>`, `--vehicle` names a vehicle addressing file (e.g. `x65_addressing.xml`)
//! for the function name, its protocols and the tool address.

use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use kline_core::safety::{self, Protocol, Target, Tier};
use kline_core::LinkConfig;
use kline_spec::{Addressing, DecodedItem, EcuDef, KProtocol, Raw, Request, SpecError, Value};
use serde_json::{json, Value as Json};

use crate::error::CliError;

/// The car's ECUs (docs/protocol.md "The car's ECUs").
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EcuName {
    /// Injection, Siemens Sirius 32 at 0x7A.
    Sirius,
    /// Body / UCH (BMT) at 0x26.
    Bmt,
}

impl EcuName {
    pub fn label(self) -> &'static str {
        match self {
            EcuName::Sirius => "sirius",
            EcuName::Bmt => "bmt",
        }
    }

    /// Definition files in the definitions directory, preferred first. The BMT has two
    /// definitions whose config indices differ; `ident` chooses between them.
    pub fn files(self) -> &'static [&'static str] {
        match self {
            EcuName::Sirius => &["SIRIUS32___EMS3132___Soft_RE1_maj.xml"],
            EcuName::Bmt => &["BMT_X65_ph1_p.xml", "BMT_SAGEM.xml"],
        }
    }
}

#[derive(Debug, Clone, Default, Args)]
pub struct EcuArgs {
    /// The car's ECU (shortcut for its definition in --defs).
    #[arg(long, value_enum)]
    pub ecu: Option<EcuName>,
    /// Any DDT2000 ECU definition XML (repeat for ident candidates; the first sets up the link).
    #[arg(long = "xml", value_name = "PATH")]
    pub xml: Vec<PathBuf>,
    /// Vehicle addressing file (e.g. vehicles/x65_addressing.xml).
    #[arg(long, value_name = "PATH")]
    pub vehicle: Option<PathBuf>,
    /// ECU address, overriding the definition's <Function Address> (e.g. 0x7A).
    #[arg(long, value_parser = parse_u8)]
    pub address: Option<u8>,
}

impl EcuArgs {
    /// The arguments again, as text for an exact command line.
    pub fn to_cli(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(e) = self.ecu {
            v.extend(["--ecu".into(), e.label().into()]);
        }
        for x in &self.xml {
            v.extend(["--xml".into(), x.display().to_string()]);
        }
        if let Some(p) = &self.vehicle {
            v.extend(["--vehicle".into(), p.display().to_string()]);
        }
        if let Some(a) = self.address {
            v.extend(["--address".into(), format!("0x{a:02X}")]);
        }
        v
    }
}

pub fn parse_u8(s: &str) -> Result<u8, String> {
    let t = s.trim();
    let r = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u8::from_str_radix(h, 16)
    } else {
        t.parse()
    };
    r.map_err(|_| format!("not a byte: {s:?} (use 0x7A or 122)"))
}

/// A loaded definition.
#[derive(Debug, Clone)]
pub struct LoadedDef {
    pub path: PathBuf,
    pub def: EcuDef,
}

/// The ECU a command talks to.
#[derive(Debug, Clone)]
pub struct Ecu {
    /// `sirius`, `bmt`, or the first definition's file stem.
    pub label: String,
    /// Candidates, the first sets up the link.
    pub defs: Vec<LoadedDef>,
    pub address_override: Option<u8>,
    pub vehicle: Option<(PathBuf, Addressing)>,
}

pub fn default_defs_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("KLINE_DDT_XML") {
        return PathBuf::from(p);
    }
    // Deprecated fallback, read for one release: the variable was called CLIO_DDT_XML.
    if let Some(p) = std::env::var_os("CLIO_DDT_XML") {
        eprintln!("warning: CLIO_DDT_XML is deprecated, use KLINE_DDT_XML");
        return PathBuf::from(p);
    }
    home().join(".local/share/kline/ecus")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn load(path: &Path) -> Result<EcuDef, CliError> {
    let bytes = std::fs::read(path).map_err(|e| {
        CliError::definition(format!("cannot read definition {}: {e}", path.display()))
    })?;
    EcuDef::from_xml_bytes(&bytes)
        .map_err(|e| CliError::definition(format!("invalid definition {}: {e}", path.display())))
}

impl Ecu {
    pub fn resolve(a: &EcuArgs, defs_dir: &Path) -> Result<Ecu, CliError> {
        let paths: Vec<PathBuf> = if !a.xml.is_empty() {
            a.xml.clone()
        } else if let Some(e) = a.ecu {
            let files = e.files();
            let mut v: Vec<PathBuf> = files.iter().map(|f| defs_dir.join(f)).collect();
            // Only the preferred file is required; the alternatives are ident candidates.
            let first = v.remove(0);
            v.retain(|p| p.exists());
            v.insert(0, first);
            v
        } else {
            return Err(CliError::usage(
                "name the ECU: --ecu sirius|bmt or --xml <file>",
            ));
        };
        let mut defs = Vec::new();
        for p in paths {
            let def = load(&p)?;
            defs.push(LoadedDef { path: p, def });
        }
        let label = match a.ecu {
            Some(e) => e.label().to_string(),
            None => defs[0]
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "ecu".into()),
        };
        let vehicle = match &a.vehicle {
            Some(p) => {
                let b = std::fs::read(p).map_err(|e| {
                    CliError::definition(format!("cannot read vehicle file {}: {e}", p.display()))
                })?;
                let v = Addressing::from_xml_bytes(&b).map_err(|e| {
                    CliError::definition(format!("invalid vehicle file {}: {e}", p.display()))
                })?;
                Some((p.clone(), v))
            }
            None => None,
        };
        let ecu = Ecu {
            label,
            defs,
            address_override: a.address,
            vehicle,
        };
        ecu.check_kline()?;
        Ok(ecu)
    }

    pub fn primary(&self) -> &LoadedDef {
        &self.defs[0]
    }

    pub fn def(&self) -> &EcuDef {
        &self.defs[0].def
    }

    /// CAN-only definitions cannot be used with a KKL cable.
    fn check_kline(&self) -> Result<(), CliError> {
        for d in &self.defs {
            match (&d.def.k, &d.def.can) {
                (Some(k), _) if k.protocol() != KProtocol::Unknown => {}
                (None, Some(_)) => {
                    return Err(CliError::definition(format!(
                        "{} is a CAN definition: CAN not supported by a KKL cable yet",
                        d.path.display()
                    )))
                }
                _ => {
                    return Err(CliError::definition(format!(
                        "{} has no usable K-line <K> section (neither ISO8 nor KWP init)",
                        d.path.display()
                    )))
                }
            }
        }
        Ok(())
    }

    pub fn address(&self) -> Result<u8, CliError> {
        if let Some(a) = self.address_override {
            return Ok(a);
        }
        self.def()
            .function
            .as_ref()
            .map(|f| f.address)
            .ok_or_else(|| {
                CliError::definition(format!(
                    "{} has no <Function Address>; pass --address 0xNN",
                    self.primary().path.display()
                ))
            })
    }

    pub fn protocol(&self) -> Protocol {
        match self.def().k.as_ref().map(|k| k.protocol()) {
            Some(KProtocol::Iso8) => Protocol::Iso8,
            _ => Protocol::Kwp,
        }
    }

    /// The policy's view of the definition ([`kline_core::safety::DefinitionGuard`]): its
    /// BMT/UCH/decoder class and its immobilizer-named requests.
    pub fn guard(&self) -> kline_core::safety::DefinitionGuard {
        kline_core::spec::definition_guard(self.def())
    }

    /// The policy target the init would produce (address from the definition or --address).
    pub fn target(&self) -> Result<Target, CliError> {
        Ok(Target {
            protocol: self.protocol(),
            ecu_address: self.address()?,
        })
    }

    /// Link config from the definition's `<K>` (kline-core mapping), with --address and the
    /// vehicle file's tool address applied.
    pub fn link_config(&self) -> Result<LinkConfig, CliError> {
        let def = self.def();
        let k = def
            .k
            .as_ref()
            .ok_or_else(|| CliError::definition("no <K> section"))?;
        let t =
            kline_core::spec::k_timing(k).map_err(|e| CliError::definition(format!("<K>: {e}")))?;
        let mut cfg = LinkConfig::new(self.address()?, t);
        cfg.expected_keywords = k.key_words();
        if let Some(tool) = self.vehicle.as_ref().and_then(|v| v.1.tool_address) {
            cfg.tool_address = tool;
        }
        Ok(cfg)
    }

    /// What the vehicle file says about the address, for the connect report.
    pub fn vehicle_info(&self) -> Json {
        let Some((path, v)) = &self.vehicle else {
            return Json::Null;
        };
        let addr = self.address().ok();
        match addr.and_then(|a| v.function(a)) {
            Some(f) => json!({
                "file": path.display().to_string(),
                "function": f.name,
                "protocols": f.protocols.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
                "iso8_address": f.iso8_address.map(|a| format!("{a:02X}")),
            }),
            None => json!({
                "file": path.display().to_string(),
                "function": null,
                "warning": "address not listed in the vehicle file",
            }),
        }
    }
}

/// A request to send: its payload and the definition's requests with exactly these bytes.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub payload: Vec<u8>,
    pub names: Vec<String>,
}

/// One `--set ITEM=VALUE`: a number, `hex:ABCD` (raw bytes), or a list label / ASCII text.
pub fn parse_set(s: &str) -> Result<(String, kline_spec::Value), CliError> {
    let (name, v) = s
        .split_once('=')
        .ok_or_else(|| CliError::usage(format!("--set {s:?}: expected ITEM=VALUE")))?;
    let (name, v) = (name.trim(), v.trim());
    if name.is_empty() {
        return Err(CliError::usage(format!("--set {s:?}: empty item name")));
    }
    let value = if let Some(h) = v.strip_prefix("hex:") {
        kline_spec::Value::Hex(h.to_string())
    } else if let Ok(x) = v.parse::<f64>() {
        kline_spec::Value::Number(x)
    } else {
        kline_spec::Value::Text(v.to_string())
    };
    Ok((name.to_string(), value))
}

/// Like [`resolve_request`], with `sets` (`ITEM=VALUE`) encoded into the request's `<Sent>`
/// data items by the definition (`Request::build`, DDT2000's request parameters). A number
/// must lie on the item's scaling grid (`encode_exact`). With `sets`, `spec` must be a request
/// name; items not set keep the bytes of `SentBytes`.
pub fn resolve_request_with(
    def: &EcuDef,
    spec: &str,
    sets: &[String],
) -> Result<Resolved, CliError> {
    if sets.is_empty() {
        return resolve_request(def, spec);
    }
    let req = def.request(spec).ok_or_else(|| {
        CliError::usage(format!(
            "--set needs a request name of {}; {spec:?} is not one",
            def.name
        ))
    })?;
    let inputs: Vec<(String, kline_spec::Value)> = sets
        .iter()
        .map(|s| parse_set(s))
        .collect::<Result<_, _>>()?;
    let items: Vec<&str> = req.sent_items.iter().map(|i| i.name.as_str()).collect();
    for (name, value) in &inputs {
        if !items.contains(&name.as_str()) {
            return Err(CliError::definition(format!(
                "{:?} has no sent item {name:?} (it has: {})",
                req.name,
                if items.is_empty() {
                    "none".to_string()
                } else {
                    items.join(", ")
                }
            )));
        }
        let data = def
            .data(name)
            .ok_or_else(|| CliError::definition(format!("no data definition {name:?}")))?;
        data.encode_exact(value)
            .map_err(|e| CliError::definition(format!("--set {name}: {e}")))?;
    }
    let refs: Vec<(&str, kline_spec::Value)> = inputs
        .iter()
        .map(|(n, v)| (n.as_str(), v.clone()))
        .collect();
    let payload = req
        .build(def, &refs)
        .map_err(|e| CliError::definition(format!("{}: {e}", req.name)))?;
    let mut names = vec![req.name.clone()];
    for r in requests_for(def, &payload) {
        if !names.contains(&r.name) {
            names.push(r.name.clone());
        }
    }
    Ok(Resolved { payload, names })
}

/// `spec` is a request name of `def` (exact, then case-insensitive) or hex bytes.
pub fn resolve_request(def: &EcuDef, spec: &str) -> Result<Resolved, CliError> {
    let payload = match def.request(spec) {
        Some(r) => r.sent_bytes.0.clone(),
        None => crate::parse_hex(spec).ok_or_else(|| {
            CliError::definition(format!(
                "{spec:?} is neither a request name of {} nor hex bytes",
                def.name
            ))
        })?,
    };
    if payload.is_empty() {
        return Err(CliError::usage("empty request"));
    }
    let names = requests_for(def, &payload)
        .iter()
        .map(|r| r.name.clone())
        .collect();
    Ok(Resolved { payload, names })
}

/// Requests of `def` whose `SentBytes` are exactly `payload`.
pub fn requests_for<'a>(def: &'a EcuDef, payload: &[u8]) -> Vec<&'a Request> {
    def.requests
        .iter()
        .filter(|r| r.sent_bytes.0 == payload)
        .collect()
}

/// Every request named for `payload`, decoded against `reply` (checked).
pub fn decode_reply(
    def: &EcuDef,
    payload: &[u8],
    reply: &[u8],
) -> Vec<(String, Result<Vec<DecodedItem>, SpecError>)> {
    requests_for(def, payload)
        .into_iter()
        .map(|r| {
            (
                r.name.clone(),
                r.decode_checked_against(def, payload, reply),
            )
        })
        .collect()
}

pub fn item_json(i: &DecodedItem) -> Json {
    json!({
        "name": i.name,
        "display": i.display,
        "unit": i.unit,
        "value": i.value.as_ref().map(value_json),
        "raw": i.raw.as_ref().map(|r| match r {
            Raw::Int(v) => json!(v),
            Raw::Bytes(b) => json!(b.to_hex()),
        }),
    })
}

fn value_json(v: &Value) -> Json {
    match v {
        Value::Number(n) => json!(n),
        Value::Text(t) | Value::Hex(t) => json!(t),
    }
}

pub fn decoded_json(decoded: &[(String, Result<Vec<DecodedItem>, SpecError>)]) -> Json {
    Json::Array(
        decoded
            .iter()
            .map(|(name, r)| match r {
                Ok(items) => json!({"request": name, "items": items.iter().map(item_json).collect::<Vec<_>>()}),
                Err(e) => json!({"request": name, "error": e.to_string()}),
            })
            .collect(),
    )
}

pub fn decoded_human(decoded: &[(String, Result<Vec<DecodedItem>, SpecError>)]) -> String {
    let mut s = String::new();
    for (name, r) in decoded {
        match r {
            Ok(items) => {
                s.push_str(&format!("  [{name}]\n"));
                let w = items.iter().map(|i| i.name.len()).max().unwrap_or(0);
                for i in items {
                    s.push_str(&format!(
                        "    {:<w$}  {} {}\n",
                        i.name,
                        i.display.as_deref().unwrap_or("-"),
                        i.unit.as_deref().unwrap_or("")
                    ));
                }
            }
            Err(e) => s.push_str(&format!("  [{name}] does not decode: {e}\n")),
        }
    }
    s.trim_end().to_string()
}

/// Identification requests: `21 80` on KWP, else Tier 0 requests named like "ident".
pub fn ident_requests<'a>(def: &'a EcuDef, target: &Target) -> Vec<&'a Request> {
    let by_bytes: Vec<&Request> = def
        .requests
        .iter()
        .filter(|r| r.sent_bytes.0 == safety::IDENTIFICATION_READ)
        .collect();
    if target.protocol == Protocol::Kwp && !by_bytes.is_empty() {
        return by_bytes;
    }
    def.requests
        .iter()
        .filter(|r| {
            r.name.to_lowercase().contains("ident")
                && safety::classify(target, &r.sent_bytes.0) == Ok(Tier::Read)
        })
        .collect()
}

/// Fault-flag reads: Tier 0 requests whose name says faults (French `panne`/`defaut`).
pub fn dtc_requests<'a>(def: &'a EcuDef, target: &Target) -> Vec<&'a Request> {
    def.requests
        .iter()
        .filter(|r| {
            is_fault_name(&r.name) && safety::classify(target, &r.sent_bytes.0) == Ok(Tier::Read)
        })
        .collect()
}

/// Whether a request or item name says "fault" (French `panne`/`defaut`).
pub fn is_fault_name(name: &str) -> bool {
    let n = fold(name);
    ["panne", "defaut", "fault", "dtc"]
        .iter()
        .any(|k| n.contains(k))
}

/// Tier 0 reads carrying at least one fault-named received item (ISO8 definitions such as
/// Safir declare `Failures Id="0"` and no fault request, but their `33 51`/`33 52` frames hold
/// "Panne presente ..." flags).
pub fn fault_item_requests<'a>(def: &'a EcuDef, target: &Target) -> Vec<&'a Request> {
    def.requests
        .iter()
        .filter(|r| {
            r.received_items.iter().any(|i| is_fault_name(&i.name))
                && safety::classify(target, &r.sent_bytes.0) == Ok(Tier::Read)
        })
        .collect()
}

fn fold(s: &str) -> String {
    s.to_lowercase()
        .replace(['é', 'è', 'ê'], "e")
        .replace("Ã©", "e")
}

/// The definition the identification reply fits: the first candidate (same address, matching
/// key bytes when it declares them) whose identification requests all decode with the checked
/// decoder. `ambiguous` when more than one fits. `ident` uses this only when DDT2000's AutoIdent
/// ladder ([`kline_spec::best_match`]) does not apply (no `<AutoIdents>`, or a reply without
/// the Renault `21 80` layout); Tier 2 writes use it to check the chosen definition.
pub struct IdentChoice {
    pub chosen: Option<usize>,
    pub ambiguous: bool,
    pub results: Vec<Json>,
}

pub fn choose_by_ident(ecu: &Ecu, keywords: (u8, u8), sent: &[u8], reply: &[u8]) -> IdentChoice {
    let mut fits = Vec::new();
    let mut results = Vec::new();
    for (i, d) in ecu.defs.iter().enumerate() {
        let kw = d.def.k.as_ref().and_then(|k| k.key_words());
        let kw_ok = kw.is_none_or(|k| k == keywords);
        let decoded = decode_reply(&d.def, sent, reply);
        let ok = kw_ok && !decoded.is_empty() && decoded.iter().all(|(_, r)| r.is_ok());
        if ok {
            fits.push(i);
        }
        results.push(json!({
            "definition": d.path.display().to_string(),
            "name": d.def.name,
            "keywords_match": kw.map(|k| k == keywords),
            "fits": ok,
            "decoded": decoded_json(&decoded),
        }));
    }
    IdentChoice {
        chosen: fits.first().copied(),
        ambiguous: fits.len() > 1,
        results,
    }
}
