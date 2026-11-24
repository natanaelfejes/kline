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
        // The transcript file is opened (appending, never truncating) only once the simulator
        // runs; until then nothing happens on the line.
        let transcript = Transcript::new(std::io::sink());
        let stdout_transcript = cfg.transcript.is_none();
        if stdout_transcript {
            transcript.redirect(std::io::stdout());
        }
        let run = match runner::start(&cfg, Some(transcript.clone())) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("kline-sim: {e}");
                return match e {
                    RunnerError::Usage(_) => 2,
                    RunnerError::Setup(_) => 3,
                };
            }
        };
        // After the control socket check: a second simulator writes nothing.
        let written = match &cfg.write_defs {
            Some(dir) => match kline_sim::builtin::write_defs(dir) {
                Ok(w) => Some(w),
                Err(e) => {
                    eprintln!("kline-sim: {e}");
                    return 3;
                }
            },
            None => None,
        };
        if let Some(p) = &cfg.transcript {
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
            {
                Ok(f) => transcript.redirect(f),
                Err(e) => {
                    eprintln!("kline-sim: transcript {}: {e}", p.display());
                    return 3;
                }
            }
        }
        let tty = run.sim.tty_path().display().to_string();
        let ctl = run.sim.control_path().display().to_string();
        let defs_dir = runner::defs_dir_in_use(&cfg);
        let defs = match &defs_dir {
            Some(d) => format!(" --defs {}", d.display()),
            None => " --defs DIR".to_string(),
        };
        println!("kline-sim: simulated K line on {tty}");
        println!("  control socket: {ctl}");
        for i in &run.info {
            println!("  ECU {i}");
        }
        if let (Some(dir), Some(w)) = (&cfg.write_defs, &written) {
            println!(
                "  definitions in {}: {} written, {} already there",
                dir.display(),
                w.written.len(),
                w.unchanged.len()
            );
        }
        if let Some(t) = &cfg.transcript {
            println!("  transcript (appended): {}", t.display());
        }
        if defs_dir.is_none() {
            println!(
                "  the built-in ECUs need their definitions for kline: start with --write-defs DIR"
            );
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
