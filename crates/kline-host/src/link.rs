//! Runs kline-core's machines over a [`Transport`] in real time, tracing every action and
//! recording the JSONL capture.

use std::io;

use clap::ValueEnum;
use kline_core::frame::Reply;
use kline_core::init::{FastInit, FiveBaudInit, InitOutcome};
use kline_core::session::{Session, SessionError, SessionOutcome};
use kline_core::{Action, Event, InitKind, InitVariant, LinkConfig, LinkError, Step};
use serde_json::{json, Value as Json};

use crate::capture::{self, Capture, Dir, Entry, Kind};
use crate::error::CliError;
use crate::trace::Tracer;
use crate::transport::Transport;
use crate::{hex, hex_sp};

/// `--variant` (docs/protocol.md "Init variants").
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum VariantArg {
    /// DDT2000's detection order (fast init, then plain 5-baud), then multi; log which worked.
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

/// Variants to try for `cfg`. Auto: DDT2000's detection order for 0x7A / 0x26
/// ([`kline_core::retry::DDT2000_DETECTION_ORDER`]: FastInit, then SingleEcu), then our MultiEcu
/// fallback. Variants that would run the identical init are skipped (MultiEcu without
/// `Multipoint`, only SingleEcu for ISO8, only fast init for a fast-init ECU).
pub fn variants(cfg: &LinkConfig, arg: VariantArg) -> Vec<InitVariant> {
    let all = match arg {
        VariantArg::Auto => {
            let mut v = kline_core::retry::DDT2000_DETECTION_ORDER.to_vec();
            v.push(InitVariant::MultiEcu);
            v
        }
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
    if cfg.timing.init == InitKind::Iso8FiveBaud {
        // ISO8 (Safir) has no fast init and no Multipoint.
        return vec![InitVariant::SingleEcu];
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
            ExchangeError::Io(e) => CliError::io_ref(e),
            ExchangeError::Session(e) => CliError::session(e),
        }
    }

    /// True for a missing or broken reply (a scan counts these), false for the transport.
    pub fn is_link(&self) -> bool {
        matches!(self, ExchangeError::Session(SessionError::Link(_)))
    }
}

/// The port preparation of [`Link::prepare_port`] on a bare transport, so `selftest` sets the
/// same line state and writes the same trace line as an init does.
pub fn prepare_port(
    t: &mut dyn Transport,
    trace: &Tracer,
    dtr: kline_core::DtrMode,
    rts: kline_core::RtsMode,
) -> io::Result<()> {
    // DTR first, then RTS (DDT2000's IOTBDSetup order: DTR set, RTS control disabled).
    let d = match dtr {
        kline_core::DtrMode::High => {
            t.set_dtr(true)?;
            "dtr=1"
        }
        kline_core::DtrMode::Low => {
            t.set_dtr(false)?;
            "dtr=0"
        }
        kline_core::DtrMode::Leave => "dtr=left as opened",
    };
    let r = match rts {
        kline_core::RtsMode::Low => {
            t.set_rts(false)?;
            "rts=0"
        }
        kline_core::RtsMode::Leave => "rts=left as opened",
    };
    trace.line(t.now_us(), format!("PORT {d} {r}"));
    Ok(())
}

/// The effective P3, inter-byte and W4 of `cfg` for the init trace line, each marked with the
/// option that set it (final critique F09).
pub fn timing_note(cfg: &LinkConfig, five_baud_kwp: bool) -> String {
    let t = cfg.effective_timing();
    let mark = |o: Option<u32>, flag: &str| {
        if o.is_some() {
            format!(" ({flag})")
        } else {
            String::new()
        }
    };
    let w4 = if five_baud_kwp {
        format!("W4 {} ms{}, ", t.kw2_delay_ms, mark(cfg.w4_ms, "--w4"))
    } else {
        String::new()
    };
    format!(
        "{w4}P3 {} ms{}, inter-byte {} ms{}",
        t.reply_to_request_ms,
        mark(cfg.p3_ms, "--p3"),
        t.inter_byte_ms,
        mark(cfg.inter_byte_ms, "--inter-byte")
    )
}

/// The K line plus its trace and capture.
pub struct Link {
    t: Box<dyn Transport>,
    trace: Tracer,
    cap: Option<Capture>,
    marker: Option<(Kind, Vec<u8>)>,
    /// Frames the transport accepted ([`Link::frames_written`]).
    frames_written: u64,
}

impl Link {
    pub fn new(t: Box<dyn Transport>, trace: Tracer, cap: Option<Capture>) -> Link {
        Link {
            t,
            trace,
            cap,
            marker: None,
            frames_written: 0,
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.t.now_ms()
    }

    /// Number of frames the transport accepted so far. A caller compares it around an
    /// exchange to know whether the request reached the line (final critique F06: a lost reply
    /// after an approved request must say that the request WAS sent).
    pub fn frames_written(&self) -> u64 {
        self.frames_written
    }

    /// Set the modem lines right after the port is opened, before the first init: DTR as
    /// [`kline_core::DtrMode`] says (default high, as the open did before `--dtr`), then RTS:
    /// with [`kline_core::RtsMode::Low`] dropped, in DDT2000's order (IOTBDSetup: DTR set, RTS
    /// control disabled), with `Leave` (default) not touched. Traced as `PORT dtr=.. rts=..`.
    pub fn prepare_port(
        &mut self,
        dtr: kline_core::DtrMode,
        rts: kline_core::RtsMode,
    ) -> io::Result<()> {
        prepare_port(self.t.as_mut(), &self.trace, dtr, rts)
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
                self.frames_written += 1;
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

    /// Run one init with `cfg`. Outer error: transport; inner: the init failed. A wake-up
    /// sleep that woke up beyond its tolerance ([`crate::transport::InitTimingError`]) fails
    /// this attempt with [`LinkError::InitTiming`], so the next variant or round can try again.
    pub fn init(&mut self, cfg: &LinkConfig) -> io::Result<Result<InitOutcome, LinkError>> {
        self.t.set_init_guard(true);
        let r = self.init_guarded(cfg);
        self.t.set_init_guard(false);
        match r {
            Err(e) => match crate::transport::InitTimingError::find(&e) {
                Some(f) => {
                    self.trace
                        .line(self.t.now_us(), format!("INIT aborted: {f}"));
                    Ok(Err(LinkError::InitTiming {
                        requested_ms: f.requested_ms,
                        late_us: f.late_us,
                        max_late_us: f.max_late_us,
                    }))
                }
                None => Err(e),
            },
            ok => ok,
        }
    }

    fn init_guarded(&mut self, cfg: &LinkConfig) -> io::Result<Result<InitOutcome, LinkError>> {
        let now = self.t.now_us();
        match cfg.init_kind() {
            InitKind::KwpFast { .. } => {
                let mut m = match FastInit::new(cfg) {
                    Ok(m) => m,
                    Err(e) => return Ok(Err(e)),
                };
                self.trace.line(
                    now,
                    format!(
                        "INIT fast init + StartCommunication, {}",
                        timing_note(cfg, false)
                    ),
                );
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
                    format!(
                        "INIT 5-baud address byte {:02X}, keyword window {} ms, {}",
                        m.address_byte(),
                        cfg.effective_kw_window_ms(),
                        timing_note(cfg, !matches!(cfg.init_kind(), InitKind::Iso8FiveBaud))
                    ),
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

    /// Send a Tier 0 request; on NRC 0x21 / 0x23 wait and send it again as `policy` says
    /// (DDT2000 RequestByteWithRetry). Returns the last outcome and the number of tries.
    pub fn request_retrying(
        &mut self,
        s: &mut Session,
        payload: &[u8],
        policy: &kline_core::RetryPolicy,
    ) -> Result<(SessionOutcome, u8), ExchangeError> {
        let mut tries = 1u8;
        loop {
            let o = self.request(s, payload)?;
            match &o {
                SessionOutcome::Reply(Reply::Negative { nrc, .. }) => {
                    match policy.nrc_retry_ms(*nrc, tries) {
                        Some(ms) => {
                            self.sleep_ms(ms);
                            tries += 1;
                        }
                        None => return Ok((o, tries)),
                    }
                }
                _ => return Ok((o, tries)),
            }
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
    /// 1-based init round.
    pub round: u8,
    pub address_byte: Option<u8>,
    pub error: Option<String>,
    /// The wait before the next attempt because the ECU answered this failed one (P3max,
    /// final critique F14).
    pub answered_wait_ms: Option<u32>,
    /// The same fast init is tried once more right away because the timing guard aborted
    /// this one (final critique F12).
    pub retry: bool,
}

impl Attempt {
    pub fn to_json(&self) -> Json {
        json!({
            "event": "init_attempt",
            "variant": variant_label(self.variant),
            "round": self.round,
            "address_byte": self.address_byte.map(|b| format!("{b:02X}")),
            "ok": self.error.is_none(),
            "error": self.error,
            "answered_wait_ms": self.answered_wait_ms,
        })
    }
}

/// Whether the ECU sent anything in a failed init attempt (key bytes, ~address, a reply or
/// part of one), so it may still be in a half-open session (final critique F14). False only
/// when nothing came back: silence, no echo, a timing-guard abort before the request, or a
/// refusal before anything was sent. A collision counts as answered (the other talker may be
/// the ECU): waiting too long is the safe side.
pub fn ecu_answered(e: &LinkError) -> bool {
    match e {
        LinkError::NoKeywords { received } => !received.is_empty(),
        LinkError::NoReply
        | LinkError::KLineInactive
        | LinkError::InitTiming { .. }
        | LinkError::Denied(_)
        | LinkError::WrongInitKind
        | LinkError::AliasedAddress { .. }
        | LinkError::PermitMismatch
        | LinkError::UnexpectedEvent => false,
        LinkError::KeywordMismatch { .. }
        | LinkError::NoAddressComplement { .. }
        | LinkError::Collision { .. }
        | LinkError::IncompleteReply { .. }
        | LinkError::Frame(_)
        | LinkError::TooManyPending { .. }
        | LinkError::UnexpectedStartReply(_) => true,
    }
}

/// Sleep `ms` in steps of at most 100 ms, returning early when `stopped()` (Ctrl-C).
fn sleep_unless_stopped(link: &mut Link, ms: u32, stopped: &dyn Fn() -> bool) {
    let mut left = ms;
    while left > 0 && !stopped() {
        let step = left.min(100);
        link.t.sleep_ms(step);
        left -= step;
    }
}

/// The `--kw-window` value the NoKeywords hint suggests.
const KW_WINDOW_HINT_MS: u32 = 700;

/// What the successful init found.
#[derive(Debug, Clone)]
pub struct Connected {
    pub variant: InitVariant,
    pub attempts: Vec<Attempt>,
    pub address_byte: Option<u8>,
    pub sync: Option<u8>,
    /// 5-baud: whether the sync byte was 0x55 (None: fast init). A false value only warns.
    pub sync_ok: Option<bool>,
    /// The warning text for a sync byte other than 0x55.
    pub sync_warning: Option<String>,
    pub keywords: (u8, u8),
    pub address_complement: Option<u8>,
    pub keywords_match: Option<bool>,
    pub kind: InitKind,
    /// 5-baud: whether the start-bit break was seen on RX (None: fast init).
    pub break_loopback: Option<bool>,
}

/// Try `variants` in order until one init succeeds, up to `policy.init_attempts` rounds with
/// `policy.init_retry_ms` between rounds (DDT2000 "Init retry n/3"); `on_attempt` sees each
/// attempt as it ends.
pub fn connect(
    link: &mut Link,
    base: &LinkConfig,
    variants: &[InitVariant],
    policy: &kline_core::RetryPolicy,
    stop: &std::sync::atomic::AtomicBool,
    on_attempt: &mut dyn FnMut(&Attempt),
) -> Result<(Session, Connected), CliError> {
    // Ctrl-C (or SIGTERM/SIGHUP) ends the schedule before the next attempt or retry round,
    // instead of after up to 19 s of attempts (final critique F05).
    let stopped = || stop.load(std::sync::atomic::Ordering::SeqCst);
    let interrupted = || {
        CliError::new(
            crate::error::exit::INIT,
            "init",
            "init stopped by Ctrl-C before the next attempt",
        )
    };
    let mut attempts = Vec::new();
    let rounds = policy.init_attempts.max(1);
    // (round, variant, is the one retry of a fast init after a timing-guard abort)
    let mut schedule: Vec<(u8, InitVariant, bool)> = (1..=rounds)
        .flat_map(|r| variants.iter().map(move |&v| (r, v, false)))
        .collect();
    let mut last_round = 1;
    let mut index = 0;
    while index < schedule.len() {
        let (round, v, is_retry) = schedule[index];
        index += 1;
        if stopped() {
            return Err(interrupted());
        }
        if round != last_round {
            last_round = round;
            link.sleep_ms(policy.init_retry_ms);
            if stopped() {
                return Err(interrupted());
            }
        }
        let mut cfg = base.clone();
        cfg.variant = v;
        let address_byte = (!matches!(cfg.init_kind(), InitKind::KwpFast { .. }))
            .then(|| cfg.five_baud_address_byte());
        let r = link.init(&cfg).map_err(CliError::io)?;
        // F12: a fast init the timing guard aborted (our host woke late; the ECU saw no
        // StartCommunication) is tried once more right away, before the next variant. Only
        // for that cause, and only once per scheduled attempt.
        let retry = !is_retry
            && matches!(r, Err(LinkError::InitTiming { .. }))
            && matches!(cfg.init_kind(), InitKind::KwpFast { .. });
        if retry {
            schedule.insert(index, (round, v, true));
        }
        // F14: the ECU answered this failed attempt, so it may still hold a half-open
        // session; the next init waits P3max for it to time out. A failed attempt never has
        // an open Session here (one exists only after a complete init), so there is no Stop
        // to send instead.
        let answered_wait_ms = match &r {
            Err(e) if ecu_answered(e) && index < schedule.len() => {
                Some(policy.answered_init_wait_ms)
            }
            _ => None,
        };
        let a = Attempt {
            retry,
            answered_wait_ms,
            variant: v,
            round,
            address_byte,
            error: r.as_ref().err().map(|e| {
                let mut t = e.to_string();
                // A widened window is the first thing to try when the ECU may only be slow
                // (W1 near the ISO 9141 maximum of 300 ms plus USB latency).
                if matches!(e, kline_core::LinkError::NoKeywords { .. })
                    && cfg.effective_kw_window_ms() < KW_WINDOW_HINT_MS
                {
                    t.push_str(&format!(" (try --kw-window {KW_WINDOW_HINT_MS})"));
                }
                t
            }),
        };
        if let Some(e) = &a.error {
            link.trace.line(
                link.t.now_us(),
                format!(
                    "INIT attempt {} (round {round}) failed: {e}",
                    variant_label(v)
                ),
            );
        }
        if retry {
            link.trace.line(
                link.t.now_us(),
                "INIT fast init aborted by the timing guard: trying it once more before the next variant",
            );
        }
        on_attempt(&a);
        attempts.push(a);
        if let Some(ms) = answered_wait_ms {
            link.trace.line(
                link.t.now_us(),
                format!("INIT the ECU answered the failed attempt: waiting {ms} ms (P3max) before the next init"),
            );
            sleep_unless_stopped(link, ms, &stopped);
        }
        if let Ok(o) = r {
            if let Some(w) = o.sync_warning() {
                link.trace
                    .line(link.t.now_us(), format!("INIT warning: {w}"));
            }
            let c = Connected {
                variant: v,
                attempts,
                address_byte: o.address_byte(),
                sync: o.sync(),
                sync_ok: o.sync_ok(),
                sync_warning: o.sync_warning(),
                keywords: o.keywords(),
                address_complement: o.address_complement(),
                keywords_match: o.keywords_match(),
                kind: o.kind(),
                break_loopback: o.break_loopback_seen(),
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
        SessionOutcome::Reply(Reply::Iso8Status(code)) => Err(CliError::new(
            crate::error::exit::COMM,
            "comm",
            format!(
                "ECU answered with ISO8 status {code:02X} ({})",
                kline_core::frame::iso8_status_name(*code)
            ),
        )),
        other => Err(CliError::new(
            crate::error::exit::COMM,
            "comm",
            format!("unexpected outcome {other:?}"),
        )),
    }
}

/// KWP2000 negative response code names (ISO 14230-3), plus the codes DDT2000's errors.xml
/// names (`kwpcode`, step 7b FIX-2).
pub fn nrc_name(nrc: u8) -> &'static str {
    match nrc {
        0x10 => "generalReject",
        0x11 => "serviceNotSupported",
        0x12 => "subFunctionNotSupported-invalidFormat",
        0x13 => "incorrectMessageLengthOrInvalidFormat",
        0x21 => "busy-repeatRequest",
        0x22 => "conditionsNotCorrect",
        0x23 => "routineNotComplete",
        0x26 => "failurePreventsExecutionOfRequestedAction",
        0x31 => "requestOutOfRange",
        0x33 => "securityAccessDenied",
        0x35 => "invalidKey",
        0x36 => "exceedNumberOfAttempts",
        0x72 => "generalProgrammingFailure",
        0x78 => "responsePending",
        0x7E => "subFunctionNotSupportedInActiveSession",
        0x7F => "serviceNotSupportedInActiveSession",
        0x80 => "serviceNotSupportedInActiveDiagnosticSession",
        // DDT2000's name (errors.xml E_KWP_87); not an ISO 14230 code.
        0x87 => "errorDuringWriting",
        _ => "unknown",
    }
}

#[cfg(test)]
mod nrc_tests {
    use super::nrc_name;

    /// Step 7b FIX-2: the NRC names DDT2000's errors.xml knows (`kwpcode`).
    #[test]
    fn nrc_names_include_the_ddt2000_codes() {
        for (nrc, name) in [
            (0x13, "incorrectMessageLengthOrInvalidFormat"),
            (0x26, "failurePreventsExecutionOfRequestedAction"),
            (0x72, "generalProgrammingFailure"),
            (0x7E, "subFunctionNotSupportedInActiveSession"),
            (0x7F, "serviceNotSupportedInActiveSession"),
            (0x87, "errorDuringWriting"),
            (0x21, "busy-repeatRequest"),
            (0x78, "responsePending"),
            (0x80, "serviceNotSupportedInActiveDiagnosticSession"),
            (0x99, "unknown"),
        ] {
            assert_eq!(nrc_name(nrc), name, "{nrc:02X}");
        }
    }

    /// Step 7b FIX-3: an ISO8 status reply is reported with its name, not as an unexpected
    /// outcome.
    #[test]
    fn iso8_status_reply_is_named() {
        let e = super::positive(&kline_core::session::SessionOutcome::Reply(
            kline_core::frame::Reply::Iso8Status(0x37),
        ))
        .unwrap_err();
        assert!(e.message.contains("37 (requestUnknown)"), "{}", e.message);
    }
}
