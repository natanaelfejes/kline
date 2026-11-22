//! Tier 0 commands shared by `kline` and `kline-agent`, and the session lifecycle around them.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::Args;
use kline_core::frame::{Framing, KwpHeader, Reply, ReplyNeed};
use kline_core::safety::{self, Protocol, RiskInfo, Target, Tier};
use kline_core::scan::{ScanPlan, ScanResult, ScanService, ScanStep, ScanStop};
use kline_core::session::{Session, SessionOutcome};
use kline_core::InitKind;
use serde_json::{json, Value as Json};

use crate::capture::{self, Capture, Dir, Kind};
use crate::defs::{self, Ecu, EcuArgs};
use crate::error::{exit, CliError};
use crate::link::{self, Connected, Link, SessionArg, VariantArg};
use crate::out::{Out, Sink};
use crate::ports::{self, PortInfo};
use crate::trace::Tracer;
use crate::transport::{Clock, PortSpec, SystemClock, Transport};
use crate::{hex, hex_sp, risk_json, tier_name};

/// Options every command accepts.
#[derive(Debug, Clone, Default, Args)]
pub struct GlobalArgs {
    /// One JSON object per line on stdout.
    #[arg(long, global = true)]
    pub json: bool,
    /// Print every action, TX and RX byte with its time on stderr (also saved next to the capture).
    #[arg(long, global = true)]
    pub trace: bool,
    /// Serial port of the KKL cable (default: the one /dev/cu.usbserial-*, FTDI /dev/ttyUSB* or
    /// FTDI COMn).
    #[arg(long, global = true, value_name = "PATH")]
    pub port: Option<String>,
    /// Simulator control socket: --port is then kline-sim's pty (docs/sim.md).
    #[arg(long, global = true, value_name = "SOCKET")]
    pub sim_control: Option<PathBuf>,
    /// Capture directory (default $KLINE_CAPTURE_DIR, else ~/.local/share/kline/captures;
    /// %APPDATA%\kline\captures on Windows).
    #[arg(long, visible_alias = "captures", global = true, value_name = "DIR")]
    pub capture_dir: Option<PathBuf>,
    /// ECU definitions directory for --ecu (default $KLINE_DDT_XML or ~/.local/share/kline/ecus;
    /// %APPDATA%\kline\ecus on Windows).
    #[arg(long, global = true, value_name = "DIR")]
    pub defs: Option<PathBuf>,
    /// RTS after the port is opened: `leave` it as the OS set it (default) or drop it `low`
    /// like DDT2000 (DTR high first). Whether the KKL cable uses RTS is NOT VERIFIED.
    #[arg(long, global = true, value_enum, default_value_t = RtsArg::Leave)]
    pub rts: RtsArg,
    /// Read window in ms for the `55 KW1 KW2` and ~address reads of a 5-baud init (100..=2000;
    /// default: the definition's KW2Timeout + 100, DDT2000). Reading starts in the middle of
    /// the 100 ms stop-bit wait, so the budget after the stop bit is about this value minus
    /// 100 ms. If connect fails with no keywords received, retry with `--kw-window 700`.
    #[arg(long, global = true, value_name = "MS", value_parser = clap::value_parser!(u32).range(100..=2000))]
    pub kw_window: Option<u32>,
}

/// `--rts` ([`kline_core::RtsMode`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum RtsArg {
    #[default]
    Leave,
    Low,
}

impl RtsArg {
    pub fn mode(self) -> kline_core::RtsMode {
        match self {
            RtsArg::Leave => kline_core::RtsMode::Leave,
            RtsArg::Low => kline_core::RtsMode::Low,
        }
    }
}

impl GlobalArgs {
    pub fn capture_dir(&self) -> PathBuf {
        self.capture_dir
            .clone()
            .or_else(|| std::env::var_os("KLINE_CAPTURE_DIR").map(PathBuf::from))
            .unwrap_or_else(|| defs::data_dir().join("captures"))
    }

    pub fn defs_dir(&self) -> PathBuf {
        self.defs.clone().unwrap_or_else(defs::default_defs_dir)
    }

    /// The port options again, for an exact command line.
    pub fn port_cli(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(p) = &self.port {
            v.extend(["--port".into(), p.clone()]);
        }
        if let Some(c) = &self.sim_control {
            v.extend(["--sim-control".into(), c.display().to_string()]);
        }
        if let Some(d) = &self.defs {
            v.extend(["--defs".into(), d.display().to_string()]);
        }
        if self.rts == RtsArg::Low {
            v.extend(["--rts".into(), "low".into()]);
        }
        if let Some(ms) = self.kw_window {
            v.extend(["--kw-window".into(), ms.to_string()]);
        }
        v
    }
}

#[derive(Debug, Clone, Args)]
pub struct LinkArgs {
    #[command(flatten)]
    pub ecu: EcuArgs,
    /// Init variant (docs/protocol.md "Init variants").
    #[arg(long, value_enum, default_value_t = VariantArg::Auto)]
    pub variant: VariantArg,
    /// Diagnostic session to open after the init.
    #[arg(long, value_enum, default_value_t = SessionArg::Auto)]
    pub session: SessionArg,
    /// Init rounds over all variants (DDT2000: 3, 500 ms apart).
    #[arg(long, default_value_t = kline_core::RetryPolicy::default().init_attempts)]
    pub init_attempts: u8,
    /// Tries of a request answered with NRC 21/23 (DDT2000 identification: 10; 200/500 ms).
    #[arg(long, default_value_t = kline_core::RetryPolicy::default().nrc_tries)]
    pub nrc_tries: u8,
    /// Send TesterPresent every 2000 ms with a required reply (DDT2000 screen option) instead of
    /// after 3000 ms of idle line.
    #[arg(long)]
    pub periodic_tester_present: bool,
    /// Abort a 5-baud init when RX shows nothing during the start-bit break (DDT2000 aborts
    /// on a missing EV_BREAK). Default: warn and go on, since a POSIX tty may not deliver a
    /// byte for a break (docs/protocol.md "5-baud init").
    #[arg(long)]
    pub strict_loopback: bool,
}

impl Default for LinkArgs {
    fn default() -> Self {
        let p = kline_core::RetryPolicy::default();
        LinkArgs {
            ecu: EcuArgs::default(),
            variant: VariantArg::Auto,
            session: SessionArg::Auto,
            init_attempts: p.init_attempts,
            nrc_tries: p.nrc_tries,
            periodic_tester_present: false,
            strict_loopback: false,
        }
    }
}

impl LinkArgs {
    /// The retry policy these options select (DDT2000 defaults otherwise).
    pub fn retry_policy(&self) -> kline_core::RetryPolicy {
        kline_core::RetryPolicy {
            init_attempts: self.init_attempts.max(1),
            nrc_tries: self.nrc_tries.max(1),
            ..kline_core::RetryPolicy::default()
        }
    }

    pub fn to_cli(&self) -> Vec<String> {
        let mut v = self.ecu.to_cli();
        if self.variant != VariantArg::Auto {
            v.extend(["--variant".into(), self.variant.label().into()]);
        }
        if self.strict_loopback {
            v.push("--strict-loopback".into());
        }
        v
    }
}

#[derive(Debug, Clone, Args)]
pub struct ReadArgs {
    #[command(flatten)]
    pub link: LinkArgs,
    /// Request name from the definition, or hex bytes (e.g. 21A9).
    pub request: String,
    /// Set a sent data item of the named request (repeatable): `ITEM=VALUE`, VALUE a number
    /// (physical units), a list label, or `hex:..` raw bytes.
    #[arg(long = "set", value_name = "ITEM=VALUE")]
    pub set: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct LiveArgs {
    #[command(flatten)]
    pub link: LinkArgs,
    /// Comma-separated request names or hex (e.g. 21A9,21AA).
    #[arg(long)]
    pub frames: String,
    /// Cycles per second (all frames once per cycle).
    #[arg(long, default_value_t = 2.0)]
    pub rate: f64,
    /// Stop after this many cycles (default: until Ctrl-C).
    #[arg(long)]
    pub count: Option<u64>,
    /// Show only these decoded items (comma-separated names, case-insensitive), also in JSON.
    #[arg(long, value_name = "NAME[,NAME..]")]
    pub items: Option<String>,
    /// One line per cycle: the --items values, or without --items the values that changed
    /// since the previous cycle (all of them in the first). Human output only.
    #[arg(long)]
    pub compact: bool,
}

#[derive(Debug, Clone, Args)]
pub struct ScanArgs {
    #[command(flatten)]
    pub link: LinkArgs,
    /// Service and identifier range, e.g. 21:B0-C5 or 1A:80-9F (read services only).
    #[arg(long)]
    pub range: String,
    /// Milliseconds between request starts (at least 200, kline-core scan policy).
    #[arg(long, default_value_t = kline_core::scan::MIN_SCAN_INTERVAL_MS)]
    pub interval: u32,
}

#[derive(Debug, Clone, Args)]
pub struct RiskArgs {
    #[command(flatten)]
    pub ecu: EcuArgs,
    /// Request name from the definition, or hex bytes.
    pub request: String,
    /// Set a sent data item of the named request (repeatable), see `read --set`.
    #[arg(long = "set", value_name = "ITEM=VALUE")]
    pub set: Vec<String>,
    #[command(flatten)]
    pub hold: HoldArgs,
}

/// `exec --hold/--watch` (final critique F01): keep the session open after the approved
/// request so its effect (BMT learning window) can be watched in the same session. `risk`
/// takes the same options to print the exact `kline exec` command.
#[derive(Debug, Clone, Default, Args)]
pub struct HoldArgs {
    /// After a positive reply, keep the session open this many seconds (1-60): TesterPresent
    /// at the definition's period and nothing else (except --watch), then Stop. Ctrl-C ends it
    /// early (Stop is sent). The approved request is never sent again.
    #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..=60))]
    pub hold: Option<u64>,
    /// While holding, send only this Tier 0 read (name or hex, e.g. 21A3) every
    /// --watch-interval ms and print the decoded items that changed.
    #[arg(long, value_name = "REQUEST", requires = "hold")]
    pub watch: Option<String>,
    /// Period of the --watch read in ms (at least the definition's P3 gap).
    #[arg(long, value_name = "MS", default_value_t = HOLD_WATCH_INTERVAL_MS, requires = "watch")]
    pub watch_interval: u64,
}

/// Default `--watch-interval`: 2 reads per second, DDT2000's screen refresh order of magnitude
/// (final critique F01 asked for `--rate 2`).
pub const HOLD_WATCH_INTERVAL_MS: u64 = 500;
/// The hold `risk` suggests for the BMT remote learning: the Technical Note's learning window
/// is about 10 s; 30 s leaves time to press the remote and see the result.
pub const REMOTE_LEARNING_HOLD_S: u64 = 30;
/// The read `risk` suggests to watch the remote learning: BMT `21 A3` ("ETATS TIR": TIR RF
/// RECU/VAL/OK, APPTIR, CODIRAPP1/2), docs/cli.md.
pub const REMOTE_LEARNING_WATCH: [u8; 2] = [0x21, 0xA3];

impl HoldArgs {
    /// The options as command words (`--hold N --watch HEX [--watch-interval MS]`), with the
    /// watch read as its resolved hex.
    pub fn to_cli(&self, watch: Option<&defs::Resolved>) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(h) = self.hold {
            v.extend(["--hold".into(), h.to_string()]);
        }
        if let Some(w) = watch {
            v.extend(["--watch".into(), hex(&w.payload)]);
            if self.watch_interval != HOLD_WATCH_INTERVAL_MS {
                v.extend(["--watch-interval".into(), self.watch_interval.to_string()]);
            }
        }
        v
    }

    /// Check `--watch` before anything is opened: it must be a Tier 0 read of this ECU (else
    /// refused, exit 7) and the interval at least the definition's P3 gap (else exit 2).
    pub fn watch_read(&self, ecu: &Ecu) -> Result<Option<defs::Resolved>, CliError> {
        let Some(w) = &self.watch else {
            return Ok(None);
        };
        let r = defs::resolve_request(ecu.def(), w)?;
        let target = ecu.target()?;
        let risk = safety::assess_in(&ecu.guard(), &target, &r.payload)
            .map_err(|d| CliError::denied(&d))?;
        if risk.tier != Tier::Read {
            return Err(CliError::refused(format!(
                "--watch {} is not a Tier 0 read ({}): only a read may be repeated while holding; nothing was sent",
                hex_sp(&r.payload),
                tier_name(risk.tier)
            )));
        }
        // Re-check R13: Tier 0 also holds the session and keep-alive frames (10 81, 10 A0,
        // 3E); repeating those into a learning window is not a read. Only reads may be watched.
        if !is_watch_read(&r.payload) {
            return Err(CliError::usage(format!(
                "--watch {} is not a read: --watch takes only 21 xx or 1A xx (exactly 2 bytes), 17 or 18; nothing was sent",
                hex_sp(&r.payload)
            )));
        }
        let p3 = u64::from(ecu.link_config()?.effective_timing().reply_to_request_ms);
        if self.watch_interval < p3 {
            return Err(CliError::usage(format!(
                "--watch-interval {} ms is below the definition's P3 gap ({p3} ms)",
                self.watch_interval
            )));
        }
        Ok(Some(r))
    }
}

/// The requests `--watch` may repeat (re-check R13): reads by identifier `21 xx` / `1A xx`
/// (exactly 2 bytes) and the fault-memory reads `17` / `18`. Never `10 xx` or `3E`, which are
/// Tier 0 but not reads. The safety tier is checked separately (kline-core).
pub fn is_watch_read(payload: &[u8]) -> bool {
    matches!(payload, [0x21 | 0x1A, _] | [0x17 | 0x18, ..])
}

#[derive(Debug, Clone, Args)]
pub struct ReplayArgs {
    /// A capture (JSONL, kline-sim replay format).
    pub file: PathBuf,
    /// Definition to decode the replies with (optional).
    #[command(flatten)]
    pub ecu: EcuArgs,
}

type Opener = Box<dyn FnMut(&PortSpec, u32, Tracer) -> io::Result<Box<dyn Transport>>>;

/// Everything outside the process logic: I/O sinks, how ports are opened and listed, the
/// Ctrl-C flag. [`Env::system`] for the binaries; tests inject a simulator.
pub struct Env {
    pub stdout: Sink,
    pub stderr: Sink,
    pub opener: Opener,
    pub list_ports: Box<dyn Fn() -> io::Result<Vec<PortInfo>>>,
    pub stop: Arc<AtomicBool>,
    /// The host clock `selftest` measures sleep precision with (real: [`SystemClock`]).
    pub host_clock: Box<dyn Fn() -> Box<dyn Clock>>,
    /// Linux sysfs root for the FTDI latency timer ([`crate::latency::SYS_ROOT`]).
    pub sys_root: PathBuf,
}

impl Env {
    pub fn system() -> Env {
        Env {
            stdout: Sink::stdout(),
            stderr: Sink::stderr(),
            opener: Box::new(crate::transport::open),
            list_ports: Box::new(ports::list),
            stop: Arc::new(AtomicBool::new(false)),
            host_clock: Box::new(|| Box::new(SystemClock::new())),
            sys_root: PathBuf::from(crate::latency::SYS_ROOT),
        }
    }
}

/// One command invocation.
pub struct Ctx<'a> {
    pub g: &'a GlobalArgs,
    pub env: &'a mut Env,
    pub out: Out,
    /// Binary name for messages (`kline`, `kline-agent`).
    pub tool: &'static str,
}

impl<'a> Ctx<'a> {
    pub fn new(g: &'a GlobalArgs, env: &'a mut Env, tool: &'static str) -> Ctx<'a> {
        let out = Out {
            json: g.json,
            stdout: env.stdout.clone(),
            stderr: env.stderr.clone(),
            record: crate::out::Recorder::default(),
        };
        Ctx { g, env, out, tool }
    }

    pub fn stopped(&self) -> bool {
        self.env.stop.load(Ordering::SeqCst)
    }

    /// Report the result and return the exit code.
    pub fn finish(&self, r: Result<(), CliError>) -> u8 {
        let code = self.report(r);
        let done = format!("done (exit {code})");
        if self.out.json {
            // `<capture>.out.txt` ends with it in both modes (code review L4); stdout stays
            // pure JSON and stderr gets no extra line.
            self.out.record.line(&done);
        } else {
            // The last line of every human run, so a terminal log shows how it ended.
            self.out.note(done);
        }
        code
    }

    fn report(&self, r: Result<(), CliError>) -> u8 {
        match r {
            Ok(()) => exit::OK,
            Err(e) => {
                if self.out.json {
                    self.out
                        .record
                        .line(&format!("{}: error: {}", self.tool, e.message));
                    self.out.stdout.line(
                        &json!({"event": "error", "code": e.code, "kind": e.kind, "message": e.message})
                            .to_string(),
                    );
                } else {
                    self.out
                        .note(format!("{}: error: {}", self.tool, e.message));
                }
                e.code
            }
        }
    }

    pub fn port_spec(&self) -> Result<PortSpec, CliError> {
        if let Some(control) = &self.g.sim_control {
            let tty = self
                .g
                .port
                .clone()
                .ok_or_else(|| CliError::usage("--sim-control needs --port <pty>"))?;
            return Ok(PortSpec::Sim {
                tty,
                control: control.clone(),
            });
        }
        if let Some(p) = &self.g.port {
            ports::check_explicit_port(p).map_err(CliError::port)?;
            return Ok(PortSpec::Serial(p.clone()));
        }
        let list = (self.env.list_ports)()
            .map_err(|e| CliError::port(format!("cannot list serial ports: {e}")))?;
        ports::auto_detect(&list)
            .map(PortSpec::Serial)
            .map_err(CliError::port)
    }
}

/// An open diagnostic session.
pub struct Open {
    pub link: Link,
    pub session: Session,
    pub ecu: Ecu,
    pub connected: Connected,
}

impl Open {
    pub fn target(&self) -> Target {
        self.session.target()
    }
}

/// Open the link for `args`, run `body`, then Stop (also after a failed body).
pub fn with_session(
    ctx: &mut Ctx,
    args: &LinkArgs,
    command: &str,
    body: impl FnOnce(&mut Ctx, &mut Open) -> Result<(), CliError>,
) -> Result<(), CliError> {
    let ecu = Ecu::resolve(&args.ecu, &ctx.g.defs_dir())?;
    let mut open = open_session(ctx, ecu, args, command)?;
    let r = body(ctx, &mut open);
    let stop = open.link.stop(&mut open.session);
    let path = open.link.capture().map(|c| c.path().display().to_string());
    let ctrl_c = ctx.stopped();
    ctx.out.emit(
        json!({"event": "stopped", "ok": stop.is_ok(), "capture": path, "ctrl_c": ctrl_c}),
        match (&stop, ctrl_c) {
            (Ok(()), true) => "stopped by Ctrl-C, Stop sent".to_string(),
            (Ok(()), false) => String::new(),
            (Err(e), true) => format!("stopped by Ctrl-C; stop: {}", e.to_cli().message),
            (Err(e), false) => format!("stop: {}", e.to_cli().message),
        },
    );
    r
}

/// Capture, transport, init (with variants) and the diagnostic session.
pub fn open_session(
    ctx: &mut Ctx,
    ecu: Ecu,
    args: &LinkArgs,
    command: &str,
) -> Result<Open, CliError> {
    let mut cfg = ecu.link_config()?;
    if args.periodic_tester_present {
        cfg.tester_present_mode = kline_core::TesterPresentMode::DDT2000_PERIODIC;
    }
    cfg.strict_break_loopback = args.strict_loopback;
    cfg.rts = ctx.g.rts.mode();
    cfg.kw_window_ms = ctx.g.kw_window;
    let policy = args.retry_policy();
    let spec = ctx.port_spec()?;
    let tracer = if ctx.g.trace {
        Tracer::to(ctx.env.stderr.clone())
    } else {
        Tracer::off()
    };
    let cap = Capture::create(&ctx.g.capture_dir(), &ecu.label, command)?;
    let trace_path = cap.path().with_extension("trace.txt");
    tracer
        .add_file(&trace_path)
        .map_err(|e| CliError::new(exit::CAPTURE, "capture", format!("trace file: {e}")))?;
    let out_path = cap.path().with_extension("out.txt");
    ctx.out.record.attach(&out_path).map_err(|e| {
        CliError::new(
            exit::CAPTURE,
            "capture",
            format!("output file {}: {e}", out_path.display()),
        )
    })?;
    ctx.out.emit(
        json!({"event": "capture", "path": cap.path().display().to_string(),
               "trace": tracer.enabled().then(|| trace_path.display().to_string()),
               "out": out_path.display().to_string()}),
        String::new(),
    );
    ctx.out.note(format!("capture: {}", cap.path().display()));
    let port_desc = match &spec {
        PortSpec::Serial(p) => p.clone(),
        PortSpec::Sim { tty, .. } => format!("{tty} (simulator)"),
    };
    let t = (ctx.env.opener)(&spec, cfg.timing.baud, tracer.clone())
        .map_err(|e| CliError::port(format!("cannot open {port_desc}: {e}")))?;
    let mut link = Link::new(t, tracer, Some(cap));
    link.prepare_port(cfg.rts).map_err(CliError::io)?;
    let variants = link::variants(&cfg, args.variant);
    let out = ctx.out.clone();
    // The whole schedule, so a failed attempt can say what comes next.
    let schedule: Vec<(u8, kline_core::InitVariant)> = (1..=policy.init_attempts.max(1))
        .flat_map(|r| variants.iter().map(move |&v| (r, v)))
        .collect();
    let address_of = |v: kline_core::InitVariant| {
        let mut c = cfg.clone();
        c.variant = v;
        (!matches!(c.init_kind(), InitKind::KwpFast { .. })).then(|| c.five_baud_address_byte())
    };
    let what = |b: Option<u8>| {
        b.map(|b| format!("byte {b:02X}"))
            .unwrap_or_else(|| "fast init".into())
    };
    let stop = ctx.env.stop.clone();
    let (mut session, connected) = link::connect(
        &mut link,
        &cfg,
        &variants,
        &policy,
        &stop,
        &mut |a| {
            let retry = if a.round > 1 {
                format!(
                    "retry {}/{} ",
                    a.round - 1,
                    policy.init_attempts.saturating_sub(1)
                )
            } else {
                String::new()
            };
            let next = schedule
                .iter()
                .position(|&(r, v)| r == a.round && v == a.variant)
                .and_then(|i| schedule.get(i + 1));
            let head = format!("init {:<6} {retry}", link::variant_label(a.variant));
            let human = match &a.error {
                None => format!("{head}{}: ok", what(a.address_byte)),
                // Not the last attempt: information, not an error (the next variant may work).
                Some(e) => {
                    let then = match next {
                        Some(&(r, v)) if r == a.round => match address_of(v) {
                            Some(b) => format!("trying 5-baud byte {b:02X}"),
                            None => "trying fast init".into(),
                        },
                        Some(&(r, _)) => format!(
                            "next round {r}/{} in {} ms",
                            policy.init_attempts, policy.init_retry_ms
                        ),
                        None => String::new(),
                    };
                    if next.is_none() {
                        format!("{head}{}: {e}", what(a.address_byte))
                    } else if a.address_byte.is_none()
                        && *e == kline_core::LinkError::NoReply.to_string()
                    {
                        format!("{head}fast init not answered (normal for many Renault K-line ECUs), {then}")
                    } else {
                        format!("{head}{}: {e}, {then}", what(a.address_byte))
                    }
                }
            };
            out.emit(a.to_json(), human)
        },
    )?;
    if connected.break_loopback == Some(false) {
        ctx.out.emit(
            json!({"event": "warning", "kind": "break_loopback_not_seen",
                   "message": "RX showed nothing during the 5-baud start bit; the init went on and the ECU answered (pass --strict-loopback to abort instead)"}),
            "warning: break loopback not seen on RX during the start bit; init went on (--strict-loopback aborts)",
        );
    }
    if let Some(w) = connected.sync_warning.as_deref() {
        ctx.out.emit(
            json!({"event": "warning", "kind": "sync_byte", "message": w}),
            format!("warning: {w}"),
        );
    }
    // SAFETY.md rule 3: the core applies the definition's class and immobilizer names to every
    // request of this session.
    session.set_definition_guard(ecu.guard());
    let t = session.target();
    ctx.out.emit(
        json!({
            "event": "connected",
            "ecu": ecu.label,
            "definition": ecu.primary().path.display().to_string(),
            "port": port_desc,
            "variant": link::variant_label(connected.variant),
            "init": init_kind_name(connected.kind),
            "protocol": proto_name(t.protocol),
            "ecu_address": format!("{:02X}", t.ecu_address),
            "address_byte": connected.address_byte.map(|b| format!("{b:02X}")),
            "sync": connected.sync.map(|b| format!("{b:02X}")),
            "sync_ok": connected.sync_ok,
            "kw1": format!("{:02X}", connected.keywords.0),
            "kw2": format!("{:02X}", connected.keywords.1),
            "address_complement": connected.address_complement.map(|b| format!("{b:02X}")),
            "keywords_match": connected.keywords_match,
            "vehicle": ecu.vehicle_info(),
        }),
        format!(
            "connected: {} at {:02X} ({}, {}) with variant {}; sync {} KW {:02X} {:02X}{}{}",
            ecu.label,
            t.ecu_address,
            proto_name(t.protocol),
            init_kind_name(connected.kind),
            link::variant_label(connected.variant),
            connected
                .sync
                .map(|b| format!("{b:02X}"))
                .unwrap_or_else(|| "-".into()),
            connected.keywords.0,
            connected.keywords.1,
            connected
                .address_complement
                .map(|b| format!(", ~address {b:02X}"))
                .unwrap_or_default(),
            match connected.keywords_match {
                Some(true) => ", key bytes match the definition",
                Some(false) => ", KEY BYTES DIFFER from the definition",
                None => "",
            }
        ),
    );
    if let Some(p) = start_session_payload(ecu.def(), t, args.session) {
        let r = link.request(&mut session, &p);
        let (ok, text) = match &r {
            Ok(o) => match link::positive(o) {
                Ok(reply) => (true, hex(reply)),
                Err(e) => (false, e.message),
            },
            Err(e) => (false, e.to_cli().message),
        };
        if let Err(link::ExchangeError::Io(e)) = r {
            return Err(CliError::io(e));
        }
        // Definition-driven: a refused 10 A0 is reported, never retried with another session
        // (10 C0, seen in other Renault logs, is an Expert request that needs a human
        // approval, never sent automatically; 10 81 is not tried either, so the ECU stays in
        // the session it chose).
        let note = (!ok).then_some("not retried with another session");
        ctx.out.emit(
            json!({"event": "session_start", "request": hex(&p), "ok": ok, "reply": text,
                   "note": note}),
            format!(
                "session {}: {}{text}{}",
                hex_sp(&p),
                if ok { "ok " } else { "FAILED, continuing: " },
                if ok {
                    ""
                } else {
                    " (not retried with another session)"
                }
            ),
        );
    }
    Ok(Open {
        link,
        session,
        ecu,
        connected,
    })
}

fn start_session_payload(def: &kline_spec::EcuDef, t: Target, s: SessionArg) -> Option<Vec<u8>> {
    if t.protocol != Protocol::Kwp {
        return None;
    }
    let a0 = vec![0x10, safety::RENAULT_DIAG_SESSION];
    let d81 = vec![0x10, safety::DEFAULT_SESSION];
    match s {
        SessionArg::None => None,
        SessionArg::A0 => Some(a0),
        SessionArg::S81 => Some(d81),
        SessionArg::Auto => [a0, d81]
            .into_iter()
            .find(|p| !defs::requests_for(def, p).is_empty()),
    }
}

pub fn proto_name(p: Protocol) -> &'static str {
    match p {
        Protocol::Iso8 => "iso8",
        Protocol::Kwp => "kwp",
    }
}

pub fn init_kind_name(k: InitKind) -> &'static str {
    match k {
        InitKind::Iso8FiveBaud => "iso8 5-baud",
        InitKind::KwpFiveBaud { multipoint: false } => "kwp 5-baud",
        InitKind::KwpFiveBaud { multipoint: true } => "kwp 5-baud multipoint",
        InitKind::KwpFast { .. } => "kwp fast init",
    }
}

// ---- commands ----

pub fn ports(ctx: &mut Ctx) -> Result<(), CliError> {
    let list = (ctx.env.list_ports)()
        .map_err(|e| CliError::port(format!("cannot list serial ports: {e}")))?;
    for p in &list {
        let twin = ports::callout_twin(p, &list);
        ctx.out.emit(
            {
                let mut j = p.to_json();
                j["event"] = json!("port");
                j["use_instead"] = json!(twin);
                j
            },
            format!(
                "{} {}{}{}",
                if ports::is_candidate(p) { "*" } else { " " },
                p.path,
                p.usb
                    .map(|u| format!(
                        "  usb {:04x}:{:04x} {}",
                        u.0,
                        u.1,
                        p.product.clone().unwrap_or_default()
                    ))
                    .unwrap_or_default(),
                twin.as_ref()
                    .map(|c| format!("  (not used, use {c})"))
                    .unwrap_or_default()
            ),
        );
    }
    let detected = ports::auto_detect(&list);
    ctx.out.emit(
        json!({"event": "ports", "count": list.len(), "detected": detected.as_ref().ok(), "problem": detected.as_ref().err()}),
        match &detected {
            Ok(p) => format!("cable: {p}"),
            Err(e) => e.clone(),
        },
    );
    Ok(())
}

#[derive(Debug, Clone, Default, Args)]
pub struct SelftestArgs {
    /// Also write a single FF byte 10 times and time its echo (USB latency vs the ~KW2
    /// handshake). Needs a looped-back K line: the cable on the car's OBD socket (no init is
    /// sent) or TX wired to RX.
    #[arg(long)]
    pub echo_rtt: bool,
    /// Linux only: write the FTDI latency timer (ms, 1..255) of the ttyUSB port before
    /// measuring (`/sys/bus/usb-serial/devices/ttyUSBn/latency_timer`; needs root or the udev
    /// rule in docs/platforms.md). On Windows set it in Device Manager (FTDI VCP driver).
    #[arg(long, value_name = "MS", value_parser = clap::value_parser!(u8).range(1..=255))]
    pub set_latency: Option<u8>,
}

/// The host-timing part of `selftest`, before any port is opened: sleep precision of the clock
/// the transport uses (crate::selftest::measure_sleep_jitter).
fn selftest_timing(ctx: &mut Ctx) {
    let mut clock = (ctx.env.host_clock)();
    let res = SystemClock::new();
    let tick_1ms = res.timer_resolution_active();
    let w = clock.now_us() + u64::from(crate::selftest::SLEEP_WARMUP_MS) * 1000;
    clock.sleep_until_us(w);
    let js: Vec<crate::selftest::SleepJitter> = crate::selftest::SLEEP_TARGETS
        .iter()
        .map(|&(ms, tol)| {
            crate::selftest::measure_sleep_jitter(
                clock.as_mut(),
                ms,
                crate::selftest::SLEEP_REPS,
                tol,
            )
        })
        .collect();
    drop(res);
    let ok = js.iter().all(|j| j.ok);
    let tail = clock.spin_tail_us();
    let os_late = clock.os_oversleep_max_us();
    let mut human = format!(
        "host timing ({}, spin tail {} us, OS sleep up to {} us late{}):",
        std::env::consts::OS,
        tail.unwrap_or(crate::hrtimer::SPIN_TAIL_US),
        os_late.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
        if cfg!(windows) {
            if tick_1ms {
                ", timeBeginPeriod(1) granted"
            } else {
                ", timeBeginPeriod(1) REFUSED"
            }
        } else {
            ""
        }
    );
    for j in &js {
        human.push_str(&format!(
            "\n  sleep {:>3} ms x{}: late min {:.3} / mean {:.3} / max {:.3} ms, early {} (limit {:.0} ms): {}",
            j.target_ms,
            j.samples,
            j.min_late_us as f64 / 1000.0,
            j.mean_late_us as f64 / 1000.0,
            j.max_late_us as f64 / 1000.0,
            j.early,
            j.tolerance_us as f64 / 1000.0,
            if j.ok { "OK" } else { "TOO IMPRECISE" }
        ));
    }
    ctx.out.emit(
        json!({"event": "selftest_timing", "os": std::env::consts::OS,
               "spin_tail_base_us": crate::hrtimer::SPIN_TAIL_US,
               "spin_tail_us": tail, "os_oversleep_max_us": os_late,
               "timer_1ms_tick": tick_1ms, "sleep_jitter": js, "ok": ok}),
        human,
    );
}

/// Linux: the ttyUSB port's FTDI latency timer, written first if `set` (crate::latency).
/// `Ok(None)` elsewhere or for a port that is not a ttyUSB.
fn selftest_latency(
    ctx: &Ctx,
    path: &str,
    set: Option<u8>,
) -> Result<Option<(PathBuf, Option<u8>)>, CliError> {
    if !cfg!(target_os = "linux") {
        return match set {
            Some(_) => Err(CliError::usage(
                "--set-latency is Linux-only (ftdi_sio sysfs); on Windows set Latency Timer in Device Manager > Port > Advanced (FTDI VCP), on macOS AppleUSBFTDI has no setting (docs/platforms.md)",
            )),
            None => Ok(None),
        };
    }
    let Some(p) = crate::latency::sysfs_path(path, &ctx.env.sys_root) else {
        return match set {
            Some(_) => Err(CliError::port(format!(
                "--set-latency: {path} is not a ttyUSB (ftdi_sio) port"
            ))),
            None => Ok(None),
        };
    };
    if let Some(ms) = set {
        crate::latency::write(&p, ms).map_err(|e| {
            CliError::port(format!(
                "cannot write {}: {e}; {}",
                p.display(),
                crate::latency::write_hint(&p, ms)
            ))
        })?;
    }
    let v = crate::latency::read(&p).ok();
    Ok(Some((p, v)))
}

/// `selftest`: port at 10400, what the driver reports, break-toggle jitter, optionally the echo
/// round trip. Without `--echo-rtt` nothing is written, only 10 ms break pulses
/// (crate::selftest).
pub fn selftest(ctx: &mut Ctx, a: &SelftestArgs) -> Result<(), CliError> {
    // Host timing first: it needs no cable, so it is reported even when no port opens.
    selftest_timing(ctx);
    let spec = ctx.port_spec()?;
    let path = match &spec {
        PortSpec::Serial(p) => p.clone(),
        PortSpec::Sim { tty, .. } => format!("{tty} (simulator)"),
    };
    let latency = match &spec {
        PortSpec::Serial(p) => selftest_latency(ctx, p, a.set_latency)?,
        PortSpec::Sim { .. } => None,
    };
    let latency_json = latency
        .as_ref()
        .map(|(p, v)| json!({"path": p.display().to_string(), "ms": v, "set": a.set_latency}));
    let latency_human = match &latency {
        Some((p, Some(v))) => format!("\nFTDI latency timer: {v} ms ({})", p.display()),
        Some((p, None)) => format!("\nFTDI latency timer: unreadable ({})", p.display()),
        None => String::new(),
    };
    let usb = (ctx.env.list_ports)()
        .ok()
        .and_then(|l| l.into_iter().find(|p| p.path == path));
    let baud = kline_core::timing::consts::DEFAULT_BAUD;
    let mut t = (ctx.env.opener)(&spec, baud, Tracer::off())
        .map_err(|e| CliError::port(format!("cannot open {path} at {baud} baud: {e}")))?;
    // The same line state and trace line as before an init (Link::prepare_port).
    let trace = if ctx.g.trace {
        Tracer::to(ctx.env.stderr.clone())
    } else {
        Tracer::off()
    };
    crate::link::prepare_port(t.as_mut(), &trace, ctx.g.rts.mode()).map_err(CliError::io)?;
    let reported = t.baud_rate();
    let j = crate::selftest::measure_break_jitter(
        t.as_mut(),
        crate::selftest::TOGGLES,
        crate::selftest::TOGGLE_HALF_MS,
    )
    .map_err(CliError::io)?;
    let echo = if a.echo_rtt {
        Some(
            crate::selftest::measure_echo_rtt(t.as_mut(), crate::selftest::ECHO_COUNT)
                .map_err(CliError::io)?,
        )
    } else {
        None
    };
    let baud_source = if cfg!(any(target_os = "macos", target_os = "ios")) {
        "termios output speed read back with tcgetattr after IOSSIOSPEED; the tty layer's record, not the FTDI divisor"
    } else {
        "termios speed read back from the driver"
    };
    let note = if echo.is_some() {
        "echo RTT includes ~1 ms byte time and the TX and RX USB latency; the ~KW2 answer of a 5-baud init (sent 25 ms after KW2, W4 25..50 ms) needs KW2 to arrive well under 25 ms late. NOT VERIFIED on the car."
    } else {
        "FTDI latency timer not measured (pass --echo-rtt on a looped-back K line): macOS AppleUSBFTDI is said to default to ~16 ms (DDT2000/Renolink users set 1 ms on Windows); bytes may then reach us up to that late. RequestToReply (300 ms) absorbs it; the ~KW2 answer of a 5-baud init (sent 25 ms after KW2, W4 25..50 ms) is the tight spot. NOT VERIFIED on the car."
    };
    let echo_human = match &echo {
        None => String::new(),
        Some(e) if e.write_error.is_some() => format!(
            "\necho RTT: not measured: write failed after {} of {} bytes: {}; ~KW2 handshake timing: not measured (the cable did not take the echo byte; replug it and check `kline ports`)",
            e.echoed,
            e.sent,
            e.write_error.as_deref().unwrap_or_default()
        ),
        Some(e) => format!(
            "\necho RTT: {}/{} bytes back{}; ~KW2 handshake timing: {}",
            e.echoed,
            e.sent,
            match (e.min_us, e.mean_us, e.max_us) {
                (Some(lo), Some(m), Some(hi)) => format!(
                    ", min {:.2} ms, mean {:.2} ms, max {:.2} ms",
                    lo as f64 / 1000.0,
                    m as f64 / 1000.0,
                    hi as f64 / 1000.0
                ),
                _ => " (no loopback: cable not on a powered K line?)".into(),
            },
            if e.kw2_handshake_ok {
                "OK"
            } else if e.echoed == 0 {
                "not measured (no echo)"
            } else {
                "AT RISK"
            }
        ),
    };
    // Timing verdicts only mean something on the real cable: a pty or the simulator has no
    // USB round trip to blame (rehearsal item 12).
    let kkl = match &spec {
        PortSpec::Sim { .. } => false,
        PortSpec::Serial(_) => usb.as_ref().is_some_and(|u| {
            ports::is_candidate(u) || u.usb.is_some_and(|v| v.0 == ports::FTDI_VID)
        }),
    };
    // A USB serial adapter of another make (CH340, PL2303) can be a KKL cable too: its timing
    // verdicts are about this host and adapter and keep their meaning (code review L8). Only a
    // port without a USB id (simulator, pty, built-in port) has none.
    let usb_id = usb.as_ref().and_then(|u| u.usb);
    let meaningful = match &spec {
        PortSpec::Sim { .. } => false,
        PortSpec::Serial(_) => kkl || usb_id.is_some(),
    };
    let not_kkl_human = match (kkl, meaningful, &spec) {
        (true, _, _) => String::new(),
        (false, _, PortSpec::Sim { .. }) => "\nthis port is not a known FTDI KKL cable (simulator/pty): timing verdicts below are not meaningful".to_string(),
        (false, true, _) => format!(
            "\nthis port is not a known FTDI KKL cable (usb {:04x}:{:04x}): the verdicts below are for this host and adapter; whether it drives a K line like the FTDI cable is NOT VERIFIED",
            usb_id.map(|v| v.0).unwrap_or_default(),
            usb_id.map(|v| v.1).unwrap_or_default()
        ),
        (false, false, _) => "\nthis port is not a known FTDI KKL cable (no USB id: simulator, pty or a built-in port): timing verdicts below are not meaningful".to_string(),
    };
    let fast_init_cause = crate::selftest::fast_init_cause(&j).filter(|_| meaningful);
    let fast_init_human = fast_init_cause
        .as_ref()
        .map(|c| format!(": {c}"))
        .unwrap_or_default();
    ctx.out.emit(
        json!({"event": "selftest", "port": path,
               "fast_init_cause": fast_init_cause,
               "kkl_cable": kkl, "timing_meaningful": meaningful,
               "usb": usb.as_ref().map(|u| u.to_json()),
               "baud_requested": baud, "baud_reported": reported,
               "baud_reported_source": baud_source,
               "break_jitter": j, "echo_rtt": echo, "latency_timer": latency_json,
               "note": note}),
        format!(
            "port: {path}{}{not_kkl_human}\nbaud: requested {baud}, driver reports {} ({baud_source})\nbreak toggles: {} edges, {} ms half-period: edge max deviation {:.2} ms, mean {:.2} ms, slowest set_break {:.2} ms, pulse length max deviation {:.2} ms; loopback {}\n5-baud bits (+-10 ms, edges): {}; fast-init TiniL (+-1 ms, pulse length): {}{fast_init_human}{echo_human}{latency_human}\n{note}",
            usb.as_ref()
                .and_then(|u| u.usb.map(|v| format!(" (usb {:04x}:{:04x} {})", v.0, v.1, u.product.clone().unwrap_or_default())))
                .unwrap_or_default(),
            reported.map(|b| b.to_string()).unwrap_or_else(|| "nothing".into()),
            j.edges,
            j.half_ms,
            j.max_dev_us as f64 / 1000.0,
            j.mean_dev_us as f64 / 1000.0,
            j.max_call_us as f64 / 1000.0,
            j.max_pulse_dev_us as f64 / 1000.0,
            if j.loopback_seen { "seen" } else { "not seen (normal without K-line power)" },
            if j.five_baud_ok { "OK" } else { "TOO IMPRECISE" },
            if j.fast_init_ok { "OK" } else { "TOO IMPRECISE" },
        ),
    );
    Ok(())
}

#[derive(Debug, Clone, Args)]
pub struct SetupArgs {
    /// Folder with the DDT2000 ECU definition files (*.xml) to copy into the definitions
    /// directory (--defs, else $KLINE_DDT_XML, else the default).
    #[arg(long, value_name = "DIR")]
    pub defs_from: PathBuf,
}

/// Why `setup` will not copy into `dest`, inside the git work tree `repo`, and where to copy
/// instead. When `dest` is the default directory itself (e.g. a dotfiles repository at `~`),
/// the default is not suggested (code review L7).
fn setup_refusal(dest: &std::path::Path, repo: &std::path::Path) -> String {
    let default = defs::data_dir().join("ecus");
    let same = |a: &std::path::Path, b: &std::path::Path| {
        a == b
            || (std::fs::canonicalize(a).ok().is_some()
                && std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok())
    };
    let hint = if same(dest, &default) {
        format!(
            "the default definitions directory {} is itself in that repository (a repository at your home folder?); choose a folder outside any repository with --defs DIR or KLINE_DDT_XML=DIR",
            default.display()
        )
    } else {
        format!(
            "use the default directory {} or another --defs outside any repository",
            default.display()
        )
    };
    format!(
        "refusing to copy Renault definitions into {}: it is inside the git work tree {} (SAFETY.md rule 1); {hint}",
        dest.display(),
        repo.display()
    )
}

/// `setup --defs-from DIR`: copy `DIR/*.xml` into the definitions directory `--ecu` reads,
/// never into a git work tree (SAFETY.md rule 1: Renault data never enters git). Opens no port.
pub fn setup(ctx: &mut Ctx, a: &SetupArgs) -> Result<(), CliError> {
    let dest = ctx.g.defs_dir();
    let src = &a.defs_from;
    let rd = std::fs::read_dir(src).map_err(|e| {
        CliError::definition(format!("cannot read --defs-from {}: {e}", src.display()))
    })?;
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("xml")))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(CliError::definition(format!(
            "{} has no .xml files (DDT2000 ECU definitions, e.g. {})",
            src.display(),
            defs::car_files()[0].1
        )));
    }
    if let Some(repo) = defs::git_work_tree(&dest) {
        return Err(CliError::definition(setup_refusal(&dest, &repo)));
    }
    std::fs::create_dir_all(&dest)
        .map_err(|e| CliError::definition(format!("cannot create {}: {e}", dest.display())))?;
    let mut copied = Vec::new();
    for f in &files {
        let name = f
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let to = dest.join(&name);
        if std::fs::canonicalize(f).ok() != std::fs::canonicalize(&to).ok() || !to.exists() {
            std::fs::copy(f, &to).map_err(|e| {
                CliError::definition(format!(
                    "cannot copy {} to {}: {e}",
                    f.display(),
                    to.display()
                ))
            })?;
        }
        copied.push(name);
    }
    let ecus: Vec<(&str, &str, bool)> = defs::car_files()
        .into_iter()
        .map(|(l, f)| (l, f, dest.join(f).exists()))
        .collect();
    let mut human = format!(
        "copied {} definition file(s) from {} to {}",
        copied.len(),
        src.display(),
        dest.display()
    );
    for (l, f, ok) in &ecus {
        human.push_str(&format!(
            "
--ecu {l:<9} {f}: {}",
            if *ok { "present" } else { "MISSING" }
        ));
    }
    let map: serde_json::Map<String, Json> = ecus
        .iter()
        .map(|(l, _, ok)| (l.to_string(), json!(ok)))
        .collect();
    ctx.out.emit(
        json!({"event": "setup", "from": src.display().to_string(),
               "to": dest.display().to_string(), "copied": copied, "ecus": map}),
        human,
    );
    Ok(())
}

pub fn connect(ctx: &mut Ctx, a: &LinkArgs) -> Result<(), CliError> {
    with_session(ctx, a, "connect", |_, _| Ok(()))
}

/// Read and decode one request; returns the positive payload.
fn read_one(
    ctx: &mut Ctx,
    o: &mut Open,
    payload: &[u8],
    names: &[String],
) -> Result<Vec<u8>, CliError> {
    let mut t_ms = o.link.now_ms();
    let r = match o.link.request(&mut o.session, payload) {
        // One retry after a damaged reply (checksum, short frame), like a live cycle: noise
        // on the line should not end a one-shot read (rehearsal item 14).
        Err(e) if is_frame_error(&e) => {
            let wait = kline_core::RetryPolicy::default().cycle_retry_ms;
            let m = e.to_cli().message;
            ctx.out.emit(
                json!({"event": "read_retry", "request": hex(payload), "message": m, "wait_ms": wait}),
                format!("{}: {m}; retrying once in {wait} ms", hex_sp(payload)),
            );
            // Ctrl-C during the wait: the retry is not sent (code review L3).
            if !idle(ctx, o, u64::from(wait))? {
                return Err(e.to_cli());
            }
            t_ms = o.link.now_ms();
            o.link.request(&mut o.session, payload)
        }
        r => r,
    }
    .map_err(|e| e.to_cli())?;
    let reply = match link::positive(&r) {
        Ok(p) => p.to_vec(),
        Err(e) => {
            if let SessionOutcome::Reply(Reply::Negative { sid, nrc }) = r {
                ctx.out.emit(
                    json!({"event": "read", "t_ms": t_ms, "request": hex(payload), "names": names,
                           "negative": {"sid": format!("{sid:02X}"), "nrc": format!("{nrc:02X}"), "name": link::nrc_name(nrc)}}),
                    String::new(),
                );
            }
            return Err(e);
        }
    };
    let decoded = defs::decode_reply(o.ecu.def(), payload, &reply);
    let (alts, alt_human) = alternatives(o, payload, &reply);
    let mut j = json!({"event": "read", "t_ms": t_ms, "request": hex(payload), "names": names,
               "reply": hex(&reply), "decoded": defs::decoded_json(&decoded)});
    if o.ecu.defs.len() > 1 {
        j["alternatives"] = json!(alts);
    }
    ctx.out.emit(
        j,
        format!(
            "{} -> {}\n{}\n{alt_human}",
            hex_sp(payload),
            hex_sp(&reply),
            defs::decoded_human(&decoded)
        )
        .trim_end(),
    );
    if !decoded.is_empty() && decoded.iter().all(|(_, r)| r.is_err()) {
        return Err(CliError::new(
            exit::DECODE,
            "decode",
            "the reply does not decode with the definition",
        ));
    }
    Ok(reply)
}

/// With several candidate definitions loaded (`--ecu bmt` while `BMT_SAGEM.xml` is present):
/// say once that the choice is open and which one decodes the values.
fn definition_choice_notice(ctx: &Ctx, o: &Open) {
    if o.ecu.defs.len() < 2 {
        return;
    }
    let names: Vec<String> = o.ecu.defs.iter().map(defs::def_stem).collect();
    ctx.out.emit(
        json!({"event": "definition_choice", "unresolved": true, "candidates": names,
               "decoded_with": names[0]}),
        format!(
            "definition choice unresolved: {}; values below decoded with {} (fine for reads; to choose: {})",
            names.join(", "),
            names[0],
            choose_hint(&o.ecu)
        ),
    );
}

/// `name display unit` of a decoded item, for one-line lists.
fn item_text(i: &kline_spec::DecodedItem) -> String {
    let mut t = format!("{} {}", i.name, i.display.as_deref().unwrap_or("-"));
    if let Some(u) = i.unit.as_deref().filter(|u| !u.is_empty()) {
        t.push(' ');
        t.push_str(u);
    }
    t
}

/// The other candidates' decodings of `reply` where they differ from the first one's: the items
/// (name and shown value) that are not the same in both. JSON per other candidate, and the
/// human lines (empty when every candidate decodes it alike).
fn alternatives(o: &Open, payload: &[u8], reply: &[u8]) -> (Vec<Json>, String) {
    if o.ecu.defs.len() < 2 {
        return (Vec::new(), String::new());
    }
    let items = |def: &kline_spec::EcuDef| -> Vec<kline_spec::DecodedItem> {
        defs::decode_reply(def, payload, reply)
            .into_iter()
            .filter_map(|(_, r)| r.ok())
            .flatten()
            .collect()
    };
    let key = |i: &kline_spec::DecodedItem| (i.name.trim().to_string(), i.display.clone());
    let primary = items(o.ecu.def());
    let mut json_out = Vec::new();
    let mut lines = Vec::new();
    let mut primary_diff: Vec<&kline_spec::DecodedItem> = Vec::new();
    for d in &o.ecu.defs[1..] {
        let other = items(&d.def);
        if other.is_empty() {
            continue;
        }
        let pk: Vec<_> = primary.iter().map(key).collect();
        let ok: Vec<_> = other.iter().map(key).collect();
        let theirs: Vec<&kline_spec::DecodedItem> =
            other.iter().filter(|i| !pk.contains(&key(i))).collect();
        for i in primary.iter().filter(|i| !ok.contains(&key(i))) {
            if !primary_diff.iter().any(|p| key(p) == key(i)) {
                primary_diff.push(i);
            }
        }
        if theirs.is_empty() && primary_diff.is_empty() {
            continue;
        }
        json_out.push(json!({"definition": defs::def_stem(d),
                             "differing": theirs.iter().map(|i| defs::item_json(i)).collect::<Vec<_>>()}));
        lines.push(format!(
            "    {}: {}",
            defs::def_stem(d),
            if theirs.is_empty() {
                "-".to_string()
            } else {
                theirs
                    .iter()
                    .map(|i| item_text(i))
                    .collect::<Vec<_>>()
                    .join("; ")
            }
        ));
    }
    if lines.is_empty() {
        return (json_out, String::new());
    }
    let mut human = format!(
        "  decoded differently by the candidates:\n    {}: {}",
        defs::def_stem(o.ecu.primary()),
        if primary_diff.is_empty() {
            "-".to_string()
        } else {
            primary_diff
                .iter()
                .map(|i| item_text(i))
                .collect::<Vec<_>>()
                .join("; ")
        }
    );
    for l in lines {
        human.push('\n');
        human.push_str(&l);
    }
    (json_out, human)
}

/// A reply that arrived damaged: bad checksum or other frame error, or cut short.
fn is_frame_error(e: &link::ExchangeError) -> bool {
    matches!(
        e,
        link::ExchangeError::Session(kline_core::session::SessionError::Link(
            kline_core::LinkError::Frame(_) | kline_core::LinkError::IncompleteReply { .. }
        ))
    )
}

/// Refuse a non-read before any init (kline-core refuses again when it is sent).
fn precheck_read(ecu: &Ecu, payload: &[u8]) -> Result<(), CliError> {
    let t = ecu.target()?;
    safety::check(&t, payload)
        .map(drop)
        .map_err(|d| CliError::denied(&d))
}

pub fn read(ctx: &mut Ctx, a: &ReadArgs) -> Result<(), CliError> {
    let ecu = Ecu::resolve(&a.link.ecu, &ctx.g.defs_dir())?;
    let r = defs::resolve_request_with(ecu.def(), &a.request, &a.set)?;
    precheck_read(&ecu, &r.payload)?;
    with_session(ctx, &a.link, "read", |ctx, o| {
        definition_choice_notice(ctx, o);
        read_one(ctx, o, &r.payload, &r.names).map(drop)
    })
}

/// DiagFailures' generic identification wants exactly this many bytes for `21 80`
/// (RE: DiagFailures 0x2403ee70, "need 26 bytes, receive n"); Sirius 32 declares MinBytes 24.
pub const DIAGFAILURES_IDENT_BYTES: usize = 26;

/// Decode `reply` with `def`; a reply shorter than MinBytes is decoded anyway (items past the
/// end are `None`) and flagged, since one MinBytes covers many software versions.
type Decoded = Vec<(
    String,
    Result<Vec<kline_spec::DecodedItem>, kline_spec::SpecError>,
)>;

fn decode_ident(def: &kline_spec::EcuDef, p: &[u8], reply: &[u8]) -> (Decoded, bool) {
    let checked = defs::decode_reply(def, p, reply);
    let too_short = !checked.is_empty()
        && checked
            .iter()
            .all(|(_, r)| matches!(r, Err(kline_spec::SpecError::TooShort { .. })));
    if !too_short {
        return (checked, false);
    }
    let lenient = defs::requests_for(def, p)
        .into_iter()
        .map(|r| (r.name.clone(), r.decode_checked_allow_short(def, reply)))
        .collect();
    (lenient, true)
}

/// `ident`: `21 80` with DDT2000's NRC 21/23 retry, then the definition: DDT2000's AutoIdent
/// ladder when the candidates have `<AutoIdents>` and the reply has the Renault layout, else the
/// first candidate whose key bytes match and whose identification requests decode.
pub fn ident(ctx: &mut Ctx, a: &LinkArgs) -> Result<(), CliError> {
    let policy = a.retry_policy();
    with_session(ctx, a, "ident", |ctx, o| {
        let t = o.target();
        let reqs = defs::ident_requests(o.ecu.def(), &t);
        let mut payloads: Vec<Vec<u8>> = Vec::new();
        for r in reqs {
            if !payloads.contains(&r.sent_bytes.0) {
                payloads.push(r.sent_bytes.0.clone());
            }
        }
        if payloads.is_empty() && t.protocol == Protocol::Kwp {
            payloads.push(safety::IDENTIFICATION_READ.to_vec());
        }
        if payloads.is_empty() {
            return Err(CliError::definition(
                "the definition has no identification request",
            ));
        }
        for p in payloads {
            let (r, tries) = o
                .link
                .request_retrying(&mut o.session, &p, &policy)
                .map_err(|e| e.to_cli())?;
            let reply = link::positive(&r)?.to_vec();
            let fallback = defs::choose_by_ident(&o.ecu, o.connected.keywords, &p, &reply);
            let parsed = (p == safety::IDENTIFICATION_READ)
                .then(|| kline_spec::Ident::from_renault_2180(&reply))
                .flatten();
            let candidates: Vec<&kline_spec::EcuDef> = o.ecu.defs.iter().map(|d| &d.def).collect();
            let has_autoidents = candidates.iter().any(|d| !d.auto_idents.is_empty());
            let auto = parsed
                .as_ref()
                .filter(|_| has_autoidents)
                .and_then(|id| kline_spec::best_match(&candidates, id));
            let (chosen, method) = match auto {
                Some((i, level)) => (
                    Some(i),
                    format!("DDT2000 AutoIdent: {}", serde_json::to_value(level).unwrap_or_default().as_str().unwrap_or("?")),
                ),
                None => (
                    fallback.chosen,
                    "first candidate whose key bytes match and whose identification requests decode (checked; AutoIdent not applicable)".to_string(),
                ),
            };
            let (decoded, short) = chosen
                .map(|i| decode_ident(&o.ecu.defs[i].def, &p, &reply))
                .unwrap_or_default();
            let chosen_path = chosen.map(|i| o.ecu.defs[i].path.display().to_string());
            let diagfailures_ok = reply.len() == DIAGFAILURES_IDENT_BYTES;
            let ambiguous = auto.is_none() && fallback.ambiguous;
            let fitting: Vec<&defs::LoadedDef> =
                fallback.fits.iter().map(|&i| &o.ecu.defs[i]).collect();
            let differences = if ambiguous {
                defs::candidate_differences(&fitting)
            } else {
                Vec::new()
            };
            let ambiguity_note = if ambiguous {
                let mut n = format!(
                    "\nnote: definition choice unresolved: {} all decode {}: fine for reads; before any config write choose the definition with {} (docs/cli.md \"Choosing the BMT definition\")",
                    fitting.iter().map(|d| defs::def_stem(d)).collect::<Vec<_>>().join(", "),
                    hex_sp(&p),
                    choose_hint(&o.ecu)
                );
                if differences.is_empty() {
                    n.push_str("\nthey decode every request alike");
                } else {
                    n.push_str("\nthey differ in:");
                    for d in &differences {
                        n.push_str(&format!("\n  {}", d.human()));
                    }
                }
                n
            } else {
                String::new()
            };
            ctx.out.emit(
                json!({"event": "ident", "request": hex(&p), "reply": hex(&reply),
                       "tries": tries, "chosen": chosen_path, "ambiguous": ambiguous,
                       "differences": differences.iter().map(defs::Difference::to_json).collect::<Vec<_>>(),
                       "method": method,
                       "autoident": {"ident": parsed, "match": auto.map(|(_, l)| l)},
                       "reply_len": reply.len(), "shorter_than_min_bytes": short,
                       "ddt2000_diagfailures_accepts": diagfailures_ok,
                       "candidates": fallback.results,
                       "decoded": defs::decoded_json(&decoded)}),
                format!(
                    "{} -> {}{}\ndefinition: {} ({method}){}{ambiguity_note}{}{}\n{}",
                    hex_sp(&p),
                    hex_sp(&reply),
                    if tries > 1 { format!(" (after {tries} tries)") } else { String::new() },
                    chosen_path.as_deref().unwrap_or("NONE fits"),
                    if ambiguous {
                        " (AMBIGUOUS: several candidates decode, see the note)"
                    } else {
                        ""
                    },
                    if short { "\nnote: reply shorter than the definition's MinBytes; decoded anyway" } else { "" },
                    if p == safety::IDENTIFICATION_READ && !diagfailures_ok {
                        format!("\nnote: {} bytes; DDT2000's DiagFailures identification needs exactly {DIAGFAILURES_IDENT_BYTES} (only DDT2000's fault screen checks that; fine for reads and for kline)", reply.len())
                    } else {
                        String::new()
                    },
                    defs::decoded_human(&decoded)
                )
                .trim_end(),
            );
            if chosen.is_none() {
                return Err(CliError::new(
                    exit::DECODE,
                    "decode",
                    "no candidate definition fits the identification reply",
                ));
            }
        }
        Ok(())
    })
}

/// How to name one definition: `--xml`, plus the BMT shortcuts for `--ecu bmt`.
pub fn choose_hint(ecu: &Ecu) -> &'static str {
    if ecu.label == "bmt" {
        "--ecu bmt-ph1p or --ecu bmt-sagem or --xml PATH"
    } else {
        "--xml PATH"
    }
}

/// Sleep `ms` in steps, sending TesterPresent when due. False when Ctrl-C was pressed.
fn idle(ctx: &Ctx, o: &mut Open, ms: u64) -> Result<bool, CliError> {
    let end = o.link.now_ms() + ms;
    loop {
        if ctx.stopped() {
            return Ok(false);
        }
        o.link.keepalive(&mut o.session).map_err(|e| e.to_cli())?;
        let now = o.link.now_ms();
        if now >= end {
            return Ok(true);
        }
        // Wake at least every 100 ms to notice Ctrl-C (ours).
        o.link.sleep_ms((end - now).min(100) as u32);
    }
}

pub fn live(ctx: &mut Ctx, a: &LiveArgs) -> Result<(), CliError> {
    let ecu = Ecu::resolve(&a.link.ecu, &ctx.g.defs_dir())?;
    let frames: Vec<defs::Resolved> = a
        .frames
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| defs::resolve_request(ecu.def(), s))
        .collect::<Result<_, _>>()?;
    if frames.is_empty() {
        return Err(CliError::usage("--frames is empty"));
    }
    for f in &frames {
        precheck_read(&ecu, &f.payload)?;
    }
    if !(a.rate > 0.0 && a.rate <= 50.0) {
        return Err(CliError::usage(
            "--rate must be in (0, 50] cycles per second",
        ));
    }
    let selected: Option<Vec<String>> = a.items.as_ref().map(|s| {
        s.split(',')
            .map(|n| n.trim().to_lowercase())
            .filter(|n| !n.is_empty())
            .collect()
    });
    if let Some(sel) = &selected {
        let known: Vec<&str> = frames
            .iter()
            .flat_map(|f| defs::requests_for(ecu.def(), &f.payload))
            .flat_map(|r| r.received_items.iter().map(|i| i.name.as_str()))
            .collect();
        let unknown: Vec<&String> = sel
            .iter()
            .filter(|n| !known.iter().any(|k| k.trim().to_lowercase() == **n))
            .collect();
        if sel.is_empty() || !unknown.is_empty() {
            return Err(CliError::definition(format!(
                "--items {:?}: no such item in the --frames requests (they have: {})",
                a.items.as_deref().unwrap_or(""),
                known.join(", ")
            )));
        }
    }
    let keep = |decoded: Decoded| -> Decoded {
        let Some(sel) = &selected else {
            return decoded;
        };
        decoded
            .into_iter()
            .map(|(n, r)| {
                (
                    n,
                    r.map(|items| {
                        items
                            .into_iter()
                            .filter(|i| sel.contains(&i.name.trim().to_lowercase()))
                            .collect()
                    }),
                )
            })
            .collect()
    };
    let period = (1000.0 / a.rate) as u64;
    let policy = a.link.retry_policy();
    with_session(ctx, &a.link, "live", |ctx, o| {
        definition_choice_notice(ctx, o);
        // --compact: the last shown text of every item (to print only what changed).
        let mut last: std::collections::HashMap<String, String> = Default::default();
        let mut cycle = 0u64;
        // DDT2000 screen refresh (RE: DdtDiagScrXml 0x5204ce50): only a cycle in which EVERY
        // request failed counts; it is retried after 400 ms, at most 3 times, then "Fatal error,
        // No communication with ECU".
        let mut retries = 0u8;
        // Over the current streak of failed cycles: did the ECU answer every request, only
        // negatively? Then the line works and "no communication" would be wrong (item 8).
        let mut streak_negative = true;
        let mut streak_nrcs: Vec<(u8, u8)> = Vec::new();
        while a.count.is_none_or(|c| cycle < c) && !ctx.stopped() {
            let start = o.link.now_ms();
            let mut any_ok = false;
            let mut line: Vec<String> = Vec::new();
            for f in &frames {
                let t_ms = o.link.now_ms();
                match o.link.request(&mut o.session, &f.payload) {
                    Ok(r) => match link::positive(&r) {
                        Ok(reply) => {
                            any_ok = true;
                            let decoded = keep(defs::decode_reply(o.ecu.def(), &f.payload, reply));
                            if a.compact {
                                for i in decoded
                                    .iter()
                                    .filter_map(|(_, r)| r.as_ref().ok())
                                    .flatten()
                                {
                                    let mut t = format!(
                                        "{}={}",
                                        i.name.trim(),
                                        i.display.as_deref().unwrap_or("-")
                                    );
                                    if let Some(u) = i.unit.as_deref().filter(|u| !u.is_empty()) {
                                        t.push(' ');
                                        t.push_str(u);
                                    }
                                    let changed =
                                        last.insert(i.name.clone(), t.clone()).as_ref() != Some(&t);
                                    if changed || selected.is_some() {
                                        line.push(t);
                                    }
                                }
                            }
                            let (alts, alt_human) = alternatives(o, &f.payload, reply);
                            let mut j = json!({"event": "live", "cycle": cycle, "t_ms": t_ms,
                                       "request": hex(&f.payload), "reply": hex(reply),
                                       "decoded": defs::decoded_json(&decoded)});
                            if o.ecu.defs.len() > 1 {
                                j["alternatives"] = json!(alts);
                            }
                            // With --items, a frame without a selected item prints nothing.
                            let shown = selected.is_none()
                                || decoded
                                    .iter()
                                    .any(|(_, r)| r.as_ref().map_or(true, |v| !v.is_empty()));
                            ctx.out.emit(
                                j,
                                if a.compact || !shown {
                                    String::new()
                                } else {
                                    format!(
                                        "#{cycle} t={t_ms} ms {}\n{}\n{alt_human}",
                                        hex_sp(&f.payload),
                                        defs::decoded_human(&decoded)
                                    )
                                    .trim_end()
                                    .to_string()
                                },
                            );
                        }
                        Err(e) => {
                            match &r {
                                SessionOutcome::Reply(Reply::Negative { sid, nrc }) => {
                                    if !streak_nrcs.contains(&(*sid, *nrc)) {
                                        streak_nrcs.push((*sid, *nrc));
                                    }
                                }
                                _ => streak_negative = false,
                            }
                            ctx.out.emit(
                                json!({"event": "live_error", "cycle": cycle, "request": hex(&f.payload), "message": e.message}),
                                format!("#{cycle} {}: {}", hex_sp(&f.payload), e.message),
                            );
                        }
                    },
                    Err(e) if e.is_link() => {
                        streak_negative = false;
                        let m = e.to_cli().message;
                        ctx.out.emit(
                            json!({"event": "live_error", "cycle": cycle, "request": hex(&f.payload), "message": m}),
                            format!("#{cycle} {}: {m}", hex_sp(&f.payload)),
                        );
                    }
                    Err(e) => return Err(e.to_cli()),
                }
            }
            if a.compact && any_ok {
                ctx.out.human(format!(
                    "#{cycle} t={start} ms {}",
                    if line.is_empty() {
                        "(no change)".to_string()
                    } else {
                        line.join("; ")
                    }
                ));
            }
            cycle += 1;
            if !any_ok {
                if retries >= policy.cycle_retries {
                    let n = retries + 1;
                    let message = if streak_negative && !streak_nrcs.is_empty() {
                        format!(
                            "ECU rejected every request ({}) in {n} cycles; stopping (the line works; the ECU does not offer these frames in this session)",
                            streak_nrcs
                                .iter()
                                .map(|(sid, nrc)| format!(
                                    "7F {sid:02X} {nrc:02X} {}",
                                    link::nrc_name(*nrc)
                                ))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    } else {
                        format!(
                            "no communication with the ECU: every request failed in {n} cycles in a row; stopping"
                        )
                    };
                    return Err(CliError::new(exit::COMM, "comm", message));
                }
                retries += 1;
                ctx.out.emit(
                    json!({"event": "live_retry", "retry": retries, "of": policy.cycle_retries,
                           "wait_ms": policy.cycle_retry_ms}),
                    format!("read data: retry {retries}/{}", policy.cycle_retries),
                );
                if !idle(ctx, o, u64::from(policy.cycle_retry_ms))? {
                    break;
                }
                continue;
            }
            retries = 0;
            streak_negative = true;
            streak_nrcs.clear();
            if a.count.is_none_or(|c| cycle < c) {
                let next = start + period;
                let now = o.link.now_ms();
                if !idle(ctx, o, next.saturating_sub(now))? {
                    break;
                }
            }
        }
        Ok(())
    })
}

/// `dtc`: for `<Failures Id="1">` the frames carrying `FailureFlag="1"` items and the set flags
/// with their devices (DDT2000 DiagFailures "Reading flags"); otherwise the Tier 0 requests named
/// like faults and every non-zero item. Ends with the clear plan (what `kline clear-dtc` would send
/// and what DDT2000 would send); nothing is cleared here.
#[derive(Debug, Clone, Default, Args)]
pub struct DtcArgs {
    #[command(flatten)]
    pub link: LinkArgs,
    /// Also print what `kline clear-dtc` would send (and DDT2000's choice); JSON always has it.
    #[arg(long)]
    pub show_clear: bool,
}

/// Present / memorized / other counts of set fault items, by item name (French `présent`,
/// `mémorisé`; English too).
fn fault_counts(active: &[Json]) -> (usize, usize, usize) {
    let (mut p, mut m) = (0, 0);
    for a in active {
        let n = defs::fold_name(a["name"].as_str().unwrap_or(""));
        if n.contains("memoris") || n.contains("memoriz") || n.contains("stored") {
            m += 1;
        } else if n.contains("present") {
            p += 1;
        }
    }
    (p, m, active.len() - p - m)
}

/// `N set (present: a, memorized: b[, other: c])`, or `N set` when no name says either.
fn fault_count_text(active: &[Json]) -> String {
    let (p, m, o) = fault_counts(active);
    if p + m == 0 {
        return format!("{} set", active.len());
    }
    let mut s = format!("{} set (present: {p}, memorized: {m}", active.len());
    if o > 0 {
        s.push_str(&format!(", other: {o}"));
    }
    s.push(')');
    s
}

pub fn dtc(ctx: &mut Ctx, a: &DtcArgs) -> Result<(), CliError> {
    let show_clear = a.show_clear;
    with_session(ctx, &a.link, "dtc", |ctx, o| {
        let t = o.target();
        let def = o.ecu.def();
        let flag_reqs: Vec<&kline_spec::Request> = def
            .failure_flag_requests()
            .into_iter()
            .filter(|r| safety::classify(&t, &r.sent_bytes.0) == Ok(Tier::Read))
            .collect();
        let named = defs::dtc_requests(def, &t);
        let (mode, chosen) = if !flag_reqs.is_empty() {
            (DtcMode::Flags, flag_reqs)
        } else if !named.is_empty() {
            (DtcMode::NamedRequests, named)
        } else {
            (DtcMode::FaultItems, defs::fault_item_requests(def, &t))
        };
        let failures = def
            .failures
            .as_ref()
            .map(|f| format!("Failures Id={} {:?}", f.id, f.name))
            .unwrap_or_else(|| "no <Failures>".into());
        let method = match mode {
            DtcMode::Flags => "Failures Id=1: frames with FailureFlag items; non-zero flags (DDT2000 DiagFailures)".to_string(),
            DtcMode::NamedRequests => "requests named like faults; non-zero items".to_string(),
            DtcMode::FaultItems => format!(
                "{failures}: DDT2000 shows no fault list for this definition; non-zero fault-named items (panne/defaut/fault) of its Tier 0 reads instead"
            ),
        };
        let reqs: Vec<(String, Vec<u8>)> = chosen
            .iter()
            .map(|r| (r.name.clone(), r.sent_bytes.0.clone()))
            .collect();
        let by_flags = mode == DtcMode::Flags;
        if reqs.is_empty() {
            return Err(CliError::definition(format!(
                "the definition has no fault-flag read ({failures}; no Devices flags, no Tier 0 request or item named panne/defaut/fault/dtc)"
            )));
        }
        if mode == DtcMode::FaultItems {
            ctx.out.emit(
                json!({"event": "dtc_note", "failures": def.failures, "method": method}),
                format!("note: {method}"),
            );
        }
        let mut all_active: Vec<Json> = Vec::new();
        let mut done: Vec<Vec<u8>> = Vec::new();
        for (name, p) in reqs {
            if done.contains(&p) {
                continue;
            }
            done.push(p.clone());
            let r = o.link.request(&mut o.session, &p).map_err(|e| e.to_cli())?;
            let reply = link::positive(&r)?.to_vec();
            let decoded = defs::decode_reply(o.ecu.def(), &p, &reply);
            let mut active = Vec::new();
            for (rname, d) in &decoded {
                let Ok(items) = d else { continue };
                if by_flags {
                    for f in o.ecu.def().active_faults(rname, items) {
                        active.push(json!({"name": f.item, "devices": f.devices,
                                           "display": f.display, "raw": f.raw}));
                    }
                } else {
                    for i in items {
                        if mode == DtcMode::FaultItems && !defs::is_fault_name(&i.name) {
                            continue;
                        }
                        if i.raw_int().is_some_and(|v| v != 0) {
                            active.push(
                                json!({"name": i.name, "display": i.display, "raw": i.raw_int()}),
                            );
                        }
                    }
                }
            }
            let (pc, mc, _) = fault_counts(&active);
            let human = format!(
                "{name} ({}): {}{}",
                hex_sp(&p),
                fault_count_text(&active),
                active
                    .iter()
                    .map(|a| format!(
                        "\n    {} = {}{}",
                        a["name"].as_str().unwrap_or(""),
                        a["display"].as_str().unwrap_or(""),
                        a["devices"]
                            .as_array()
                            .filter(|d| !d.is_empty())
                            .map(|d| format!(
                                "  [{}]",
                                d.iter()
                                    .filter_map(|x| x.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ))
                            .unwrap_or_default()
                    ))
                    .collect::<String>()
            );
            ctx.out.emit(
                json!({"event": "dtc", "request": hex(&p), "name": name, "reply": hex(&reply),
                       "active": active, "present": pc, "memorized": mc,
                       "decoded": defs::decoded_json(&decoded)}),
                human,
            );
            all_active.extend(active);
        }
        let plan = o.ecu.def().clear_plan();
        let total = all_active.len();
        let (pc, mc, _) = fault_counts(&all_active);
        let count = fault_count_text(&all_active);
        let count = count.strip_prefix(&format!("{total} set")).unwrap_or("");
        ctx.out.emit(
            json!({"event": "dtc_summary", "active": total, "present": pc, "memorized": mc,
                   "method": method, "clear_plan": clear_plan_json(&plan)}),
            if show_clear {
                format!(
                    "{total} fault flag(s) set{count}\n{}",
                    clear_plan_human(&plan)
                )
            } else {
                format!("{total} fault flag(s) set{count}")
            },
        );
        Ok(())
    })
}

/// Where `dtc` finds the faults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DtcMode {
    /// `<Failures Id="1">`: FailureFlag items.
    Flags,
    /// Tier 0 requests named like faults: every non-zero item.
    NamedRequests,
    /// Neither (e.g. ISO8 `Failures Id="0"`): fault-named items of Tier 0 reads.
    FaultItems,
}

/// JSON of a [`kline_spec::ClearPlan`].
pub fn clear_plan_json(p: &kline_spec::ClearPlan) -> Json {
    json!({"ours": {"payload": hex(&p.ours.payload), "source": p.ours.source},
           "ddt2000": {"payload": hex(&p.ddt2000.payload), "source": p.ddt2000.source},
           "differs": p.differs(), "tier": "service (tier 1)"})
}

fn clear_source_text(s: &kline_spec::ClearSource) -> String {
    match s {
        kline_spec::ClearSource::NamedRequest { name } => format!("definition request {name:?}"),
        kline_spec::ClearSource::DefinitionRequest { name } => {
            format!("the definition's own clear request {name:?}")
        }
        kline_spec::ClearSource::Ddt2000Fallback => "DDT2000's built-in fallback".into(),
    }
}

/// Human text of a clear plan: the bytes `kline clear-dtc` sends and what DDT2000 would send.
pub fn clear_plan_human(p: &kline_spec::ClearPlan) -> String {
    let mut s = format!(
        "clear (Tier 1, human approval): kline clear-dtc would send {} ({})",
        hex_sp(&p.ours.payload),
        clear_source_text(&p.ours.source)
    );
    if p.differs() {
        s.push_str(&format!(
            "\nDDT2000's global clear would send {} instead ({}; no ClearDiagnosticInformation.* request in the definition); use --ddt2000-clear for that",
            hex_sp(&p.ddt2000.payload),
            clear_source_text(&p.ddt2000.source)
        ));
    } else {
        s.push_str("; DDT2000 sends the same bytes");
    }
    s
}

pub fn parse_range(s: &str) -> Result<(ScanService, u8, u8), CliError> {
    let bad = || CliError::usage(format!("bad --range {s:?}: use 21:B0-C5 or 1A:80-9F"));
    let (svc, r) = s.split_once(':').ok_or_else(bad)?;
    let service = match svc.trim().to_ascii_uppercase().as_str() {
        "21" => ScanService::LocalIdentifier,
        "1A" => ScanService::EcuIdentification,
        _ => {
            return Err(CliError::refused(format!(
                "scan service {svc:?} refused: only read services 21 and 1A can be scanned"
            )))
        }
    };
    let (a, b) = r.split_once('-').ok_or_else(bad)?;
    let a = u8::from_str_radix(a.trim(), 16).map_err(|_| bad())?;
    let b = u8::from_str_radix(b.trim(), 16).map_err(|_| bad())?;
    if a > b {
        return Err(bad());
    }
    Ok((service, a, b))
}

pub fn scan(ctx: &mut Ctx, a: &ScanArgs) -> Result<(), CliError> {
    let (service, from, to) = parse_range(&a.range)?;
    with_session(ctx, &a.link, "scan", |ctx, o| {
        let mut plan = ScanPlan::new(service, from..=to, a.interval);
        let mut sent = 0u32;
        let stop = loop {
            if ctx.stopped() {
                break None;
            }
            match plan.next(o.link.now_ms()) {
                ScanStep::Send(req) => {
                    sent += 1;
                    let id = req.identifier();
                    let payload = req.payload().to_vec();
                    let first = o
                        .session
                        .request_scan(req, o.link.now_ms())
                        .map_err(|e| CliError::session(&e))?;
                    let (ev, human) = match o.link.run(&mut o.session, first) {
                        Ok(SessionOutcome::Reply(r)) => {
                            plan.report(ScanResult::Reply(&r));
                            match &r {
                                Reply::Positive(p) => (
                                    json!({"result": "positive", "reply": hex(p)}),
                                    format!("positive {}", hex_sp(p)),
                                ),
                                Reply::Negative { nrc, .. } => (
                                    json!({"result": "negative", "nrc": format!("{nrc:02X}"), "name": link::nrc_name(*nrc)}),
                                    format!("negative {nrc:02X} ({})", link::nrc_name(*nrc)),
                                ),
                                other => (
                                    json!({"result": "other", "reply": format!("{other:?}")}),
                                    format!("{other:?}"),
                                ),
                            }
                        }
                        Ok(other) => {
                            plan.report(ScanResult::Missing);
                            (
                                json!({"result": "missing", "message": format!("{other:?}")}),
                                "missing".into(),
                            )
                        }
                        Err(e) if e.is_link() => {
                            plan.report(ScanResult::Missing);
                            let m = e.to_cli().message;
                            (
                                json!({"result": "missing", "message": m}),
                                format!("missing: {m}"),
                            )
                        }
                        Err(e) => return Err(e.to_cli()),
                    };
                    let mut j = ev;
                    j["event"] = json!("scan");
                    j["request"] = json!(hex(&payload));
                    j["id"] = json!(format!("{id:02X}"));
                    ctx.out.emit(j, format!("{} : {human}", hex_sp(&payload)));
                }
                ScanStep::Wait { ms } => {
                    if !idle(ctx, o, u64::from(ms))? {
                        break None;
                    }
                }
                ScanStep::AwaitingReport => {
                    return Err(CliError::new(
                        exit::INTERNAL,
                        "internal",
                        "scan result not reported",
                    ))
                }
                ScanStep::Finished => break None,
                ScanStep::Stopped(s) => break Some(s),
            }
        };
        let reason = stop.map(|s| match s {
            ScanStop::MissingReplies => "missing_replies".to_string(),
            ScanStop::SecurityNrc(n) => format!("security_nrc_{n:02X}"),
        });
        ctx.out.emit(
            json!({"event": "scan_done", "sent": sent, "stopped": reason}),
            match &reason {
                Some(r) => format!("scan stopped after {sent} requests: {r}"),
                None => format!("scan finished: {sent} requests"),
            },
        );
        match reason {
            Some(r) => Err(CliError::new(
                exit::SCAN_STOPPED,
                "scan_stopped",
                format!("scan stopped early: {r}"),
            )),
            None => Ok(()),
        }
    })
}

/// The policy's view of a request without sending anything: tier and RiskInfo, or why it is
/// refused (kline-core policy with the definition's guard, `kline_core::spec::definition_guard`).
pub struct Assessment {
    pub target: Target,
    pub payload: Vec<u8>,
    pub names: Vec<String>,
    pub risk: Result<RiskInfo, CliError>,
}

/// Resolve `request` (with `sets`, [`defs::resolve_request_with`]) and assess it.
pub fn assess(ecu: &Ecu, request: &str, sets: &[String]) -> Result<Assessment, CliError> {
    let r = defs::resolve_request_with(ecu.def(), request, sets)?;
    let target = ecu.target()?;
    // Same policy as the session (kline-core): byte rules plus the definition guard.
    let risk =
        safety::assess_in(&ecu.guard(), &target, &r.payload).map_err(|d| CliError::denied(&d));
    Ok(Assessment {
        target,
        payload: r.payload,
        names: r.names,
        risk,
    })
}

pub fn assessment_json(a: &Assessment) -> Json {
    json!({
        "request": hex(&a.payload),
        "names": a.names,
        "ecu_address": format!("{:02X}", a.target.ecu_address),
        "protocol": proto_name(a.target.protocol),
        "allowed": a.risk.is_ok(),
        "risk": a.risk.as_ref().ok().map(risk_json),
        "refused": a.risk.as_ref().err().map(|e| e.message.clone()),
    })
}

pub fn assessment_human(a: &Assessment) -> String {
    let head = format!(
        "{} to {:02X} ({}){}",
        hex_sp(&a.payload),
        a.target.ecu_address,
        proto_name(a.target.protocol),
        if a.names.is_empty() {
            String::new()
        } else {
            format!(" = {}", crate::sanitize_list(&a.names))
        }
    );
    match &a.risk {
        Err(e) => format!("{head}\nREFUSED: {}", e.message),
        Ok(r) => format!("{head}\n{}", crate::risk_human(r)),
    }
}

pub fn risk(ctx: &mut Ctx, a: &RiskArgs) -> Result<(), CliError> {
    let ecu = Ecu::resolve(&a.ecu, &ctx.g.defs_dir())?;
    let asx = assess(&ecu, &a.request, &a.set)?;
    let watch = a.hold.watch_read(&ecu)?;
    let mut j = assessment_json(&asx);
    j["event"] = json!("risk");
    let mut human = assessment_human(&asx);
    if let Some((command, suggested)) =
        exec_command(ctx, &ecu, &a.ecu, &asx, &a.hold, watch.as_ref())?
    {
        j["command"] = json!(command);
        human.push_str(&format!(
            "\n{}: {command}",
            if suggested {
                "command (remote learning: hold the session and watch 21 A3 for the learning window)"
            } else {
                "command"
            }
        ));
    }
    ctx.out.emit(j, human);
    asx.risk.map(drop)
}

/// The exact `kline exec` command for an allowed non-read (None for a read or a refusal), and
/// whether --hold/--watch were added because the request is the BMT remote learning and none
/// were given.
pub fn exec_command(
    ctx: &Ctx,
    ecu: &Ecu,
    ecu_args: &EcuArgs,
    asx: &Assessment,
    hold: &HoldArgs,
    watch: Option<&defs::Resolved>,
) -> Result<Option<(String, bool)>, CliError> {
    match &asx.risk {
        Ok(r) if r.tier != Tier::Read => {}
        _ => return Ok(None),
    }
    let mut words: Vec<String> = vec!["kline".into()];
    words.extend(ctx.g.port_cli());
    words.push("exec".into());
    words.extend(ecu_args.to_cli());
    words.push(hex(&asx.payload));
    let remote_learning = safety::is_immobilizer_ecu(&ecu.guard(), &asx.target)
        && safety::BMT_REMOTE_LEARNING
            .iter()
            .any(|p| p.as_slice() == asx.payload);
    let suggested = remote_learning && hold.hold.is_none();
    if suggested {
        let h = HoldArgs {
            hold: Some(REMOTE_LEARNING_HOLD_S),
            watch: Some(hex(&REMOTE_LEARNING_WATCH)),
            watch_interval: HOLD_WATCH_INTERVAL_MS,
        };
        let w = h.watch_read(ecu)?;
        words.extend(h.to_cli(w.as_ref()));
    } else {
        words.extend(hold.to_cli(watch));
    }
    Ok(Some((crate::shell_join(&words), suggested)))
}

// ---- replay ----

/// One exchange reconstructed from a capture.
struct Exchange {
    t_ms: u64,
    request: Vec<u8>,
    replies: Vec<Result<Reply, String>>,
}

pub fn replay(ctx: &mut Ctx, a: &ReplayArgs) -> Result<(), CliError> {
    let entries = capture::read(&a.file)?;
    let ecu = if a.ecu.ecu.is_some() || !a.ecu.xml.is_empty() {
        Some(Ecu::resolve(&a.ecu, &ctx.g.defs_dir())?)
    } else {
        None
    };
    let mut iso8: Option<bool> = ecu.as_ref().map(|e| e.protocol() == Protocol::Iso8);
    let mut count = 0usize;
    let mut i = 0;
    while i < entries.len() {
        let e = &entries[i];
        let bytes = e
            .bytes()
            .map_err(|m| CliError::new(exit::CAPTURE, "capture", m))?;
        if e.dir == Dir::Tx && e.kind.is_some() {
            // Wake-up: the next rx holds 55 KW1 KW2 (5-baud); a single-byte tx after it is ~KW2.
            let (kind, wake) = match e.kind {
                Some(Kind::FiveBaud) => ("five_baud", format!("5-baud byte {}", hex(&bytes))),
                _ => ("fast_init", "fast init".to_string()),
            };
            let kw = entries
                .get(i + 1)
                .filter(|n| n.dir == Dir::Rx)
                .map(|n| n.hex.clone());
            if e.kind == Some(Kind::FiveBaud) && ecu.is_none() {
                let handshake = entries
                    .iter()
                    .skip(i + 1)
                    .find(|n| n.dir == Dir::Tx)
                    .is_some_and(|n| n.kind.is_none() && n.hex.len() == 2);
                iso8 = Some(!handshake);
            }
            if e.kind == Some(Kind::FastInit) {
                iso8 = Some(false);
            }
            ctx.out.emit(
                json!({"event": "wake", "t_ms": e.t_ms, "kind": kind, "hex": e.hex, "reply": kw}),
                format!(
                    "t={} ms wake-up: {wake}, reply {}",
                    e.t_ms,
                    kw.as_deref().unwrap_or("none")
                ),
            );
            i += 1;
            continue;
        }
        if e.dir == Dir::Rx {
            i += 1;
            continue;
        }
        // A tx frame: the rx entries up to the next tx are its echo and replies.
        let mut rx = Vec::new();
        let mut j = i + 1;
        while j < entries.len() && entries[j].dir == Dir::Rx {
            rx.extend(
                entries[j]
                    .bytes()
                    .map_err(|m| CliError::new(exit::CAPTURE, "capture", m))?,
            );
            j += 1;
        }
        if let Some(x) = exchange(&bytes, &rx, iso8.unwrap_or(false), e.t_ms) {
            count += 1;
            emit_exchange(ctx, ecu.as_ref(), &x);
        } else {
            ctx.out.emit(
                json!({"event": "tx", "t_ms": e.t_ms, "hex": e.hex, "rx": hex(&rx)}),
                format!("t={} ms tx {} rx {}", e.t_ms, hex_sp(&bytes), hex_sp(&rx)),
            );
        }
        i = j;
    }
    ctx.out.emit(
        json!({"event": "replay_done", "entries": entries.len(), "exchanges": count}),
        format!("{} entries, {count} exchanges", entries.len()),
    );
    Ok(())
}

/// Split `rx` (echo + reply frames) of the request frame `tx`.
fn exchange(tx: &[u8], rx: &[u8], iso8: bool, t_ms: u64) -> Option<Exchange> {
    let framing = if iso8 {
        Framing::Iso8
    } else {
        if tx.len() < 4 {
            return None;
        }
        Framing::Kwp(KwpHeader {
            target: tx[1],
            source: tx[2],
            length_in_format: tx[0] & 0x3F != 0,
        })
    };
    let request = framing.reversed().decode(tx).ok()?.to_vec();
    let mut rest = rx;
    // Echo (a leading 0x00 is break residue, docs/protocol.md "Exchange").
    if rest.first() == Some(&0) && tx.first() != Some(&0) {
        rest = &rest[1..];
    }
    if rest.starts_with(tx) {
        rest = &rest[tx.len()..];
    }
    let mut replies = Vec::new();
    while !rest.is_empty() {
        let need = match framing.need_after_first(rest[0]) {
            Ok(ReplyNeed::Bytes(n)) => 1 + n,
            Ok(ReplyNeed::HeaderWithLength) => match rest.get(3) {
                Some(&l) => 4 + l as usize + 1,
                None => rest.len() + 1,
            },
            Err(e) => {
                replies.push(Err(format!("{e} at {}", hex(rest))));
                break;
            }
        };
        if rest.len() < need {
            replies.push(Err(format!("incomplete reply {}", hex_sp(rest))));
            break;
        }
        let (f, tail) = rest.split_at(need);
        replies.push(
            framing
                .decode(f)
                .map_err(|e| e.to_string())
                .and_then(|p| Reply::classify(&framing, p).map_err(|e| e.to_string())),
        );
        rest = tail;
    }
    Some(Exchange {
        t_ms,
        request,
        replies,
    })
}

fn emit_exchange(ctx: &Ctx, ecu: Option<&Ecu>, x: &Exchange) {
    let final_reply = x.replies.iter().rev().find_map(|r| r.as_ref().ok());
    let decoded = match (ecu, final_reply) {
        (Some(e), Some(Reply::Positive(p))) => defs::decode_reply(e.def(), &x.request, p),
        _ => Vec::new(),
    };
    let replies: Vec<Json> = x
        .replies
        .iter()
        .map(|r| match r {
            Ok(Reply::Positive(p)) => json!({"positive": hex(p)}),
            Ok(Reply::Negative { sid, nrc }) => {
                json!({"negative": {"sid": format!("{sid:02X}"), "nrc": format!("{nrc:02X}")}})
            }
            Ok(Reply::ResponsePending { sid }) => json!({"pending": format!("{sid:02X}")}),
            Ok(Reply::Iso8Status(s)) => json!({"iso8_status": format!("{s:02X}"),
                                                "name": kline_core::frame::iso8_status_name(*s)}),
            Err(e) => json!({"error": e}),
        })
        .collect();
    let human_replies: Vec<String> = x
        .replies
        .iter()
        .map(|r| match r {
            Ok(Reply::Positive(p)) => hex_sp(p),
            Ok(Reply::Iso8Status(s)) => format!(
                "ISO8 status {s:02X} ({})",
                kline_core::frame::iso8_status_name(*s)
            ),
            Ok(other) => format!("{other:?}"),
            Err(e) => format!("error: {e}"),
        })
        .collect();
    ctx.out.emit(
        json!({"event": "exchange", "t_ms": x.t_ms, "request": hex(&x.request),
               "replies": replies, "decoded": defs::decoded_json(&decoded)}),
        format!(
            "t={} ms {} -> {}\n{}",
            x.t_ms,
            hex_sp(&x.request),
            if human_replies.is_empty() {
                "no reply".to_string()
            } else {
                human_replies.join(" | ")
            },
            defs::decoded_human(&decoded)
        )
        .trim_end(),
    );
}

/// Tier name for messages.
pub fn tier_label(t: Tier) -> &'static str {
    tier_name(t)
}

#[cfg(test)]
mod setup_tests {
    use super::*;

    /// Code review L7: when the default definitions directory itself is in a repository (a
    /// dotfiles repo at `~`), the refusal must not suggest that same default.
    #[test]
    fn refusal_for_the_default_directory_does_not_suggest_it() {
        let default = defs::data_dir().join("ecus");
        let m = setup_refusal(&default, std::path::Path::new("/home/x"));
        assert!(m.contains("default definitions directory"), "{m}");
        assert!(m.contains("outside any repository"), "{m}");
        assert!(!m.contains("use the default directory"), "{m}");
        let m = setup_refusal(
            std::path::Path::new("/repo/ecus"),
            std::path::Path::new("/repo"),
        );
        assert!(m.contains("use the default directory"), "{m}");
    }
}
