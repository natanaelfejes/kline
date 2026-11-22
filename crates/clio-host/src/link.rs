//! Runs clio-core's machines over a [`Transport`] in real time, tracing every action and
//! recording the JSONL capture.

use std::io;

use clap::ValueEnum;
use clio_core::frame::Reply;
use clio_core::init::{FastInit, FiveBaudInit, InitOutcome};
use clio_core::session::{Session, SessionError, SessionOutcome};
use clio_core::{Action, Event, InitKind, InitVariant, LinkConfig, LinkError, Step};
use serde_json::{json, Value as Json};

use crate::capture::{self, Capture, Dir, Entry, Kind};
use crate::error::CliError;
use crate::trace::Tracer;
use crate::transport::Transport;
use crate::{hex, hex_sp};

/// `--variant` (docs/protocol.md "Init variants").
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum VariantArg {
    /// Try single, multi, fast in the documented order; log which worked.
    #[default]
    Auto,
    Single,
    Multi,
    Fast,
}

impl VariantArg {
    pub fn label(self) -> &'static str {
        match self {
            VariantArg::Auto => "auto",
            VariantArg::Single => "single",
            VariantArg::Multi => "multi",
            VariantArg::Fast => "fast",
        }
    }
}

pub fn variant_label(v: InitVariant) -> &'static str {
    match v {
        InitVariant::SingleEcu => "single",
        InitVariant::MultiEcu => "multi",
        InitVariant::FastInit => "fast",
    }
}

/// Variants to try for `cfg`, in the order of docs/protocol.md: SingleEcu, MultiEcu, FastInit
/// for a KWP 5-baud ECU; variants that would run the identical init are skipped (MultiEcu
/// without `Multipoint`, anything but SingleEcu for ISO8, only fast init for a fast-init ECU).
pub fn variants(cfg: &LinkConfig, arg: VariantArg) -> Vec<InitVariant> {
    let all = match arg {
        VariantArg::Auto => vec![
            InitVariant::SingleEcu,
            InitVariant::MultiEcu,
            InitVariant::FastInit,
        ],
        VariantArg::Single => vec![InitVariant::SingleEcu],
        VariantArg::Multi => vec![InitVariant::MultiEcu],
        VariantArg::Fast => vec![InitVariant::FastInit],
    };
    if arg != VariantArg::Auto {
        return all;
    }
    if matches!(cfg.timing.init, InitKind::KwpFast { .. }) {
        // A fast-init definition: every variant runs the same fast init; call it that.
        return vec![InitVariant::FastInit];
    }
    let mut seen: Vec<(InitKind, u8)> = Vec::new();
    all.into_iter()
        .filter(|v| {
            let mut c = cfg.clone();
            c.variant = *v;
            let key = (c.init_kind(), c.five_baud_address_byte());
            if seen.contains(&key) {
                false
            } else {
                seen.push(key);
                true
            }
        })
        .collect()
}

/// `--session` after the init (docs/protocol.md "Session": the CLI decides).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum SessionArg {
    /// `10 A0` if the definition has it, else `10 81` if it has that, else nothing.
    #[default]
    Auto,
    None,
    A0,
    #[value(name = "81")]
    S81,
}

/// Failure of an exchange: the transport, or the session machine.
#[derive(Debug)]
pub enum ExchangeError {
    Io(io::Error),
    Session(SessionError),
}

impl ExchangeError {
    pub fn to_cli(&self) -> CliError {
        match self {
            ExchangeError::Io(e) => CliError::port(format!("transport I/O error: {e}")),
            ExchangeError::Session(e) => CliError::session(e),
        }
    }

    /// True for a missing or broken reply (a scan counts these), false for the transport.
    pub fn is_link(&self) -> bool {
        matches!(self, ExchangeError::Session(SessionError::Link(_)))
    }
}

/// The K line plus its trace and capture.
pub struct Link {
    t: Box<dyn Transport>,
    trace: Tracer,
    cap: Option<Capture>,
    marker: Option<(Kind, Vec<u8>)>,
}

impl Link {
    pub fn new(t: Box<dyn Transport>, trace: Tracer, cap: Option<Capture>) -> Link {
        Link {
            t,
            trace,
            cap,
            marker: None,
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.t.now_ms()
    }

    pub fn sleep_ms(&mut self, ms: u32) {
        self.trace.line(self.t.now_us(), format!("WAIT {ms} ms"));
        self.t.sleep_ms(ms);
    }

    pub fn capture(&self) -> Option<&Capture> {
        self.cap.as_ref()
    }

    fn record(&mut self, t_ms: u64, dir: Dir, kind: Option<Kind>, bytes: &[u8]) {
        if let Some(c) = &mut self.cap {
            c.push(&Entry {
                t_ms,
                dir,
                kind,
                hex: hex(bytes),
                unix_ms: Some(capture::unix_ms()),
            });
        }
    }

    /// Perform one action: trace, transport, capture.
    pub fn act(&mut self, a: &Action) -> io::Result<Event> {
        let t0_us = self.t.now_us();
        let t0 = self.t.now_ms();
        if matches!(a, Action::SetBreak(true)) {
            if let Some((kind, bytes)) = self.marker.take() {
                // First break of the init: where the wake-up starts (sim replay format).
                self.record(t0, Dir::Tx, Some(kind), &bytes);
            }
        }
        let tr = self.trace.clone();
        let ev = match a {
            Action::SetDtr(on) => {
                tr.line(t0_us, format!("DTR {}", if *on { "high" } else { "low" }));
                self.t.set_dtr(*on)?;
                Event::Done
            }
            Action::SetBreak(on) => {
                tr.line(
                    t0_us,
                    if *on {
                        "BREAK on  (K low)"
                    } else {
                        "BREAK off (K high)"
                    },
                );
                self.t.set_break(*on)?;
                Event::Done
            }
            Action::Sleep { ms } => {
                tr.line(t0_us, format!("SLEEP {ms} ms"));
                self.t.sleep_ms(*ms);
                Event::Done
            }
            Action::CheckBreakLoopback => {
                let seen = self.t.break_loopback_seen()?;
                tr.line(
                    t0_us,
                    format!("BREAK LOOPBACK {}", if seen { "seen" } else { "NOT seen" }),
                );
                Event::BreakLoopbackSeen(seen)
            }
            Action::FlushRx => {
                tr.line(t0_us, "FLUSH RX");
                self.t.flush_rx()?;
                Event::Done
            }
            Action::Write {
                frame,
                inter_byte_ms,
            } => {
                tr.line(
                    t0_us,
                    format!(
                        "TX {} (inter-byte {inter_byte_ms} ms)",
                        hex_sp(frame.bytes())
                    ),
                );
                self.t.write(frame.bytes(), *inter_byte_ms)?;
                self.record(t0, Dir::Tx, None, frame.bytes());
                Event::Done
            }
            Action::Read { count, timeout_ms } => {
                let b = self.t.read(*count, *timeout_ms)?;
                let t1 = self.t.now_us();
                let dt = (t1.saturating_sub(t0_us)) as f64 / 1000.0;
                tr.line(
                    t1,
                    format!(
                        "RX {}/{count} in {dt:.1} ms (timeout {timeout_ms}){}{}",
                        b.len(),
                        if b.is_empty() { "" } else { ": " },
                        hex_sp(&b)
                    ),
                );
                if !b.is_empty() {
                    self.record(self.t.now_ms(), Dir::Rx, None, &b);
                }
                if b.len() == *count {
                    Event::ReadOk(b)
                } else {
                    Event::ReadTimeout(b)
                }
            }
        };
        Ok(ev)
    }

    fn pump<T, E>(
        &mut self,
        first: Action,
        mut feed: impl FnMut(Event, u64) -> Step<T, E>,
    ) -> io::Result<Result<T, E>> {
        let mut a = first;
        loop {
            let ev = self.act(&a)?;
            match feed(ev, self.t.now_ms()) {
                Step::Act(next) => a = next,
                Step::Done(t) => return Ok(Ok(t)),
                Step::Failed(e) => return Ok(Err(e)),
            }
        }
    }

    /// Run one init with `cfg`. Outer error: transport; inner: the init failed.
    pub fn init(&mut self, cfg: &LinkConfig) -> io::Result<Result<InitOutcome, LinkError>> {
        let now = self.t.now_us();
        match cfg.init_kind() {
            InitKind::KwpFast { .. } => {
                let mut m = match FastInit::new(cfg) {
                    Ok(m) => m,
                    Err(e) => return Ok(Err(e)),
                };
                self.trace.line(now, "INIT fast init + StartCommunication");
                self.marker = Some((Kind::FastInit, Vec::new()));
                let first = m.start();
                self.pump(first, |ev, _| m.on_event(ev))
            }
            _ => {
                let mut m = match FiveBaudInit::new(cfg) {
                    Ok(m) => m,
                    Err(e) => return Ok(Err(e)),
                };
                self.trace.line(
                    now,
                    format!("INIT 5-baud address byte {:02X}", m.address_byte()),
                );
                self.marker = Some((Kind::FiveBaud, vec![m.address_byte()]));
                let first = m.start();
                self.pump(first, |ev, _| m.on_event(ev))
            }
        }
    }

    pub fn session(&self, init: InitOutcome) -> Session {
        Session::new(init, self.t.now_ms())
    }

    /// Run an exchange the session already started (`first` from `Session::request`,
    /// `request_scan`, `request_approved`, ...).
    pub fn run(&mut self, s: &mut Session, first: Action) -> Result<SessionOutcome, ExchangeError> {
        match self.pump(first, |ev, now| s.on_event(ev, now)) {
            Ok(Ok(o)) => Ok(o),
            Ok(Err(e)) => Err(ExchangeError::Session(e)),
            Err(e) => Err(ExchangeError::Io(e)),
        }
    }

    /// Send a Tier 0 request.
    pub fn request(
        &mut self,
        s: &mut Session,
        payload: &[u8],
    ) -> Result<SessionOutcome, ExchangeError> {
        let first = s
            .request(payload, self.t.now_ms())
            .map_err(ExchangeError::Session)?;
        self.run(s, first)
    }

    /// TesterPresent if it is due now. Errors are returned, a missing reply is fine.
    pub fn keepalive(&mut self, s: &mut Session) -> Result<(), ExchangeError> {
        if s.tester_present_due(self.t.now_ms()) {
            let first = s
                .tester_present(self.t.now_ms())
                .map_err(ExchangeError::Session)?;
            self.run(s, first)?;
        }
        Ok(())
    }

    /// Stop the session (best effort).
    pub fn stop(&mut self, s: &mut Session) -> Result<(), ExchangeError> {
        if s.is_closed() {
            return Ok(());
        }
        let first = s.stop(self.t.now_ms()).map_err(ExchangeError::Session)?;
        self.run(s, first).map(drop)
    }
}

/// One init attempt, for the connect report.
#[derive(Debug, Clone)]
pub struct Attempt {
    pub variant: InitVariant,
    pub address_byte: Option<u8>,
    pub error: Option<String>,
}

impl Attempt {
    pub fn to_json(&self) -> Json {
        json!({
            "event": "init_attempt",
            "variant": variant_label(self.variant),
            "address_byte": self.address_byte.map(|b| format!("{b:02X}")),
            "ok": self.error.is_none(),
            "error": self.error,
        })
    }
}

/// What the successful init found.
#[derive(Debug, Clone)]
pub struct Connected {
    pub variant: InitVariant,
    pub attempts: Vec<Attempt>,
    pub address_byte: Option<u8>,
    pub sync: Option<u8>,
    pub keywords: (u8, u8),
    pub address_complement: Option<u8>,
    pub keywords_match: Option<bool>,
    pub kind: InitKind,
}

/// Try `variants` in order until one init succeeds; `on_attempt` sees each attempt as it ends.
pub fn connect(
    link: &mut Link,
    base: &LinkConfig,
    variants: &[InitVariant],
    on_attempt: &mut dyn FnMut(&Attempt),
) -> Result<(Session, Connected), CliError> {
    let mut attempts = Vec::new();
    for &v in variants {
        let mut cfg = base.clone();
        cfg.variant = v;
        let address_byte = (!matches!(cfg.init_kind(), InitKind::KwpFast { .. }))
            .then(|| cfg.five_baud_address_byte());
        let r = link.init(&cfg).map_err(CliError::io)?;
        let a = Attempt {
            variant: v,
            address_byte,
            error: r.as_ref().err().map(|e| e.to_string()),
        };
        on_attempt(&a);
        attempts.push(a);
        if let Ok(o) = r {
            let c = Connected {
                variant: v,
                attempts,
                address_byte: o.address_byte(),
                sync: o.sync(),
                keywords: o.keywords(),
                address_complement: o.address_complement(),
                keywords_match: o.keywords_match(),
                kind: o.kind(),
            };
            let s = link.session(o);
            return Ok((s, c));
        }
    }
    let summary: Vec<String> = attempts
        .iter()
        .map(|a| {
            format!(
                "{}: {}",
                variant_label(a.variant),
                a.error.as_deref().unwrap_or("?")
            )
        })
        .collect();
    Err(CliError::new(
        crate::error::exit::INIT,
        "init",
        format!("every init attempt failed ({})", summary.join("; ")),
    ))
}

/// Positive payload of an outcome, or the error to report.
pub fn positive(o: &SessionOutcome) -> Result<&[u8], CliError> {
    match o {
        SessionOutcome::Reply(Reply::Positive(p)) => Ok(p),
        SessionOutcome::Reply(Reply::Negative { sid, nrc }) => Err(CliError::new(
            crate::error::exit::NEGATIVE,
            "negative",
            format!(
                "ECU answered negatively: 7F {sid:02X} {nrc:02X} ({})",
                nrc_name(*nrc)
            ),
        )),
        other => Err(CliError::new(
            crate::error::exit::COMM,
            "comm",
            format!("unexpected outcome {other:?}"),
        )),
    }
}

/// KWP2000 negative response code names (ISO 14230-3).
pub fn nrc_name(nrc: u8) -> &'static str {
    match nrc {
        0x10 => "generalReject",
        0x11 => "serviceNotSupported",
        0x12 => "subFunctionNotSupported-invalidFormat",
        0x21 => "busy-repeatRequest",
        0x22 => "conditionsNotCorrect",
        0x23 => "routineNotComplete",
        0x31 => "requestOutOfRange",
        0x33 => "securityAccessDenied",
        0x35 => "invalidKey",
        0x36 => "exceedNumberOfAttempts",
        0x78 => "responsePending",
        0x80 => "serviceNotSupportedInActiveDiagnosticSession",
        _ => "unknown",
    }
}
