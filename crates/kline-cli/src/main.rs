//! `kline` binary. See docs/cli.md.

use anyhow::Context;

fn main() -> anyhow::Result<()> {
    let mut env = kline_cli::CliEnv::system();
    let stop = env.host.stop.clone();
    // Ctrl-C, SIGTERM and SIGHUP (ctrlc `termination`) end `live`/`scan` cleanly: the session
    // is stopped and the capture is complete; an init schedule stops before its next attempt. A
    // second one releases K and exits at once (final critique F05).
    ctrlc::set_handler(move || kline_host::transport::on_interrupt(&stop))
        .context("installing the Ctrl-C handler")?;
    let code = kline_cli::run_args(std::env::args_os(), &mut env);
    std::process::exit(i32::from(code));
}
