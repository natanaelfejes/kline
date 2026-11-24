//! `kline-sim`: a simulated K line with ECUs behind a pty, for rehearsing a car session with
//! the `kline` binary at home (docs/sim.md "Running it"). macOS and Linux only.

#[cfg(unix)]
fn main() {
    std::process::exit(unix::main());
}

#[cfg(not(unix))]
fn main() {
    eprintln!("kline-sim: needs a Unix pty and socket (macOS or Linux); not available on Windows");
    std::process::exit(2);
}

#[cfg(unix)]
mod unix {
    use std::path::Path;

    use kline_sim::multi::Transcript;
    use kline_sim::runner::{self, Config, RunnerError, USAGE};

    pub fn main() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(String::as_str) == Some("ctl") {
            return ctl(&args[1..]);
        }
        let cfg = match Config::parse(args) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("kline-sim: {e}");
                return 2;
            }
        };
        if cfg.help {
            print!("{USAGE}");
            return 0;
        }
        if let Some(dir) = &cfg.write_defs {
            match kline_sim::builtin::write_defs(dir) {
                Ok(files) => {
                    for f in files {
                        println!("wrote {}", f.display());
                    }
                }
                Err(e) => {
                    eprintln!("kline-sim: {e}");
                    return 3;
                }
            }
        }
        let transcript = match &cfg.transcript {
            Some(p) => match std::fs::File::create(p) {
                Ok(f) => Transcript::new(f),
                Err(e) => {
                    eprintln!("kline-sim: transcript {}: {e}", p.display());
                    return 3;
                }
            },
            None => Transcript::new(std::io::stdout()),
        };
        let run = match runner::start(&cfg, Some(transcript)) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("kline-sim: {e}");
                return match e {
                    RunnerError::Usage(_) => 2,
                    RunnerError::Setup(_) => 3,
                };
            }
        };
        let tty = run.sim.tty_path().display().to_string();
        let ctl = run.sim.control_path().display().to_string();
        let defs = cfg
            .write_defs
            .as_ref()
            .map(|d| format!(" --defs {}", d.display()))
            .unwrap_or_default();
        println!("kline-sim: simulated K line on {tty}");
        println!("  control socket: {ctl}");
        for i in &run.info {
            println!("  ECU {i}");
        }
        if let Some(t) = &cfg.transcript {
            println!("  transcript: {}", t.display());
        }
        println!("run kline against it:");
        println!("  kline --port {tty} --sim-control {ctl}{defs} <command> ..");
        println!("take an ECU off the line and back:");
        println!("  kline-sim ctl {ctl} ECU 26 OFF   (ON to bring it back)");
        println!("Ctrl-C stops the simulator.");
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    fn ctl(args: &[String]) -> i32 {
        let [socket, cmd @ ..] = args else {
            eprintln!("usage: kline-sim ctl SOCKET COMMAND..");
            return 2;
        };
        if cmd.is_empty() {
            eprintln!("usage: kline-sim ctl SOCKET COMMAND..");
            return 2;
        }
        match runner::send_control(Path::new(socket), &cmd.join(" ")) {
            Ok(r) if r == "ERR" || r.is_empty() => {
                eprintln!("kline-sim: the simulator refused {:?}", cmd.join(" "));
                1
            }
            Ok(r) => {
                println!("{r}");
                0
            }
            Err(e) => {
                eprintln!("kline-sim: {socket}: {e}");
                3
            }
        }
    }
}
