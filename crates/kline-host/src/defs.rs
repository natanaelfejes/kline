//! Which ECU definition(s) a command uses, and what the link and the policy need from them.
//!
//! `--ecu sirius|bmt` are shortcuts for the car's files in the definitions directory
//! (`--defs`, `$KLINE_DDT_XML` (deprecated fallback `$CLIO_DDT_XML`), default `~/.local/share/kline/ecus`, `%APPDATA%\kline\ecus` on Windows). `--xml` takes any
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
    /// Body / UCH (BMT) at 0x26: `BMT_X65_ph1_p.xml`, with `BMT_SAGEM.xml` as a second
    /// candidate when present (definition choice unresolved until named).
    Bmt,
    /// BMT with `BMT_X65_ph1_p.xml` only (the definition choice made by the owner).
    #[value(name = "bmt-ph1p")]
    BmtPh1p,
    /// BMT with `BMT_SAGEM.xml` only.
    #[value(name = "bmt-sagem")]
    BmtSagem,
}

impl EcuName {
    pub fn label(self) -> &'static str {
        match self {
            EcuName::Sirius => "sirius",
            EcuName::Bmt => "bmt",
            EcuName::BmtPh1p => "bmt-ph1p",
            EcuName::BmtSagem => "bmt-sagem",
        }
    }

    /// Definition files in the definitions directory, preferred first. The BMT has two
    /// definitions whose config indices differ; `ident` chooses between them.
    pub fn files(self) -> &'static [&'static str] {
        match self {
            EcuName::Sirius => &["SIRIUS32___EMS3132___Soft_RE1_maj.xml"],
            EcuName::Bmt => &["BMT_X65_ph1_p.xml", "BMT_SAGEM.xml"],
            EcuName::BmtPh1p => &["BMT_X65_ph1_p.xml"],
            EcuName::BmtSagem => &["BMT_SAGEM.xml"],
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
    data_dir().join("ecus")
}

/// kline's data directory (ECU definitions `ecus/`, `captures/`), docs/platforms.md:
/// `~/.local/share/kline` on Linux and macOS (the historical default, kept on macOS too so
/// existing setups keep working), `%APPDATA%\kline` on Windows.
pub fn data_dir() -> PathBuf {
    data_dir_from(cfg!(windows), |k| std::env::var_os(k))
}

/// [`data_dir`] for a given OS family and environment (tests).
pub fn data_dir_from(windows: bool, env: impl Fn(&str) -> Option<std::ffi::OsString>) -> PathBuf {
    let nonempty = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if windows {
        if let Some(a) = nonempty("APPDATA") {
            return a.join("kline");
        }
        if let Some(u) = nonempty("USERPROFILE") {
            return u.join("AppData").join("Roaming").join("kline");
        }
        return PathBuf::from("kline-data");
    }
    nonempty("HOME")
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/share/kline")
}

/// Exit 8 for `--ecu` when its definition file is not in the definitions directory: what is
/// missing and the three ways to fix it.
pub fn missing_definition(dir: &Path, file: &str) -> CliError {
    let state = if dir.is_dir() {
        format!("{} has no {file}", dir.display())
    } else {
        format!("{} does not exist", dir.display())
    };
    CliError::definition(format!(
        "definition {file} not found: {state}. Fix: copy the DDT2000 XML files there once with `kline setup --defs-from DIR`, or name their folder with --defs DIR or KLINE_DDT_XML=DIR (default definitions directory: {})",
        data_dir().join("ecus").display()
    ))
}

/// The car's definition files expected by `--ecu`, for `setup`'s report.
pub fn car_files() -> Vec<(&'static str, &'static str)> {
    EcuName::value_variants()
        .iter()
        .map(|e| (e.label(), e.files()[0]))
        .collect()
}

/// The git work tree `dir` (or its nearest existing ancestor) lies in, if any.
pub fn git_work_tree(dir: &Path) -> Option<PathBuf> {
    let abs = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(dir)
    };
    abs.ancestors()
        .find(|a| a.join(".git").exists())
        .map(Path::to_path_buf)
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
            if !v[0].exists() {
                return Err(missing_definition(defs_dir, files[0]));
            }
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

/// One-line meanings of known flags, keyed by item name: `(item, raw value or None for any,
/// text)`. Sources: the owner's record `step7-oos-safe-extract.md` section A (BMT `21 A3`
/// remote and `21 A4` transponder status, confirmed from the DDT2000 XML) and
/// `trf-investigation.md` (TRF = remote type; 1 = RF, polarity inferred with high
/// confidence). Shown in human output only; JSON is unchanged. Add rows here, not in code.
pub const FLAG_MEANINGS: &[(&str, Option<u64>, &str)] = &[
    ("TRF", Some(1), "RF remote"),
    (
        "TRF",
        Some(0),
        "infrared remote: the BMT ignores an RF remote",
    ),
    ("TIR RF RECU", None, "1 = a remote frame was received"),
    (
        "TIR RF VAL",
        None,
        "1 = the remote frame is valid (well-formed)",
    ),
    ("TIR RF OK", None, "1 = remote accepted (learned remote)"),
    (
        "TRP RECU",
        None,
        "1 = transponder frame received by the antenna ring",
    ),
    ("TRP VAL", None, "1 = transponder frame valid (well-formed)"),
    (
        "TRPOK",
        None,
        "1 = transponder authenticated, code accepted",
    ),
    ("INTCLE", None, "1 = key inserted in the ignition switch"),
    (
        "VERLOGI",
        None,
        "state of the coded (VERLOG) line to the engine ECU",
    ),
];

/// The [`FLAG_MEANINGS`] text for a decoded item, if any.
pub fn flag_meaning(i: &DecodedItem) -> Option<&'static str> {
    let name = i.name.trim();
    FLAG_MEANINGS
        .iter()
        .find(|(n, v, _)| *n == name && v.is_none_or(|v| i.raw_int() == Some(v)))
        .map(|(_, _, t)| *t)
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
                        "    {:<w$}  {} {}",
                        i.name,
                        i.display.as_deref().unwrap_or("-"),
                        i.unit.as_deref().unwrap_or("")
                    ));
                    if let Some(m) = flag_meaning(i) {
                        s.push_str(&format!("  ({m})"));
                    }
                    s.push('\n');
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

/// Lower case without French accents, for name matching.
pub fn fold_name(s: &str) -> String {
    fold(s)
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
    /// Every candidate that fits, in order.
    pub fits: Vec<usize>,
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
        fits,
    }
}

/// One place where candidate definitions disagree: a received bit field whose item name
/// differs (or exists in only some), or a list value of a sent item whose text differs (e.g. the
/// BMT `3B D2` function index 7: `CARCLIENT` in ph1_p, `CONDROUL` in SAGEM).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    pub request: Vec<u8>,
    pub place: String,
    /// (definition file stem, item name or list text there).
    pub names: Vec<(String, Option<String>)>,
}

impl Difference {
    pub fn to_json(&self) -> Json {
        let names: serde_json::Map<String, Json> = self
            .names
            .iter()
            .map(|(d, n)| (d.clone(), json!(n)))
            .collect();
        json!({"request": crate::hex(&self.request), "place": self.place, "names": names})
    }

    pub fn human(&self) -> String {
        format!(
            "{} {}: {}",
            crate::hex_sp(&self.request),
            self.place,
            self.names
                .iter()
                .map(|(d, n)| format!("{} ({d})", n.as_deref().unwrap_or("-")))
                .collect::<Vec<_>>()
                .join(" vs ")
        )
    }
}

/// File stem of a loaded definition, for messages.
pub fn def_stem(d: &LoadedDef) -> String {
    d.path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| d.def.name.clone())
}

/// Width in bits of a data definition.
fn width_bits(d: &kline_spec::DataDef) -> u32 {
    if d.is_bytes {
        d.bytes_count * 8
    } else {
        d.bits_count.max(1)
    }
}

/// Where a received item sits, in the words of docs/cli.md: `byte 3 mask 0x80` for a field
/// inside one byte (DDT2000 big-endian bit offsets, MSB first), else `byte 3 (16 bits)`.
fn place_text(first_byte: u32, bit_offset: u32, bits: u32) -> String {
    let byte = first_byte + bit_offset / 8;
    let off = bit_offset % 8;
    if off + bits <= 8 {
        let mask = (((1u32 << bits) - 1) << (8 - off - bits)) as u8;
        format!("byte {byte} mask 0x{mask:02X}")
    } else if off == 0 {
        format!("byte {byte} ({bits} bits)")
    } else {
        format!("byte {byte} bit offset {off} ({bits} bits)")
    }
}

/// Where `defs` disagree on the same request bytes: received bit fields named differently
/// (or present in only some) and sent list values with different texts. Names are compared
/// trimmed. Empty when they agree everywhere.
/// Per definition: the name or text found there.
type PerDef = Vec<Option<String>>;

pub fn candidate_differences(defs: &[&LoadedDef]) -> Vec<Difference> {
    use std::collections::BTreeMap;
    let mut payloads: Vec<Vec<u8>> = Vec::new();
    for d in defs {
        for r in &d.def.requests {
            if !payloads.contains(&r.sent_bytes.0) {
                payloads.push(r.sent_bytes.0.clone());
            }
        }
    }
    let mut out = Vec::new();
    for p in payloads {
        // (byte, bit offset, bits) -> name per definition; (byte, offset, value) -> list text.
        let mut recv: BTreeMap<(u32, u32, u32), PerDef> = BTreeMap::new();
        let mut sent: BTreeMap<(u32, u32, i64), (String, PerDef)> = BTreeMap::new();
        for (k, d) in defs.iter().enumerate() {
            for r in requests_for(&d.def, &p) {
                for i in &r.received_items {
                    let bits = d.def.data(&i.name).map(width_bits).unwrap_or(8);
                    let key = (i.first_byte + i.bit_offset / 8, i.bit_offset % 8, bits);
                    let slot = recv.entry(key).or_insert_with(|| vec![None; defs.len()]);
                    slot[k] = Some(i.name.trim().to_string());
                }
                for i in &r.sent_items {
                    let Some(data) = d.def.data(&i.name) else {
                        continue;
                    };
                    for li in &data.list {
                        let key = (i.first_byte, i.bit_offset, li.value);
                        let slot = sent
                            .entry(key)
                            .or_insert_with(|| (i.name.trim().to_string(), vec![None; defs.len()]));
                        slot.1[k] = Some(li.text.trim().to_string());
                    }
                }
            }
        }
        let differs = |v: &[Option<String>]| v.iter().any(|n| n != &v[0]);
        for ((byte, off, bits), names) in recv {
            if differs(&names) {
                out.push(Difference {
                    request: p.clone(),
                    place: place_text(byte, off, bits),
                    names: defs.iter().map(|d| def_stem(d)).zip(names).collect(),
                });
            }
        }
        for ((_, _, value), (item, names)) in sent {
            if differs(&names) {
                out.push(Difference {
                    request: p.clone(),
                    place: format!("sent {item:?} = {value}"),
                    names: defs.iter().map(|d| def_stem(d)).zip(names).collect(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod data_dir_tests {
    use super::data_dir_from;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| OsString::from(*v))
        }
    }

    #[test]
    fn unix_keeps_the_xdg_style_default() {
        assert_eq!(
            data_dir_from(false, env(&[("HOME", "/home/n"), ("APPDATA", "C:\\x")])),
            PathBuf::from("/home/n/.local/share/kline")
        );
        assert_eq!(
            data_dir_from(false, env(&[])),
            PathBuf::from("./.local/share/kline")
        );
    }

    #[test]
    fn windows_uses_appdata_then_userprofile() {
        let a = data_dir_from(true, env(&[("APPDATA", "R"), ("HOME", "/h")]));
        assert_eq!(a, PathBuf::from("R").join("kline"));
        let u = data_dir_from(true, env(&[("APPDATA", ""), ("USERPROFILE", "U")]));
        assert_eq!(
            u,
            PathBuf::from("U")
                .join("AppData")
                .join("Roaming")
                .join("kline")
        );
        assert_eq!(data_dir_from(true, env(&[])), PathBuf::from("kline-data"));
    }
}

#[cfg(test)]
mod difference_tests {
    use super::*;

    /// Hand-written two-request definition; `seven` is the text of list value 7.
    fn def(seven: &str, extra_bit: bool) -> LoadedDef {
        let extra = if extra_bit {
            r#"<DataItem Name="Extra" Ref="1" FirstByte="4" BitOffset="7"/>"#
        } else {
            ""
        };
        let xml = format!(
            r#"<Target Name="T" xmlns="http://www-diag.renault.com/2002/ECU">
<Function Name="F" Address="38"/>
<Datas>
<Data Name="Flag"><Bits count="1"><Scaled/></Bits></Data>
<Data Name="Extra"><Bits count="1"><Scaled/></Bits></Data>
<Data Name="Function"><Bits count="8"><List><Item Value="3" Text="SAME"/><Item Value="7" Text="{seven}"/></List></Bits></Data>
</Datas>
<Requests Endian="Big">
<Request Name="Read"><Sent><SentBytes>21A1</SentBytes></Sent>
<Received MinBytes="4"><DataItem Name="Flag" Ref="1" FirstByte="3" BitOffset="4"/>{extra}</Received></Request>
<Request Name="Write"><Sent><SentBytes>3BD20000</SentBytes><DataItem Name="Function" Ref="1" FirstByte="3"/></Sent>
<Received MinBytes="1"></Received></Request>
</Requests></Target>"#
        );
        LoadedDef {
            path: PathBuf::from(if extra_bit { "a.xml" } else { "b.xml" }),
            def: EcuDef::from_xml_bytes(xml.as_bytes()).unwrap(),
        }
    }

    #[test]
    fn differing_bits_and_list_texts_are_listed() {
        let (a, b) = (def("CARCLIENT ", true), def("CONDROUL", false));
        let d = candidate_differences(&[&a, &b]);
        let h: Vec<String> = d.iter().map(Difference::human).collect();
        assert_eq!(
            h,
            [
                "21 A1 byte 4 mask 0x01: Extra (a) vs - (b)",
                "3B D2 00 00 sent \"Function\" = 7: CARCLIENT (a) vs CONDROUL (b)",
            ],
            "{d:#?}"
        );
        // Same layout, texts equal after trimming: nothing to report.
        assert!(candidate_differences(&[&def("X ", false), &def("X", false)]).is_empty());
    }
}

#[cfg(test)]
mod meaning_tests {
    use super::*;

    fn item(name: &str, raw: u64) -> DecodedItem {
        DecodedItem {
            name: name.into(),
            raw: Some(Raw::Int(raw)),
            value: Some(Value::Number(raw as f64)),
            display: Some(raw.to_string()),
            unit: None,
        }
    }

    #[test]
    fn known_flags_get_a_one_line_meaning() {
        let d = vec![(
            "R".to_string(),
            Ok(vec![
                item("TRF", 1),
                item("TIR RF RECU", 0),
                item("TRPOK", 1),
                item("OTHER", 1),
            ]),
        )];
        let h = decoded_human(&d);
        assert!(h.contains("TRF          1   (RF remote)"), "{h}");
        assert!(h.contains("(1 = a remote frame was received)"), "{h}");
        assert!(
            h.contains("(1 = transponder authenticated, code accepted)"),
            "{h}"
        );
        assert!(
            h.lines().any(|l| l.trim_end().ends_with("OTHER        1")),
            "{h}"
        );
        let d = vec![("R".to_string(), Ok(vec![item("TRF", 0)]))];
        assert!(
            decoded_human(&d).contains("(infrared remote: the BMT ignores an RF remote)"),
            "{}",
            decoded_human(&d)
        );
    }
}
