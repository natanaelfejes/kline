//! `kline-agent`: the K-line tool for AI use (docs/cli.md "kline-agent").
//!
//! Tier 0 only. Built against kline-core WITHOUT `approved-routines` (tests/no_approvals.rs
//! checks the resolved features), so no approval type exists in this binary when it is built on
//! its own. Non-read requests can only be *prepared*: `prepare` prints the RiskInfo and the exact
//! `kline exec` command a human runs at the terminal. Output is always JSON lines. `scan` is not
//! offered here (docs/protocol.md: a scan runs only as an explicit human command).

#![forbid(unsafe_code)]

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use kline_core::safety::{self, Tier};
use kline_host::cmd::{
    self, Ctx, DtcArgs, Env, GlobalArgs, LinkArgs, LiveArgs, ReadArgs, ReplayArgs, RiskArgs,
};
use kline_host::defs::Ecu;
use kline_host::error::{exit, CliError};
use kline_host::hex;
use serde_json::json;

#[derive(Debug, Parser)]
#[command(
    name = "kline-agent",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("KLINE_GIT_COMMIT"), ")"),
    long_version = concat!(
        env!("CARGO_PKG_VERSION"),
        " (",
        env!("KLINE_GIT_COMMIT"),
        ")",
        "\nCopyright (C) 2026 Natanael Feješ. GPL-3.0-or-later, see LICENSE and NOTICE.\nhttps://github.com/natanaelfejes/kline"
    ),
    about = "Read-only K-line tool for AI agents: Tier 0 reads and `prepare` (never sends a non-read)"
)]
struct Cli {
    #[command(flatten)]
    g: GlobalArgs,
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Ports,
    Connect(LinkArgs),
    Ident(LinkArgs),
    Read(ReadArgs),
    Live(LiveArgs),
    Dtc(DtcArgs),
    Risk(RiskArgs),
    /// Prepare a non-read request for a human: RiskInfo and the exact `kline exec` command.
    Prepare(PrepareArgs),
    Replay(ReplayArgs),
}

#[derive(Debug, Clone, Args)]
struct PrepareArgs {
    #[command(flatten)]
    link: LinkArgs,
    /// Request name from the definition, or hex bytes.
    request: String,
    /// Set a sent data item of the named request (repeatable): `ITEM=VALUE`. The printed
    /// `kline exec` command carries the resulting bytes.
    #[arg(long = "set", value_name = "ITEM=VALUE")]
    set: Vec<String>,
    /// `--hold/--watch` for the printed `kline exec` command (the watch read is checked here).
    #[command(flatten)]
    hold: cmd::HoldArgs,
}

fn prepare(ctx: &mut Ctx, a: &PrepareArgs) -> Result<(), CliError> {
    let ecu = Ecu::resolve(&a.link.ecu, &ctx.g.defs_dir())?;
    let asx = cmd::assess(&ecu, &a.request, &a.set)?;
    let watch = a.hold.watch_read(&ecu)?;
    let mut j = cmd::assessment_json(&asx);
    j["event"] = json!("prepare");
    let mut words: Vec<String> = vec!["kline".into()];
    words.extend(ctx.g.port_cli());
    words.push("exec".into());
    words.extend(a.link.to_cli());
    words.push(hex(&asx.payload));
    words.extend(a.hold.to_cli(watch.as_ref()));
    let command = kline_host::shell_join(&words);
    match &asx.risk {
        Ok(r) if r.tier == Tier::Read => {
            j["note"] = json!("this is a read: kline-agent can run it with `read`");
            j["command"] = json!(null);
        }
        Ok(_) => {
            j["command"] = json!(command);
            j["note"] = json!(
                "only a human can run this, in an interactive terminal; kline-agent cannot send it"
            );
        }
        Err(_) => j["command"] = json!(null),
    }
    ctx.out.emit(j, "");
    asx.risk.map(drop)
}

fn main() -> anyhow::Result<()> {
    let mut env = Env::system();
    let stop = env.stop.clone();
    // Ctrl-C, SIGTERM and SIGHUP (ctrlc `termination`) end `live`/`scan` cleanly: the session
    // is stopped and the capture is complete; an init schedule stops before its next attempt. A
    // second one releases K and exits at once (final critique F05).
    ctrlc::set_handler(move || kline_host::transport::on_interrupt(&stop))
        .context("installing the Ctrl-C handler")?;
    if safety::APPROVALS_COMPILED {
        // Cargo unified kline-core's feature in from another member (e.g. a joint build with
        // kline-cli): approval types exist in this binary. Refuse to start (security re-check
        // finding 4); build it alone with `cargo build --release -p kline-agent`.
        env.stderr.line(
            "kline-agent: refusing to start: built with kline-core `approved-routines` unified in; rebuild it alone with `cargo build --release -p kline-agent`",
        );
        std::process::exit(i32::from(exit::REFUSED));
    }
    let code = match Cli::try_parse() {
        Ok(mut cli) => {
            cli.g.json = true;
            let g = cli.g.clone();
            let mut ctx = Ctx::new(&g, &mut env, "kline-agent");
            let r = match &cli.cmd {
                Command::Ports => cmd::ports(&mut ctx),
                Command::Connect(a) => cmd::connect(&mut ctx, a),
                Command::Ident(a) => cmd::ident(&mut ctx, a),
                Command::Read(a) => cmd::read(&mut ctx, a),
                Command::Live(a) => cmd::live(&mut ctx, a),
                Command::Dtc(a) => cmd::dtc(&mut ctx, a),
                Command::Risk(a) => cmd::risk(&mut ctx, a),
                Command::Prepare(a) => prepare(&mut ctx, a),
                Command::Replay(a) => cmd::replay(&mut ctx, a),
            };
            ctx.finish(r)
        }
        Err(e) => {
            let _ = e.print();
            if e.use_stderr() {
                exit::USAGE
            } else {
                exit::OK
            }
        }
    };
    std::process::exit(i32::from(code));
}
