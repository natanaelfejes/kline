//! Tier 0 commands shared by `clio` and `clio-agent`, and the session lifecycle around them.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::Args;
use clio_core::frame::{Framing, KwpHeader, Reply, ReplyNeed};
use clio_core::safety::{self, Protocol, RiskInfo, Target, Tier};
use clio_core::scan::{ScanPlan, ScanResult, ScanService, ScanStep, ScanStop};
use clio_core::session::{Session, SessionOutcome};
use clio_core::InitKind;
use serde_json::{json, Value as Json};

use crate::capture::{self, Capture, Dir, Kind};
use crate::defs::{self, Ecu, EcuArgs};
use crate::error::{exit, CliError};
use crate::link::{self, Connected, Link, SessionArg, VariantArg};
use crate::out::{Out, Sink};
use crate::ports::{self, PortInfo};
use crate::trace::Tracer;
use crate::transport::{PortSpec, Transport};
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
    /// Serial port of the KKL cable (default: the one /dev/cu.usbserial-*).
    #[arg(long, global = true, value_name = "PATH")]
    pub port: Option<String>,
    /// Simulator control socket: --port is then clio-sim's pty (docs/sim.md).
    #[arg(long, global = true, value_name = "SOCKET")]
    pub sim_control: Option<PathBuf>,
    /// Capture directory (default $CLIO_CAPTURE_DIR or ~/Documents/Clio/captures).
    #[arg(long, global = true, value_name = "DIR")]
    pub capture_dir: Option<PathBuf>,
    /// ECU definitions directory for --ecu (default $CLIO_DDT_XML or ~/Documents/Clio/ddt-extract/ecus).
    #[arg(long, global = true, value_name = "DIR")]
    pub defs: Option<PathBuf>,
}

impl GlobalArgs {
    pub fn capture_dir(&self) -> PathBuf {
        self.capture_dir
            .clone()
            .or_else(|| std::env::var_os("CLIO_CAPTURE_DIR").map(PathBuf::from))
            .unwrap_or_else(|| defs::home().join("Documents/Clio/captures"))
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
        v
    }
}

#[derive(Debug, Clone, Default, Args)]
pub struct LinkArgs {
    #[command(flatten)]
    pub ecu: EcuArgs,
    /// Init variant (docs/protocol.md "Init variants").
    #[arg(long, value_enum, default_value_t = VariantArg::Auto)]
    pub variant: VariantArg,
    /// Diagnostic session to open after the init.
    #[arg(long, value_enum, default_value_t = SessionArg::Auto)]
    pub session: SessionArg,
}

impl LinkArgs {
    pub fn to_cli(&self) -> Vec<String> {
        let mut v = self.ecu.to_cli();
        if self.variant != VariantArg::Auto {
            v.extend(["--variant".into(), self.variant.label().into()]);
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
}

#[derive(Debug, Clone, Args)]
pub struct ScanArgs {
    #[command(flatten)]
    pub link: LinkArgs,
    /// Service and identifier range, e.g. 21:B0-C5 or 1A:80-9F (read services only).
    #[arg(long)]
    pub range: String,
    /// Milliseconds between request starts (at least 200, clio-core scan policy).
    #[arg(long, default_value_t = clio_core::scan::MIN_SCAN_INTERVAL_MS)]
    pub interval: u32,
}

#[derive(Debug, Clone, Args)]
pub struct RiskArgs {
    #[command(flatten)]
    pub ecu: EcuArgs,
    /// Request name from the definition, or hex bytes.
    pub request: String,
}

#[derive(Debug, Clone, Args)]
pub struct ReplayArgs {
    /// A capture (JSONL, clio-sim replay format).
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
}

impl Env {
    pub fn system() -> Env {
        Env {
            stdout: Sink::stdout(),
            stderr: Sink::stderr(),
            opener: Box::new(crate::transport::open),
            list_ports: Box::new(ports::list),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// One command invocation.
pub struct Ctx<'a> {
    pub g: &'a GlobalArgs,
    pub env: &'a mut Env,
    pub out: Out,
    /// Binary name for messages (`clio`, `clio-agent`).
    pub tool: &'static str,
}

impl<'a> Ctx<'a> {
    pub fn new(g: &'a GlobalArgs, env: &'a mut Env, tool: &'static str) -> Ctx<'a> {
        let out = Out {
            json: g.json,
            stdout: env.stdout.clone(),
            stderr: env.stderr.clone(),
        };
        Ctx { g, env, out, tool }
    }

    pub fn stopped(&self) -> bool {
        self.env.stop.load(Ordering::SeqCst)
    }

    /// Report the result and return the exit code.
    pub fn finish(&self, r: Result<(), CliError>) -> u8 {
        match r {
            Ok(()) => exit::OK,
            Err(e) => {
                if self.out.json {
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

    fn port_spec(&self) -> Result<PortSpec, CliError> {
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
    ctx.out.emit(
        json!({"event": "stopped", "ok": stop.is_ok(), "capture": path}),
        match &stop {
            Ok(()) => String::new(),
            Err(e) => format!("stop: {}", e.to_cli().message),
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
    let cfg = ecu.link_config()?;
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
    ctx.out.emit(
        json!({"event": "capture", "path": cap.path().display().to_string(),
               "trace": tracer.enabled().then(|| trace_path.display().to_string())}),
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
    let variants = link::variants(&cfg, args.variant);
    let out = ctx.out.clone();
    let (mut session, connected) = link::connect(&mut link, &cfg, &variants, &mut |a| {
        out.emit(
            a.to_json(),
            format!(
                "init {:<6} {}: {}",
                link::variant_label(a.variant),
                a.address_byte
                    .map(|b| format!("byte {b:02X}"))
                    .unwrap_or_else(|| "fast init".into()),
                a.error.as_deref().unwrap_or("ok")
            ),
        )
    })?;
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
        ctx.out.emit(
            json!({"event": "session_start", "request": hex(&p), "ok": ok, "reply": text}),
            format!(
                "session {}: {}{text}",
                hex_sp(&p),
                if ok { "ok " } else { "FAILED, continuing: " }
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

fn start_session_payload(def: &clio_spec::EcuDef, t: Target, s: SessionArg) -> Option<Vec<u8>> {
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
        ctx.out.emit(
            {
                let mut j = p.to_json();
                j["event"] = json!("port");
                j
            },
            format!(
                "{} {}{}",
                if ports::is_candidate(p) { "*" } else { " " },
                p.path,
                p.usb
                    .map(|u| format!(
                        "  usb {:04x}:{:04x} {}",
                        u.0,
                        u.1,
                        p.product.clone().unwrap_or_default()
                    ))
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
    let t_ms = o.link.now_ms();
    let r = o
        .link
        .request(&mut o.session, payload)
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
    ctx.out.emit(
        json!({"event": "read", "t_ms": t_ms, "request": hex(payload), "names": names,
               "reply": hex(&reply), "decoded": defs::decoded_json(&decoded)}),
        format!(
            "{} -> {}\n{}",
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

/// Refuse a non-read before any init (clio-core refuses again when it is sent).
fn precheck_read(ecu: &Ecu, payload: &[u8]) -> Result<(), CliError> {
    let t = ecu.target()?;
    safety::check(&t, payload)
        .map(drop)
        .map_err(|d| CliError::denied(&d))
}

pub fn read(ctx: &mut Ctx, a: &ReadArgs) -> Result<(), CliError> {
    let ecu = Ecu::resolve(&a.link.ecu, &ctx.g.defs_dir())?;
    let r = defs::resolve_request(ecu.def(), &a.request)?;
    precheck_read(&ecu, &r.payload)?;
    with_session(ctx, &a.link, "read", |ctx, o| {
        read_one(ctx, o, &r.payload, &r.names).map(drop)
    })
}

pub fn ident(ctx: &mut Ctx, a: &LinkArgs) -> Result<(), CliError> {
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
            let r = o.link.request(&mut o.session, &p).map_err(|e| e.to_cli())?;
            let reply = link::positive(&r)?.to_vec();
            let choice = defs::choose_by_ident(&o.ecu, o.connected.keywords, &p, &reply);
            let chosen = choice
                .chosen
                .map(|i| o.ecu.defs[i].path.display().to_string());
            let decoded = choice
                .chosen
                .map(|i| defs::decode_reply(&o.ecu.defs[i].def, &p, &reply))
                .unwrap_or_default();
            ctx.out.emit(
                json!({"event": "ident", "request": hex(&p), "reply": hex(&reply),
                       "chosen": chosen, "ambiguous": choice.ambiguous,
                       "candidates": choice.results,
                       "method": "first candidate whose key bytes match and whose identification requests decode (checked); AutoIdent not used"}),
                format!(
                    "{} -> {}\ndefinition: {}{}\n{}",
                    hex_sp(&p),
                    hex_sp(&reply),
                    chosen.as_deref().unwrap_or("NONE fits"),
                    if choice.ambiguous {
                        " (AMBIGUOUS: several candidates decode; check before any config write)"
                    } else {
                        ""
                    },
                    defs::decoded_human(&decoded)
                )
                .trim_end(),
            );
            if choice.chosen.is_none() {
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

/// Consecutive failed cycles after which `live` gives up (ours).
const LIVE_MAX_FAILED_CYCLES: u32 = 3;

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
    let period = (1000.0 / a.rate) as u64;
    with_session(ctx, &a.link, "live", |ctx, o| {
        let mut cycle = 0u64;
        let mut failed = 0u32;
        while a.count.is_none_or(|c| cycle < c) && !ctx.stopped() {
            let start = o.link.now_ms();
            let mut ok = true;
            for f in &frames {
                let t_ms = o.link.now_ms();
                match o.link.request(&mut o.session, &f.payload) {
                    Ok(r) => match link::positive(&r) {
                        Ok(reply) => {
                            let decoded = defs::decode_reply(o.ecu.def(), &f.payload, reply);
                            ctx.out.emit(
                                json!({"event": "live", "cycle": cycle, "t_ms": t_ms,
                                       "request": hex(&f.payload), "reply": hex(reply),
                                       "decoded": defs::decoded_json(&decoded)}),
                                format!(
                                    "#{cycle} t={t_ms} ms {}\n{}",
                                    hex_sp(&f.payload),
                                    defs::decoded_human(&decoded)
                                ),
                            );
                        }
                        Err(e) => {
                            ok = false;
                            ctx.out.emit(
                                json!({"event": "live_error", "cycle": cycle, "request": hex(&f.payload), "message": e.message}),
                                format!("#{cycle} {}: {}", hex_sp(&f.payload), e.message),
                            );
                        }
                    },
                    Err(e) if e.is_link() => {
                        ok = false;
                        let m = e.to_cli().message;
                        ctx.out.emit(
                            json!({"event": "live_error", "cycle": cycle, "request": hex(&f.payload), "message": m}),
                            format!("#{cycle} {}: {m}", hex_sp(&f.payload)),
                        );
                    }
                    Err(e) => return Err(e.to_cli()),
                }
            }
            failed = if ok { 0 } else { failed + 1 };
            if failed >= LIVE_MAX_FAILED_CYCLES {
                return Err(CliError::new(
                    exit::COMM,
                    "comm",
                    format!("{failed} cycles in a row failed; stopping"),
                ));
            }
            cycle += 1;
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
/// like faults and every non-zero item. Ends with the clear plan (what `clio clear-dtc` would send
/// and what DDT2000 would send); nothing is cleared here.
pub fn dtc(ctx: &mut Ctx, a: &LinkArgs) -> Result<(), CliError> {
    with_session(ctx, a, "dtc", |ctx, o| {
        let t = o.target();
        let def = o.ecu.def();
        let flag_reqs: Vec<&clio_spec::Request> = def
            .failure_flag_requests()
            .into_iter()
            .filter(|r| safety::classify(&t, &r.sent_bytes.0) == Ok(Tier::Read))
            .collect();
        let method = if flag_reqs.is_empty() {
            "requests named like faults; non-zero items"
        } else {
            "Failures Id=1: frames with FailureFlag items; non-zero flags (DDT2000 DiagFailures)"
        };
        let reqs: Vec<(String, Vec<u8>)> = if flag_reqs.is_empty() {
            defs::dtc_requests(def, &t)
        } else {
            flag_reqs
        }
        .iter()
        .map(|r| (r.name.clone(), r.sent_bytes.0.clone()))
        .collect();
        let by_flags = method.starts_with("Failures");
        if reqs.is_empty() {
            return Err(CliError::definition(
                "the definition has no fault-flag read (no Failures/Devices flags, no Tier 0 request named panne/defaut/fault/dtc)",
            ));
        }
        let mut total = 0usize;
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
                        if i.raw_int().is_some_and(|v| v != 0) {
                            active.push(
                                json!({"name": i.name, "display": i.display, "raw": i.raw_int()}),
                            );
                        }
                    }
                }
            }
            total += active.len();
            let human = format!(
                "{name} ({}): {} active{}",
                hex_sp(&p),
                active.len(),
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
                       "active": active, "decoded": defs::decoded_json(&decoded)}),
                human,
            );
        }
        let plan = o.ecu.def().clear_plan();
        ctx.out.emit(
            json!({"event": "dtc_summary", "active": total, "method": method,
                   "clear_plan": clear_plan_json(&plan)}),
            format!("{total} fault flag(s) set\n{}", clear_plan_human(&plan)),
        );
        Ok(())
    })
}

/// JSON of a [`clio_spec::ClearPlan`].
pub fn clear_plan_json(p: &clio_spec::ClearPlan) -> Json {
    json!({"ours": {"payload": hex(&p.ours.payload), "source": p.ours.source},
           "ddt2000": {"payload": hex(&p.ddt2000.payload), "source": p.ddt2000.source},
           "differs": p.differs(), "tier": "service (tier 1)"})
}

fn clear_source_text(s: &clio_spec::ClearSource) -> String {
    match s {
        clio_spec::ClearSource::NamedRequest { name } => format!("definition request {name:?}"),
        clio_spec::ClearSource::DefinitionRequest { name } => {
            format!("the definition's own clear request {name:?}")
        }
        clio_spec::ClearSource::Ddt2000Fallback => "DDT2000's built-in fallback".into(),
    }
}

/// Human text of a clear plan: the bytes `clio clear-dtc` sends and what DDT2000 would send.
pub fn clear_plan_human(p: &clio_spec::ClearPlan) -> String {
    let mut s = format!(
        "clear (Tier 1, human approval): clio clear-dtc would send {} ({})",
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
/// refused (address-based policy first, then the definition name guard for tiers 1-3).
pub struct Assessment {
    pub target: Target,
    pub payload: Vec<u8>,
    pub names: Vec<String>,
    pub risk: Result<RiskInfo, CliError>,
}

pub fn assess(ecu: &Ecu, request: &str) -> Result<Assessment, CliError> {
    let r = defs::resolve_request(ecu.def(), request)?;
    let target = ecu.target()?;
    let risk = match safety::assess(&target, &r.payload) {
        Err(d) => Err(CliError::denied(&d)),
        Ok(risk) if risk.tier != Tier::Read => {
            defs::name_guard(ecu.def(), &target, &r.payload).map(|()| risk)
        }
        Ok(risk) => Ok(risk),
    };
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
            format!(" = {}", a.names.join(" / "))
        }
    );
    match &a.risk {
        Err(e) => format!("{head}\nREFUSED: {}", e.message),
        Ok(r) => format!("{head}\n{}", crate::risk_human(r)),
    }
}

pub fn risk(ctx: &mut Ctx, a: &RiskArgs) -> Result<(), CliError> {
    let ecu = Ecu::resolve(&a.ecu, &ctx.g.defs_dir())?;
    let asx = assess(&ecu, &a.request)?;
    let mut j = assessment_json(&asx);
    j["event"] = json!("risk");
    ctx.out.emit(j, assessment_human(&asx));
    asx.risk.map(drop)
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
            replies.push(Err(format!("incomplete reply {}", hex(rest))));
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
            Ok(Reply::Iso8Status(s)) => json!({"iso8_status": format!("{s:02X}")}),
            Err(e) => json!({"error": e}),
        })
        .collect();
    let human_replies: Vec<String> = x
        .replies
        .iter()
        .map(|r| match r {
            Ok(Reply::Positive(p)) => hex_sp(p),
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
