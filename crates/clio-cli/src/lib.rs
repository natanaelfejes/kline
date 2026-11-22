//! `clio`: the interactive K-line CLI (docs/cli.md). The shared Tier 0 commands live in
//! clio-host; this crate adds `exec`, the only path to a non-read request (SAFETY.md rule 2):
//! clio-core's `approved-routines` feature is enabled here and nowhere else.
//!
//! `exec` runs only in an interactive terminal (stdin and stdout are TTYs), never with
//! `--json`; it shows the request's `RiskInfo` verbatim on `/dev/tty` and reads the random
//! confirmation code from `/dev/tty`, not from stdin, so a pipe cannot answer it.

#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use clio_core::safety::{self, Approval, Tier};
use clio_host::cmd::{
    self, Ctx, Env, GlobalArgs, LinkArgs, LiveArgs, ReadArgs, ReplayArgs, RiskArgs, ScanArgs,
};
use clio_host::defs::{self, Ecu};
use clio_host::error::{exit, CliError};
use clio_host::link;
use clio_host::{hex, hex_sp, risk_human, tier_name};
use serde_json::json;

#[derive(Debug, Parser)]
#[command(
    name = "clio",
    version,
    about = "K-line diagnostics for the Clio II (and other DDT2000 K-line ECUs) over a dumb FTDI KKL cable",
    after_help = "Exit codes and the car quickstart: docs/cli.md"
)]
pub struct Cli {
    #[command(flatten)]
    pub g: GlobalArgs,
    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List serial ports and the detected KKL cable.
    Ports,
    /// Init the ECU (trying init variants), open the session, Stop. Shows which variant worked.
    Connect(LinkArgs),
    /// Read 21 80 and choose the definition that fits the identification.
    Ident(LinkArgs),
    /// One read request (name or hex), decoded. Tier 0 only.
    Read(ReadArgs),
    /// Read frames repeatedly, decoded (Ctrl-C to stop).
    Live(LiveArgs),
    /// Read the fault-flag frames and show the flags that are set.
    Dtc(LinkArgs),
    /// Read-only identifier scan (21/1A only, 200 ms spacing, stops after 3 missing replies).
    Scan(ScanArgs),
    /// What a request would do (tier, RiskInfo) without sending anything.
    Risk(RiskArgs),
    /// Send ONE non-read request after showing its risk and a typed confirmation code (TTY only).
    Exec(ExecArgs),
    /// Clear the fault memory (Tier 1): shows the exact bytes, then runs like `exec` (TTY only).
    ClearDtc(ClearArgs),
    /// Decode a capture (JSONL) offline.
    Replay(ReplayArgs),
}

#[derive(Debug, Clone, Args)]
pub struct ClearArgs {
    #[command(flatten)]
    pub link: LinkArgs,
    /// Send what DDT2000's global clear would send (`ClearDiagnosticInformation.*`, else
    /// `14 FF 00`) instead of the definition's own clear request.
    #[arg(long)]
    pub ddt2000_clear: bool,
    /// Seconds to wait for the confirmation code.
    #[arg(long, default_value_t = 120)]
    pub confirm_timeout: u64,
}

#[derive(Debug, Clone, Args)]
pub struct ExecArgs {
    #[command(flatten)]
    pub link: LinkArgs,
    /// Request name from the definition, or hex bytes.
    pub request: String,
    /// Seconds to wait for the confirmation code (TesterPresent keeps the session open).
    #[arg(long, default_value_t = 120)]
    pub confirm_timeout: u64,
}

/// The human's terminal.
pub trait Prompt {
    /// Show text on the terminal.
    fn show(&mut self, text: &str) -> io::Result<()>;
    /// Start reading one line; the answer arrives on the channel.
    fn ask(&mut self) -> Receiver<io::Result<String>>;
}

/// `/dev/tty` (the controlling terminal, not stdin).
pub struct DevTty {
    w: std::fs::File,
}

impl DevTty {
    pub fn open() -> io::Result<DevTty> {
        let w = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")?;
        Ok(DevTty { w })
    }
}

impl Prompt for DevTty {
    fn show(&mut self, text: &str) -> io::Result<()> {
        writeln!(self.w, "{text}")?;
        self.w.flush()
    }

    fn ask(&mut self) -> Receiver<io::Result<String>> {
        let (tx, rx) = mpsc::channel();
        match self.w.try_clone() {
            Ok(f) => {
                std::thread::spawn(move || {
                    let mut line = String::new();
                    let r = io::BufReader::new(f).read_line(&mut line).map(|_| line);
                    let _ = tx.send(r);
                });
            }
            Err(e) => {
                let _ = tx.send(Err(e));
            }
        }
        rx
    }
}

type TtyOpener = Box<dyn FnMut() -> io::Result<Box<dyn Prompt>>>;

/// Host environment plus the terminal for approvals.
pub struct CliEnv {
    pub host: Env,
    /// stdin and stdout are terminals.
    pub interactive: bool,
    pub tty: TtyOpener,
}

impl CliEnv {
    pub fn system() -> CliEnv {
        CliEnv {
            host: Env::system(),
            interactive: io::stdin().is_terminal() && io::stdout().is_terminal(),
            tty: Box::new(|| DevTty::open().map(|t| Box::new(t) as Box<dyn Prompt>)),
        }
    }
}

/// Parse `args` and run; the exit code (docs/cli.md).
pub fn run_args<I, T>(args: I, env: &mut CliEnv) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    match Cli::try_parse_from(args) {
        Ok(cli) => run(cli, env),
        Err(e) => {
            let code = if e.use_stderr() {
                exit::USAGE
            } else {
                exit::OK
            };
            let s = e.render().to_string();
            if e.use_stderr() {
                env.host.stderr.line(s.trim_end());
            } else {
                env.host.stdout.line(s.trim_end());
            }
            code
        }
    }
}

pub fn run(cli: Cli, env: &mut CliEnv) -> u8 {
    let interactive = env.interactive;
    let CliEnv { host, tty, .. } = env;
    let mut ctx = Ctx::new(&cli.g, host, "clio");
    let r = match &cli.cmd {
        Command::Ports => cmd::ports(&mut ctx),
        Command::Connect(a) => cmd::connect(&mut ctx, a),
        Command::Ident(a) => cmd::ident(&mut ctx, a),
        Command::Read(a) => cmd::read(&mut ctx, a),
        Command::Live(a) => cmd::live(&mut ctx, a),
        Command::Dtc(a) => cmd::dtc(&mut ctx, a),
        Command::Scan(a) => cmd::scan(&mut ctx, a),
        Command::Risk(a) => cmd::risk(&mut ctx, a),
        Command::Exec(a) => exec(&mut ctx, a, interactive, tty),
        Command::ClearDtc(a) => clear_dtc(&mut ctx, a, interactive, tty),
        Command::Replay(a) => cmd::replay(&mut ctx, a),
    };
    ctx.finish(r)
}

/// `clio clear-dtc`: pick the clear request (the author's DDT2000 behaviour notes "Failures"), show its bytes
/// and DDT2000's choice, then the normal Tier 1 approval of `exec`.
pub fn clear_dtc(
    ctx: &mut Ctx,
    a: &ClearArgs,
    interactive: bool,
    tty: &mut TtyOpener,
) -> Result<(), CliError> {
    let ecu = Ecu::resolve(&a.link.ecu, &ctx.g.defs_dir())?;
    let plan = ecu.def().clear_plan();
    let choice = if a.ddt2000_clear {
        &plan.ddt2000
    } else {
        &plan.ours
    };
    ctx.out.emit(
        json!({"event": "clear_plan", "plan": cmd::clear_plan_json(&plan),
               "chosen": hex(&choice.payload)}),
        format!(
            "{}\nwill send: {}",
            cmd::clear_plan_human(&plan),
            hex_sp(&choice.payload)
        ),
    );
    let e = ExecArgs {
        link: a.link.clone(),
        request: hex(&choice.payload),
        confirm_timeout: a.confirm_timeout,
    };
    exec(ctx, &e, interactive, tty)
}

/// `clio exec`: one human-approved non-read request.
pub fn exec(
    ctx: &mut Ctx,
    a: &ExecArgs,
    interactive: bool,
    tty: &mut TtyOpener,
) -> Result<(), CliError> {
    if ctx.g.json {
        return Err(CliError::refused(
            "exec never runs with --json: a non-read request needs a human at the terminal",
        ));
    }
    if !interactive {
        return Err(CliError::refused(
            "exec needs an interactive terminal (stdin and stdout must be a TTY); nothing was sent",
        ));
    }
    let ecu = Ecu::resolve(&a.link.ecu, &ctx.g.defs_dir())?;
    let asx = cmd::assess(&ecu, &a.request)?;
    let risk = asx.risk.clone()?;
    if risk.tier == Tier::Read {
        return Err(CliError::usage(format!(
            "{} is a read: use `clio read`",
            hex_sp(&asx.payload)
        )));
    }
    let mut prompt = (tty)().map_err(|e| {
        CliError::refused(format!("cannot open /dev/tty for the confirmation: {e}"))
    })?;
    ctx.out.emit(json!({}), cmd::assessment_human(&asx));
    let payload = asx.payload.clone();
    let names = asx.names.clone();
    let timeout_ms = a.confirm_timeout.saturating_mul(1000);
    cmd::with_session(ctx, &a.link, "exec", |ctx, o| {
        let identification = if risk.tier == Tier::ConfigWrite {
            Some(config_write_prereads(ctx, o, &payload)?)
        } else {
            None
        };
        let challenge = match risk.tier {
            Tier::Service => o.session.service_challenge(&payload),
            Tier::ConfigWrite => o
                .session
                .config_write_challenge(&payload, identification.as_deref().unwrap_or(&[])),
            Tier::Expert => o.session.expert_challenge(&payload),
            Tier::Read => unreachable!("reads are refused above"),
        }
        .map_err(|e| CliError::session(&e))?;
        let t = challenge.target();
        let screen = format!(
            "\n==== APPROVAL REQUIRED: {} ====\nrequest:  {}{}\nECU:      {:02X} ({})\n{}\n\nType the code {} and Enter to send exactly this request once; anything else aborts.",
            tier_name(challenge.tier()),
            hex_sp(challenge.payload()),
            if names.is_empty() { String::new() } else { format!(" = {}", names.join(" / ")) },
            t.ecu_address,
            cmd::proto_name(t.protocol),
            risk_human(challenge.risk()),
            challenge.code()
        );
        prompt
            .show(&screen)
            .map_err(|e| CliError::refused(format!("cannot write to /dev/tty: {e}")))?;
        let rx = prompt.ask();
        let start = o.link.now_ms();
        let typed = loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(line)) => break line,
                Ok(Err(e)) => {
                    return Err(CliError::new(
                        exit::NOT_APPROVED,
                        "not_approved",
                        format!("reading /dev/tty failed: {e}; nothing was sent"),
                    ))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(CliError::new(
                        exit::NOT_APPROVED,
                        "not_approved",
                        "terminal closed; nothing was sent",
                    ))
                }
                Err(RecvTimeoutError::Timeout) => {
                    if ctx.stopped() || o.link.now_ms().saturating_sub(start) > timeout_ms {
                        return Err(CliError::new(
                            exit::NOT_APPROVED,
                            "not_approved",
                            "no confirmation in time; nothing was sent",
                        ));
                    }
                    // The ECU sleeps after 5 s of silence (P3max): keep the session open.
                    o.link.keepalive(&mut o.session).map_err(|e| e.to_cli())?;
                }
            }
        };
        let approval = Approval::confirm(challenge, &typed).map_err(|_| {
            CliError::new(
                exit::NOT_APPROVED,
                "not_approved",
                "confirmation code does not match; nothing was sent",
            )
        })?;
        let first = o
            .session
            .request_approved(&payload, approval, o.link.now_ms())
            .map_err(|e| CliError::session(&e))?;
        let r = o.link.run(&mut o.session, first).map_err(|e| e.to_cli())?;
        let reply = link::positive(&r);
        ctx.out.emit(
            json!({}),
            match &reply {
                Ok(p) => format!("sent {}: positive reply {}", hex_sp(&payload), hex_sp(p)),
                Err(e) => format!("sent {}: {}", hex_sp(&payload), e.message),
            },
        );
        reply.map(drop)
    })
}

/// Tier 2 preconditions in this session: `21 80` identification with an unambiguous
/// definition choice, then the backup read. Returns the identification reply.
fn config_write_prereads(
    ctx: &mut Ctx,
    o: &mut cmd::Open,
    payload: &[u8],
) -> Result<Vec<u8>, CliError> {
    let id_req = safety::IDENTIFICATION_READ;
    let r = o
        .link
        .request(&mut o.session, &id_req)
        .map_err(|e| e.to_cli())?;
    let ident = link::positive(&r)?.to_vec();
    let choice = defs::choose_by_ident(&o.ecu, o.connected.keywords, &id_req, &ident);
    let chosen = match (choice.chosen, choice.ambiguous) {
        (Some(i), false) => i,
        (Some(_), true) => {
            return Err(CliError::refused(
                "config write refused: several definitions fit the identification; pass the right one with --xml",
            ))
        }
        (None, _) => {
            return Err(CliError::refused(
                "config write refused: no definition fits the 21 80 identification",
            ))
        }
    };
    if chosen != 0 {
        return Err(CliError::refused(format!(
            "config write refused: the identification fits {}, not the definition the request was checked against; pass it with --xml",
            o.ecu.defs[chosen].path.display()
        )));
    }
    let backup = safety::config_backup_read(&o.target(), payload)
        .ok_or_else(|| CliError::refused("no backup read defined for this write"))?;
    let r = o
        .link
        .request(&mut o.session, &backup)
        .map_err(|e| e.to_cli())?;
    let b = link::positive(&r)?.to_vec();
    ctx.out.emit(
        json!({}),
        format!(
            "identification {} -> {}\nbackup {} -> {} (also in the capture)",
            hex_sp(&id_req),
            hex(&ident),
            hex_sp(&backup),
            hex(&b)
        ),
    );
    Ok(ident)
}
