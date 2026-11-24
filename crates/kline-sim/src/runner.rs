//! The `kline-sim` runner: one simulated K line with any number of ECUs behind a pty, for
//! rehearsing a car session at home (docs/sim.md "Running it"). macOS and Linux only (a pty
//! and a Unix socket); there is no Windows build of the runner.
//!
//! [`Config::parse`] reads the command line, [`Config::build`] makes the line and its
//! [`Control`], [`start`] puts it behind a [`crate::pty::PtySim`]. The binary
//! (`src/bin/kline-sim.rs`) only prints and waits; tests call the same functions.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::builtin::{Builtin, BUILTINS};
use crate::bus::{Bus, BusConfig};
use crate::ecu::{
    BitMemory, EcuProfile, FaultRule, InitFault, ReplyFault, SpecEcu, BOSCH_ABS_ADDRESS,
};
use crate::frame::WireFormat;
use crate::journal::Journal;
use crate::multi::{MultiEcu, Presence, Slot, Transcript};
use crate::replay::{read_jsonl, ReplayEcu};
use crate::scenario::Scenario;
use crate::timing::Tolerance;

/// BMT address in the X65 addressing (docs/protocol.md "The Clio II ECUs").
pub const BMT_ADDRESS: u8 = 0x26;

pub const USAGE: &str = "\
kline-sim: simulated K line with ECUs behind a pty (macOS, Linux)

usage: kline-sim [OPTIONS]
       kline-sim ctl SOCKET COMMAND..

Without --ecu or --replay the whole simulated car runs: sirius (7A), bmt (26), airbag (2C),
abs (01), all built in (hand-written definitions, no Renault file needed).

ECUs (addresses are hex):
  --ecu NAME                 built-in ECU: sirius, bmt, airbag, abs
  --ecu PATH.xml             ECU from a definition, at its <Function Address>
  --ecu ADDR=PATH.xml        the same, checked against ADDR
  --replay ADDR=FILE.jsonl   play a capture back as the ECU at ADDR (one session)
Answers and faults (requests are hex payloads):
  --answer ADDR:REQ=positive|nrc:NN|HEX   answer exactly REQ so (e.g. 7A:2701=nrc:22)
  --fault ADDR:REQ=F[,after=N][,times=N]  REQ: hex prefix or * (every request);
         F: silent | nrc:NN | bad-checksum | pending:N | drop:N | slow:MS
  --fault ADDR:init=silent|no-complement|wrong-keywords:KKKK|bad-sync:NN
BMT lock memory (21 A3 data byte 0 bit 7, 1 unlocked):
  --bmt-lock-follows true|false   follows K-line 31 A0 00 01 / 31 A1 00 01 (default false,
                                  as the car did)
  --bmt-lock-initial locked|unlocked   (default unlocked)
Line:
  --strict              ISO timing windows (default lenient: a pty jitters)
  --control PATH        control socket (default <temp dir>/kline-sim.ctl)
  --transcript FILE     transcript to FILE instead of stdout
  --write-defs DIR      write the built-in definitions into DIR under the car's file names
                        (never overwrites), for kline --defs DIR

Control commands (kline-sim ctl SOCKET ..., or the socket itself):
  ECUS?                 the ECUs and whether each is on the line
  ECU ADDR OFF|ON       take an ECU off the line / put it back
  LOCK ADDR?            BMT lock memory: LOCKED or UNLOCKED
  LOCK ADDR LOCKED|UNLOCKED   press the dashboard lock button
";

#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Setup(String),
}

fn usage(s: impl Into<String>) -> RunnerError {
    RunnerError::Usage(s.into())
}

/// Where one ECU comes from.
#[derive(Debug, Clone)]
pub enum Source {
    Builtin(&'static str),
    Xml(PathBuf),
    Replay(PathBuf),
}

#[derive(Debug, Clone)]
pub struct EcuArg {
    /// None: the definition's address.
    pub address: Option<u8>,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub enum FaultArg {
    Init(InitFault),
    Reply(FaultRule),
}

/// The parsed command line.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub ecus: Vec<EcuArg>,
    pub answers: Vec<(u8, Vec<u8>, AnswerArg)>,
    pub faults: Vec<(u8, FaultArg)>,
    pub bmt_lock_follows: bool,
    pub bmt_lock_locked: bool,
    pub strict: bool,
    pub control: Option<PathBuf>,
    pub transcript: Option<PathBuf>,
    pub write_defs: Option<PathBuf>,
    pub help: bool,
}

#[derive(Debug, Clone)]
pub enum AnswerArg {
    Positive,
    Nrc(u8),
    Payload(Vec<u8>),
}

pub fn parse_hex(s: &str) -> Result<Vec<u8>, RunnerError> {
    let t: String = s
        .trim()
        .trim_start_matches("0x")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if t.is_empty() || !t.len().is_multiple_of(2) {
        return Err(usage(format!("not hex bytes: {s:?}")));
    }
    (0..t.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&t[i..i + 2], 16))
        .collect::<Result<_, _>>()
        .map_err(|_| usage(format!("not hex bytes: {s:?}")))
}

pub fn parse_addr(s: &str) -> Result<u8, RunnerError> {
    let t = s.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    u8::from_str_radix(t, 16).map_err(|_| usage(format!("not a hex address: {s:?} (e.g. 26)")))
}

fn parse_num<T: std::str::FromStr>(s: &str, what: &str) -> Result<T, RunnerError> {
    s.parse()
        .map_err(|_| usage(format!("{what}: not a number: {s:?}")))
}

fn split_addr(s: &str, flag: &str) -> Result<(u8, String), RunnerError> {
    let (a, rest) = s
        .split_once(':')
        .ok_or_else(|| usage(format!("{flag} {s:?}: want ADDR:REQ=..")))?;
    Ok((parse_addr(a)?, rest.to_string()))
}

fn parse_answer(s: &str) -> Result<(u8, Vec<u8>, AnswerArg), RunnerError> {
    let (addr, rest) = split_addr(s, "--answer")?;
    let (req, what) = rest
        .split_once('=')
        .ok_or_else(|| usage(format!("--answer {s:?}: want ADDR:REQ=ANSWER")))?;
    let req = parse_hex(req)?;
    let w = what.trim().to_ascii_lowercase();
    let a = if w == "positive" {
        AnswerArg::Positive
    } else if let Some(n) = w.strip_prefix("nrc:") {
        AnswerArg::Nrc(parse_addr(n)?)
    } else {
        AnswerArg::Payload(parse_hex(&w)?)
    };
    Ok((addr, req, a))
}

fn parse_fault(s: &str) -> Result<(u8, FaultArg), RunnerError> {
    let (addr, rest) = split_addr(s, "--fault")?;
    let (on, spec) = rest
        .split_once('=')
        .ok_or_else(|| usage(format!("--fault {s:?}: want ADDR:REQ=FAULT")))?;
    let mut parts = spec.split(',');
    let kind = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let (k, arg) = match kind.split_once(':') {
        Some((k, a)) => (k.to_string(), Some(a.to_string())),
        None => (kind.clone(), None),
    };
    let need = |what: &str| {
        arg.clone()
            .ok_or_else(|| usage(format!("--fault {s:?}: {k} needs :{what}")))
    };
    if on.eq_ignore_ascii_case("init") {
        let f = match k.as_str() {
            "silent" => InitFault::Silent,
            "no-complement" => InitFault::NoAddressComplement,
            "wrong-keywords" => {
                let b = parse_hex(&need("KKKK")?)?;
                if b.len() != 2 {
                    return Err(usage("wrong-keywords takes two bytes, e.g. D08E"));
                }
                InitFault::WrongKeywords(b[0], b[1])
            }
            "bad-sync" => InitFault::BadSync(parse_addr(&need("NN")?)?),
            _ => return Err(usage(format!("--fault {s:?}: unknown init fault {k:?}"))),
        };
        if parts.next().is_some() {
            return Err(usage("an init fault takes no after=/times="));
        }
        return Ok((addr, FaultArg::Init(f)));
    }
    let fault = match k.as_str() {
        "silent" => ReplyFault::NoReply,
        "nrc" => ReplyFault::Negative {
            nrc: parse_addr(&need("NN")?)?,
        },
        "bad-checksum" => ReplyFault::BadChecksum,
        "pending" => ReplyFault::ResponsePending {
            count: parse_num(&need("N")?, "pending")?,
        },
        "drop" => ReplyFault::DropBytes {
            count: parse_num(&need("N")?, "drop")?,
        },
        "slow" => ReplyFault::Slow {
            delay_ms: parse_num(&need("MS")?, "slow")?,
        },
        _ => return Err(usage(format!("--fault {s:?}: unknown fault {k:?}"))),
    };
    let mut rule = FaultRule {
        on: if on.trim() == "*" {
            None
        } else {
            Some(parse_hex(on)?)
        },
        fault,
        times: None,
        skip: 0,
    };
    for p in parts {
        match p.trim().split_once('=') {
            Some(("after", n)) => rule.skip = parse_num(n, "after")?,
            Some(("times", n)) => rule.times = Some(parse_num(n, "times")?),
            _ => return Err(usage(format!("--fault {s:?}: unknown option {p:?}"))),
        }
    }
    Ok((addr, FaultArg::Reply(rule)))
}

fn parse_bool(s: &str, flag: &str) -> Result<bool, RunnerError> {
    match s {
        "true" | "yes" | "1" => Ok(true),
        "false" | "no" | "0" => Ok(false),
        _ => Err(usage(format!("{flag}: want true or false, got {s:?}"))),
    }
}

impl Config {
    /// Parse the options (without the program name).
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Config, RunnerError> {
        let mut c = Config::default();
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            let (flag, inline) = match a.split_once('=') {
                Some((f, v)) if f.starts_with("--") && !f.contains(':') => {
                    (f.to_string(), Some(v.to_string()))
                }
                _ => (a.clone(), None),
            };
            let mut val = || -> Result<String, RunnerError> {
                match &inline {
                    Some(v) => Ok(v.clone()),
                    None => it
                        .next()
                        .ok_or_else(|| usage(format!("{flag} needs a value"))),
                }
            };
            match flag.as_str() {
                "-h" | "--help" => c.help = true,
                "--ecu" => {
                    let v = val()?;
                    let e = if let Some(b) = Builtin::by_name(&v) {
                        EcuArg {
                            address: Some(b.address),
                            source: Source::Builtin(b.name),
                        }
                    } else if let Some((addr, path)) = v.split_once('=') {
                        EcuArg {
                            address: Some(parse_addr(addr)?),
                            source: match path.strip_prefix("builtin:") {
                                Some(n) => Source::Builtin(
                                    Builtin::by_name(n)
                                        .ok_or_else(|| usage(format!("no built-in ECU {n:?}")))?
                                        .name,
                                ),
                                None => Source::Xml(PathBuf::from(path)),
                            },
                        }
                    } else {
                        EcuArg {
                            address: None,
                            source: Source::Xml(PathBuf::from(&v)),
                        }
                    };
                    c.ecus.push(e);
                }
                "--replay" => {
                    let v = val()?;
                    let (addr, path) = v
                        .split_once('=')
                        .ok_or_else(|| usage(format!("--replay {v:?}: want ADDR=FILE.jsonl")))?;
                    c.ecus.push(EcuArg {
                        address: Some(parse_addr(addr)?),
                        source: Source::Replay(PathBuf::from(path)),
                    });
                }
                "--answer" => c.answers.push(parse_answer(&val()?)?),
                "--fault" => c.faults.push(parse_fault(&val()?)?),
                "--bmt-lock-follows" => {
                    c.bmt_lock_follows = parse_bool(&val()?, "--bmt-lock-follows")?
                }
                "--bmt-lock-initial" => {
                    c.bmt_lock_locked = match val()?.as_str() {
                        "locked" => true,
                        "unlocked" => false,
                        v => {
                            return Err(usage(format!(
                                "--bmt-lock-initial: want locked or unlocked, got {v:?}"
                            )))
                        }
                    }
                }
                "--strict" => c.strict = true,
                "--control" => c.control = Some(PathBuf::from(val()?)),
                "--transcript" => c.transcript = Some(PathBuf::from(val()?)),
                "--write-defs" => c.write_defs = Some(PathBuf::from(val()?)),
                _ => return Err(usage(format!("unknown option {a:?} (kline-sim --help)"))),
            }
        }
        Ok(c)
    }

    /// The ECUs to run: the given ones, or the whole built-in car.
    pub fn ecus(&self) -> Vec<EcuArg> {
        if self.ecus.is_empty() {
            BUILTINS
                .iter()
                .map(|b| EcuArg {
                    address: Some(b.address),
                    source: Source::Builtin(b.name),
                })
                .collect()
        } else {
            self.ecus.clone()
        }
    }

    /// Build the line. `transcript`: where the transcript goes (the binary passes stdout or
    /// the `--transcript` file).
    pub fn build(&self, transcript: Option<Transcript>) -> Result<Built, RunnerError> {
        let mut slots = Vec::new();
        let mut info = Vec::new();
        let mut presence = Vec::new();
        let mut locks = Vec::new();
        for e in self.ecus() {
            let (slot, line, lock) = self.build_one(&e)?;
            if slots.iter().any(|s: &Slot| s.address == slot.address) {
                return Err(usage(format!(
                    "two ECUs at {:02X}: one ECU per address",
                    slot.address
                )));
            }
            presence.push((slot.address, slot.name.clone(), slot.presence.clone()));
            info.push(line);
            if let Some(m) = lock {
                locks.push((slot.address, m));
            }
            slots.push(slot);
        }
        for (a, _, _) in &self.answers {
            check_addr(&slots, *a, "--answer")?;
        }
        for (a, _) in &self.faults {
            check_addr(&slots, *a, "--fault")?;
        }
        let journals = slots
            .iter()
            .map(|s| (s.address, s.name.clone(), s.journal.clone()))
            .collect();
        let tol = self.tolerance(0);
        let bus = Bus::new(
            MultiEcu::new(slots, transcript),
            BusConfig::default(),
            tol,
            Journal::new(),
        );
        Ok(Built {
            bus,
            control: Control { presence, locks },
            info,
            journals,
        })
    }

    fn tolerance(&self, p3: u64) -> Tolerance {
        if self.strict {
            Tolerance::strict(p3)
        } else {
            Tolerance::lenient()
        }
    }

    fn build_one(&self, e: &EcuArg) -> Result<(Slot, String, Option<BitMemory>), RunnerError> {
        if let Source::Replay(path) = &e.source {
            let addr = e.address.expect("--replay has an address");
            if self
                .answers
                .iter()
                .map(|x| x.0)
                .chain(self.faults.iter().map(|x| x.0))
                .any(|a| a == addr)
            {
                return Err(usage(format!(
                    "--answer/--fault on {addr:02X}: a replayed ECU only plays its capture"
                )));
            }
            let f = std::fs::File::open(path)
                .map_err(|err| RunnerError::Setup(format!("{}: {err}", path.display())))?;
            let entries = read_jsonl(std::io::BufReader::new(f))
                .map_err(|err| RunnerError::Setup(format!("{}: {err}", path.display())))?;
            let n = entries.len();
            let r = ReplayEcu::new(entries);
            let j = r.journal();
            let fmt = if addr == BOSCH_ABS_ADDRESS {
                WireFormat::Iso8
            } else {
                WireFormat::Kwp {
                    length_in_format: true,
                }
            };
            let mut slot = Slot::new(addr, "replay", r, fmt, j);
            slot.fast_init = false;
            let line = format!(
                "{addr:02X} replay of {} ({n} entries, one session)",
                path.display()
            );
            return Ok((slot, line, None));
        }
        let (def, scenario, name, what) = match &e.source {
            Source::Builtin(n) => {
                let b = Builtin::by_name(n).expect("parsed built-in");
                (b.def(), b.scenario(), b.name.to_string(), "built-in".into())
            }
            Source::Xml(p) => {
                let bytes = std::fs::read(p)
                    .map_err(|err| RunnerError::Setup(format!("{}: {err}", p.display())))?;
                let def = kline_spec::EcuDef::from_xml_bytes(&bytes)
                    .map_err(|err| RunnerError::Setup(format!("{}: {err}", p.display())))?;
                let stem = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "ecu".into());
                (def, Scenario::new(), stem, p.display().to_string())
            }
            Source::Replay(_) => unreachable!(),
        };
        let mut profile = EcuProfile::from_def(&def).map_err(|err| {
            RunnerError::Setup(format!("{name}: cannot simulate this definition: {err}"))
        })?;
        if let Some(a) = e.address {
            if a != profile.address {
                return Err(usage(format!(
                    "{name}: the definition's address is {:02X}, not {a:02X}",
                    profile.address
                )));
            }
        }
        profile.tolerance = self.tolerance(profile.tolerance.p3_min_ms);
        let addr = profile.address;
        let offset = profile.positive_sid_offset;
        let fmt = profile.wire_format();
        let mut ecu = SpecEcu::new(profile, def, scenario)
            .map_err(|err| RunnerError::Setup(format!("{name}: {err}")))?;
        for (a, req, ans) in &self.answers {
            if *a != addr {
                continue;
            }
            let reply = match ans {
                AnswerArg::Positive => {
                    let mut r = req.clone();
                    r[0] = r[0].wrapping_add(offset);
                    r
                }
                AnswerArg::Nrc(n) => vec![0x7F, req[0], *n],
                AnswerArg::Payload(p) => p.clone(),
            };
            ecu = ecu.with_reply(req, &reply);
        }
        for (a, f) in &self.faults {
            if *a != addr {
                continue;
            }
            ecu = match f {
                FaultArg::Init(i) => ecu.with_init_fault(*i),
                FaultArg::Reply(r) => ecu.with_fault(r.clone()),
            };
        }
        let mut line = format!("{addr:02X} {name} ({what})");
        let mut lock = None;
        if addr == BMT_ADDRESS {
            let m = BitMemory::bmt_lock(self.bmt_lock_follows, !self.bmt_lock_locked);
            line.push_str(&format!(
                ", lock memory {} and {} K-line 31 A0/31 A1",
                if self.bmt_lock_locked {
                    "locked"
                } else {
                    "unlocked"
                },
                if self.bmt_lock_follows {
                    "follows"
                } else {
                    "does not follow"
                }
            ));
            ecu = ecu.with_bit_memory(m.clone());
            lock = Some(m);
        }
        let j = ecu.journal();
        Ok((Slot::new(addr, name, ecu, fmt, j), line, lock))
    }
}

fn check_addr(slots: &[Slot], a: u8, flag: &str) -> Result<(), RunnerError> {
    if slots.iter().any(|s| s.address == a) {
        Ok(())
    } else {
        Err(usage(format!("{flag}: no ECU at {a:02X}")))
    }
}

/// The line, ready for [`crate::pty::PtySim::spawn_with_control`].
pub struct Built {
    pub bus: Bus,
    pub control: Control,
    /// One line per ECU, for the banner.
    pub info: Vec<String>,
    /// Each ECU's journal (address, name, journal).
    pub journals: Vec<(u8, String, Journal)>,
}

/// What the control socket can change while the line runs.
#[derive(Clone)]
pub struct Control {
    presence: Vec<(u8, String, Presence)>,
    locks: Vec<(u8, BitMemory)>,
}

impl Control {
    /// Answer one control command (case-insensitive), None: not a runner command.
    pub fn command(&self, line: &str) -> Option<String> {
        let up = line.trim().to_ascii_uppercase();
        let w: Vec<&str> = up.split_whitespace().collect();
        let find = |a: &str| -> Option<&(u8, String, Presence)> {
            let a = parse_addr(a).ok()?;
            self.presence.iter().find(|(x, _, _)| *x == a)
        };
        let lock = |a: &str| -> Option<&BitMemory> {
            let a = parse_addr(a.trim_end_matches('?')).ok()?;
            self.locks.iter().find(|(x, _)| *x == a).map(|(_, m)| m)
        };
        let state = |m: &BitMemory| if m.get() { "UNLOCKED" } else { "LOCKED" };
        match w.as_slice() {
            ["ECUS?"] => Some(
                self.presence
                    .iter()
                    .map(|(a, n, p)| {
                        format!("{a:02X} {n} {}", if p.is_on() { "on" } else { "off" })
                    })
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            ["ECU", a, on @ ("ON" | "OFF")] => find(a).map(|(_, _, p)| {
                p.set(*on == "ON");
                "OK".to_string()
            }),
            ["LOCK", a] if a.ends_with('?') => lock(a).map(|m| state(m).to_string()),
            ["LOCK", a, "?"] => lock(a).map(|m| state(m).to_string()),
            ["LOCK", a, s @ ("LOCKED" | "UNLOCKED")] => lock(a).map(|m| {
                m.set(*s == "UNLOCKED");
                "OK".to_string()
            }),
            _ => None,
        }
    }
}

/// A running runner: the pty and what to tell the owner.
pub struct Running {
    pub sim: crate::pty::PtySim,
    pub control: Control,
    pub info: Vec<String>,
    pub journals: Vec<(u8, String, Journal)>,
}

/// Default control socket path.
pub fn default_control_path() -> PathBuf {
    std::env::temp_dir().join("kline-sim.ctl")
}

/// Remove a socket left by a simulator that was killed (nobody accepts on it). A live one is
/// an error: two simulators would share the name.
fn clear_stale_socket(path: &Path) -> Result<(), RunnerError> {
    if !path.exists() {
        return Ok(());
    }
    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        return Err(RunnerError::Setup(format!(
            "{} is in use by another simulator; stop it or pass --control PATH",
            path.display()
        )));
    }
    std::fs::remove_file(path)
        .map_err(|e| RunnerError::Setup(format!("cannot remove stale {}: {e}", path.display())))
}

/// Build the line and run it behind a pty.
pub fn start(cfg: &Config, transcript: Option<Transcript>) -> Result<Running, RunnerError> {
    let built = cfg.build(transcript)?;
    let ctl = cfg.control.clone().unwrap_or_else(default_control_path);
    clear_stale_socket(&ctl)?;
    let control = built.control.clone();
    let c2 = built.control;
    let sim = crate::pty::PtySim::spawn_with_control(
        built.bus,
        &ctl,
        Box::new(move |line| c2.command(line)),
    )
    .map_err(|e| RunnerError::Setup(format!("cannot open a pty / control socket: {e}")))?;
    Ok(Running {
        sim,
        control,
        info: built.info,
        journals: built.journals,
    })
}

/// Send one command to a running simulator's control socket and return its reply.
pub fn send_control(socket: &Path, command: &str) -> std::io::Result<String> {
    use std::io::{BufRead, BufReader, Write};
    let mut s = std::os::unix::net::UnixStream::connect(socket)?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    s.write_all(format!("{}\n", command.trim()).as_bytes())?;
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line)?;
    Ok(line.trim().to_string())
}
