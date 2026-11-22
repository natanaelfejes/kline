//! `kline-agent`: the K-line tool for AI use (docs/cli.md "kline-agent").
//!
//! Tier 0 only. Built against kline-core WITHOUT `approved-routines` (tests/no_approvals.rs
//! checks the resolved features), so no approval type exists in this binary when it is built on
//! its own. Non-read requests can only be *prepared*: `prepare` prints the RiskInfo and the exact
//! `kline exec` command a human runs at the terminal. Output is always JSON lines. `scan` is not
//! offered here (docs/protocol.md: a scan runs only as an explicit human command).

#![forbid(unsafe_code)]

use std::sync::atomic::Ordering;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use kline_core::safety::{self, Tier};
use kline_host::cmd::{
    self, Ctx, Env, GlobalArgs, LinkArgs, LiveArgs, ReadArgs, ReplayArgs, RiskArgs,
};
use kline_host::defs::Ecu;
use kline_host::error::{exit, CliError};
use kline_host::hex;
use serde_json::json;

#[derive(Debug, Parser)]
#[command(
    name = "kline-agent",
    version,
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
    Dtc(LinkArgs),
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
}

/// Quote one shell word if needed.
fn sh(w: &str) -> String {
    if !w.is_empty()
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=,@+".contains(c))
    {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', "'\\''"))
    }
}

fn prepare(ctx: &mut Ctx, a: &PrepareArgs) -> Result<(), CliError> {
    let ecu = Ecu::resolve(&a.link.ecu, &ctx.g.defs_dir())?;
    let asx = cmd::assess(&ecu, &a.request)?;
    let mut j = cmd::assessment_json(&asx);
    j["event"] = json!("prepare");
    let mut words: Vec<String> = vec!["kline".into()];
    words.extend(ctx.g.port_cli());
    words.push("exec".into());
    words.extend(a.link.to_cli());
    words.push(hex(&asx.payload));
    let command = words.iter().map(|w| sh(w)).collect::<Vec<_>>().join(" ");
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
    ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
        .context("installing the Ctrl-C handler")?;
    if safety::APPROVALS_COMPILED {
        // Cargo unified kline-core's feature in from another member (e.g. a joint build with
        // kline-cli). This binary still has no approval code path; build it alone to be sure.
        env.stderr.line(
            "kline-agent: warning: built with kline-core approved-routines unified in; build with `cargo build -p kline-agent` alone",
        );
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
