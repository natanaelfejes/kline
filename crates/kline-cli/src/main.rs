//! `kline` binary. See docs/cli.md.

use std::sync::atomic::Ordering;

use anyhow::Context;

fn main() -> anyhow::Result<()> {
    let mut env = kline_cli::CliEnv::system();
    let stop = env.host.stop.clone();
    // Ctrl-C ends `live`/`scan` cleanly: the session is stopped and the capture is complete.
    ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
        .context("installing the Ctrl-C handler")?;
    let code = kline_cli::run_args(std::env::args_os(), &mut env);
    std::process::exit(i32::from(code));
}
