//! Rehearsal of the owner's third car session against the `kline-sim` runner: every `kline`
//! command of the session script, in order, against the simulated car on a pty, with the
//! exit code each one should have (docs/sim.md "Rehearsing a session").
//!
//! - The line is the runner's own (`kline_sim::runner`, the code behind the `kline-sim`
//!   binary): the four built-in ECUs, the built-in definitions written by `--write-defs` under
//!   the car's file names (no Renault file), lenient timing.
//! - Reads, `risk`, `ports`, `selftest` and `--version` run the real `kline` binary.
//! - `exec` needs a human at a terminal to type the confirmation code. Here it runs the same
//!   command logic in-process (`kline_cli::run_args`) over the same pty, with the test terminal
//!   of `common` typing the code shown (`Human::TypesShownCode`); the safety checks, the
//!   approval and the code are the real ones, nothing is weakened.
//! - Long durations are shortened (`--duration`, `--hold` at most [`MAX_SECONDS`]); the owner
//!   runs them in full at home.
//! - The script is a copy of the session-3 command list (`SESSION3`); set
//!   `KLINE_SESSION_COMMANDS=FILE` to run another list in the same format (`## Section`
//!   headers, `# comments`, one `kline ...` command per line).
//!
//! One test per section, each with its own simulator, so the sections run in parallel.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use common::{temp_dir, FakeTty, Human, TempDir};
use kline_cli::{CliEnv, Prompt};
use kline_host::cmd::Env;
use kline_host::out::Sink;
use kline_sim::runner::{self, Config, Running};

/// Longest `--duration` / `--hold` in the test (seconds); the script's own values are longer.
const MAX_SECONDS: u64 = 2;

/// The session-3 command list (owner's script, 2026-11-24), comments translated.
const SESSION3: &str = r#"
## V0
kline ports
kline read --ecu bmt 21A1 21A3 21A4 21A8
kline live --ecu bmt --frames 21A3 --rate 20 --compact --duration 60
kline live --ecu bmt --frames 21A4 --rate 20 --compact --duration 600
kline read --ecu bmt 21A8

## Home
kline --version
kline selftest
kline risk --ecu bmt 31A00001 --hold 45 --watch 21A3 --watch-interval 50
kline risk --ecu bmt 31A10001 --hold 45 --watch 21A3 --watch-interval 50
kline risk --ecu sirius 30940001
kline risk --ecu sirius 30941100
kline risk --ecu sirius 2701
kline risk --ecu sirius 2300000001
kline risk --ecu sirius 230000000001

## B
kline ports
kline ident --ecu bmt
kline read --ecu bmt 21A1 21A3 21A4 21A7 21A8 21A2
kline dtc --ecu bmt
kline live --ecu bmt --frames 21A3,21A7 --rate 5 --compact --duration 120

## C
kline read --ecu bmt 21A2
kline read --ecu bmt 21A3 21A7
kline exec --ecu bmt 31A00001 --hold 45 --watch 21A3 --watch-interval 50
kline live --ecu bmt --frames 21A3,21A0 --rate 5 --compact --duration 90
kline exec --ecu bmt 31A10001 --hold 45 --watch 21A3 --watch-interval 50
kline live --ecu bmt --frames 21A3 --rate 5 --compact --duration 90
kline exec --ecu bmt 31A00001 --hold 20 --watch 21A3 --watch-interval 50
kline exec --ecu bmt 31A10001 --hold 45 --watch 21A3 --watch-interval 50
# C13 reserve only:
kline exec --ecu bmt 31A00001 --hold 15 --watch 21A7 --watch-interval 50
kline exec --ecu bmt 31A10001 --hold 45 --watch 21A7 --watch-interval 50
kline live --ecu bmt --frames 21A3 --rate 5 --compact --duration 40

## D
kline ident --xml "$AB70" --xml "$AB80"
kline dtc --xml "$AB70"
kline dtc --xml "$AB80"
kline read --xml "$AB70" 2180 2183 2184 2185
kline read --xml "$AB80" 2180 2185 2186 2189
kline ident --xml "$ABS" --variant single
kline dtc --xml "$ABS" --variant single
kline dtc --ecu sirius
kline live --ecu sirius --frames 21A9 --rate 2 --duration 20

## K
kline live --ecu sirius --frames 21AA,21A9 --rate 8 --duration 900
kline dtc --xml "$ABS" --variant single

## F
kline live --ecu bmt --frames 21A3 --rate 20 --compact --duration 90
kline live --ecu bmt --frames 21A4 --rate 20 --compact --duration 600
kline live --ecu bmt --frames 21A4 --rate 20 --compact --duration 600
kline live --ecu bmt --frames 21A4 --rate 20 --compact --duration 300
kline read --ecu bmt 21A8

## E1
kline read --ecu sirius 21A9
kline exec --ecu sirius 30940001 --stop 30941100

## H
kline dtc --ecu sirius
kline dtc --ecu bmt
kline dtc --xml "$AB70"
kline read --xml "$AB70" 2185

## J
kline dtc --xml "$AB70"
kline dtc --xml "$AB80"
kline read --xml "$AB70" 2185
kline read --ecu sirius 21A9
kline exec --ecu sirius 30940001 --stop 30941100
kline dtc --ecu sirius
kline dtc --ecu bmt
kline read --ecu bmt 21A4
kline read --ecu sirius 21A9

## N (optional, last; never 2702, never an address other than 0)
kline read --ecu sirius 21A9
kline exec --ecu sirius 2701
# only after 7F 27 80 or 7F 27 22:
kline exec --ecu sirius --session none 2701
kline exec --ecu sirius 2300000001
# only after 7F 23 13:
kline exec --ecu sirius 230000000001
kline dtc --ecu sirius

## M
kline live --ecu bmt --frames 21A3 --rate 5 --compact --duration 40
ls -la "$KLINE_CAPTURE_DIR"
"#;

/// How the simulated Sirius answers the expert requests of section N (the script branches
/// on these answers; the real ECU's answers are unknown).
const SIM_ARGS: &[&str] = &[
    "--answer",
    "7A:2701=nrc:22",
    "--answer",
    "7A:2300000001=nrc:13",
    "--answer",
    "7A:230000000001=nrc:12",
];

/// Exit code each command should have against the simulated car (docs/cli.md exit codes).
/// Default 0.
fn expected(section: &str, cmd: &str) -> u8 {
    // Section N: the simulated Sirius answers 2701 / 23 .. negatively (SIM_ARGS): exit 6.
    if section.starts_with('N') && cmd.contains(" exec ") {
        return 6;
    }
    0
}

struct Sim {
    run: Running,
    defs: TempDir,
    caps: TempDir,
}

fn sim(tag: &str, extra: &[&str]) -> Option<Sim> {
    let defs = temp_dir(&format!("rehearsal-defs-{tag}"));
    let caps = temp_dir(&format!("rehearsal-caps-{tag}"));
    let ctl =
        std::env::temp_dir().join(format!("kline-rehearsal-{tag}-{}.ctl", std::process::id()));
    kline_sim::builtin::write_defs(defs.path()).unwrap();
    let mut args: Vec<String> = vec!["--control".into(), ctl.display().to_string()];
    args.extend(SIM_ARGS.iter().chain(extra).map(|s| s.to_string()));
    let cfg = Config::parse(args).unwrap();
    match runner::start(&cfg, None) {
        Ok(run) => Some(Sim { run, defs, caps }),
        Err(e) => {
            eprintln!("SKIPPED: pty not available here ({e})");
            None
        }
    }
}

impl Sim {
    fn prefix(&self) -> Vec<String> {
        vec![
            "--port".into(),
            self.run.sim.tty_path().display().to_string(),
            "--sim-control".into(),
            self.run.sim.control_path().display().to_string(),
            "--defs".into(),
            self.defs.display().to_string(),
            "--capture-dir".into(),
            self.caps.display().to_string(),
        ]
    }

    fn var(&self, name: &str) -> String {
        let file = match name {
            "AB70" => "AB70_X65.xml",
            "AB80" => "AB80_X65_X90_V1.xml",
            "ABS" => "X24_ABS_Bosch5.3.xml",
            "KLINE_CAPTURE_DIR" => return self.caps.display().to_string(),
            _ => panic!("unknown variable ${name}"),
        };
        self.defs.join(file).display().to_string()
    }
}

/// Split a script line like a shell would for these commands: words, double quotes, `$VAR`.
fn words(line: &str, var: impl Fn(&str) -> String) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                in_word = true;
            }
            '$' => {
                let mut name = String::new();
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_alphanumeric() || n == '_' {
                        name.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                cur.push_str(&var(&name));
                in_word = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

/// `--duration` and `--hold` capped at [`MAX_SECONDS`].
fn shorten(args: &mut [String]) {
    for i in 1..args.len() {
        if matches!(args[i - 1].as_str(), "--duration" | "--hold") {
            if let Ok(n) = args[i].parse::<u64>() {
                args[i] = n.min(MAX_SECONDS).to_string();
            }
        }
    }
}

fn run_binary(prefix: &[String], args: &[String]) -> (u8, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_kline"))
        .args(prefix)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    (o.status.code().unwrap_or(-1) as u8, text)
}

/// `kline exec` in-process with the test terminal typing the code it shows, over the pty.
fn run_exec(prefix: &[String], args: &[String]) -> (u8, String) {
    let (out, out_buf) = Sink::buffer();
    let (err, err_buf) = Sink::buffer();
    let screen = Arc::new(Mutex::new(String::new()));
    let s2 = screen.clone();
    let mut host = Env::system();
    host.stdout = out;
    host.stderr = err;
    let mut env = CliEnv {
        host,
        interactive: true,
        tty: Box::new(move || {
            Ok(Box::new(FakeTty {
                human: Human::TypesShownCode,
                screen: s2.clone(),
                asks: 0,
            }) as Box<dyn Prompt>)
        }),
    };
    let mut full = vec!["kline".to_string()];
    full.extend_from_slice(prefix);
    full.extend_from_slice(args);
    let code = kline_cli::run_args(full, &mut env);
    let s = |b: &Arc<Mutex<Vec<u8>>>| String::from_utf8_lossy(&b.lock().unwrap()).into_owned();
    let text = format!("{}{}{}", screen.lock().unwrap(), s(&out_buf), s(&err_buf));
    (code, text)
}

/// The commands of one section of the script.
fn section(name: &str) -> Vec<String> {
    let script = match std::env::var_os("KLINE_SESSION_COMMANDS") {
        Some(p) => std::fs::read_to_string(PathBuf::from(p)).unwrap(),
        None => SESSION3.to_string(),
    };
    let mut cur = String::new();
    let mut out = Vec::new();
    for l in script.lines() {
        let l = l.trim();
        if let Some(h) = l.strip_prefix("## ") {
            cur = h.to_string();
            continue;
        }
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if cur.split_whitespace().next() == Some(name) {
            out.push(l.to_string());
        }
    }
    out
}

/// Run every command of `name` against a fresh simulator and check the exit codes.
fn rehearse(name: &str) -> Option<Sim> {
    let sim = sim(&name.to_ascii_lowercase(), &[])?;
    let cmds = section(name);
    assert!(!cmds.is_empty(), "no commands in section {name}");
    let prefix = sim.prefix();
    let mut failures = Vec::new();
    for line in &cmds {
        let mut w = words(line, |v| sim.var(v));
        match w.first().map(String::as_str) {
            Some("kline") => {}
            Some("ls") => {
                // The captures of this section are there.
                let p = Path::new(&w[w.len() - 1]);
                let n = std::fs::read_dir(p).map(|d| d.count()).unwrap_or(0);
                if n == 0 {
                    failures.push(format!("{line}: no capture files in {}", p.display()));
                }
                continue;
            }
            _ => panic!("not a kline command: {line}"),
        }
        let mut args = w.split_off(1);
        shorten(&mut args);
        let (code, text) = if args.iter().any(|a| a == "exec") {
            run_exec(&prefix, &args)
        } else {
            run_binary(&prefix, &args)
        };
        let want = expected(name, &format!(" {} ", args.join(" ")));
        eprintln!(
            "[{name}] exit {code} (want {want}): kline {}",
            args.join(" ")
        );
        if code != want {
            failures.push(format!("{line}: exit {code}, want {want}\n{text}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n----\n"));
    Some(sim)
}

fn no_writes_except(sim: &Sim, allowed: &[&[u8]]) {
    for (addr, name, j) in &sim.run.journals {
        for w in j.writes() {
            assert!(
                allowed.contains(&w.payload.as_slice()),
                "unexpected write to {addr:02X} {name}: {w}"
            );
        }
    }
}

#[test]
fn v0_reads_only() {
    if let Some(s) = rehearse("V0") {
        no_writes_except(&s, &[]);
    }
}

#[test]
fn home_risk_and_selftest() {
    if let Some(s) = rehearse("Home") {
        no_writes_except(&s, &[]);
    }
}

#[test]
fn b_bmt_reads() {
    if let Some(s) = rehearse("B") {
        no_writes_except(&s, &[]);
    }
}

#[test]
fn c_bmt_lock_outputs() {
    if let Some(s) = rehearse("C") {
        let bmt = &s.run.journals.iter().find(|j| j.0 == 0x26).unwrap().2;
        let sent: Vec<Vec<u8>> = bmt.writes().into_iter().map(|w| w.payload).collect();
        let lock = vec![0x31, 0xA0, 0x00, 0x01];
        let unlock = vec![0x31, 0xA1, 0x00, 0x01];
        assert_eq!(
            sent,
            vec![
                lock.clone(),
                unlock.clone(),
                lock.clone(),
                unlock.clone(),
                lock,
                unlock
            ]
        );
        no_writes_except(&s, &[&[0x31, 0xA0, 0x00, 0x01], &[0x31, 0xA1, 0x00, 0x01]]);
        // The car's model: the lock memory did not follow the K-line outputs.
        assert_eq!(
            s.run.control.command("LOCK 26?").as_deref(),
            Some("UNLOCKED")
        );
    }
}

#[test]
fn d_airbag_abs_sirius() {
    if let Some(s) = rehearse("D") {
        no_writes_except(&s, &[]);
    }
}

#[test]
fn k_long_live_and_abs() {
    if let Some(s) = rehearse("K") {
        no_writes_except(&s, &[]);
    }
}

#[test]
fn f_bmt_live() {
    if let Some(s) = rehearse("F") {
        no_writes_except(&s, &[]);
    }
}

#[test]
fn e1_sirius_output_test() {
    if let Some(s) = rehearse("E1") {
        no_writes_except(&s, &[&[0x30, 0x94, 0x00, 0x01], &[0x30, 0x94, 0x11, 0x00]]);
    }
}

#[test]
fn h_fault_reads() {
    if let Some(s) = rehearse("H") {
        no_writes_except(&s, &[]);
    }
}

#[test]
fn j_after_repair() {
    if let Some(s) = rehearse("J") {
        no_writes_except(&s, &[&[0x30, 0x94, 0x00, 0x01], &[0x30, 0x94, 0x11, 0x00]]);
    }
}

#[test]
fn n_sirius_expert_requests() {
    if let Some(s) = rehearse("N") {
        no_writes_except(
            &s,
            &[
                &[0x27, 0x01],
                &[0x23, 0x00, 0x00, 0x00, 0x01],
                &[0x23, 0x00, 0x00, 0x00, 0x00, 0x01],
            ],
        );
    }
}

#[test]
fn m_last_live_and_captures() {
    if let Some(s) = rehearse("M") {
        no_writes_except(&s, &[]);
    }
}

/// `kline --port .. <args>` against `sim`, real binary.
fn kline(sim: &Sim, args: &[&str]) -> (u8, String) {
    let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    run_binary(&sim.prefix(), &a)
}

fn kline_exec(sim: &Sim, args: &[&str]) -> (u8, String) {
    let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    run_exec(&sim.prefix(), &a)
}

#[test]
fn a_lock_memory_that_follows_the_k_line_outputs() {
    let Some(s) = sim("follows", &["--bmt-lock-follows", "true"]) else {
        return;
    };
    let (c, t) = kline(&s, &["read", "--ecu", "bmt", "21A3"]);
    assert_eq!(c, 0, "{t}");
    assert!(t.contains("unlocked"), "{t}");
    let (c, t) = kline_exec(&s, &["exec", "--ecu", "bmt", "31A00001"]);
    assert_eq!(c, 0, "{t}");
    assert_eq!(s.run.control.command("LOCK 26?").as_deref(), Some("LOCKED"));
    let (c, t) = kline(&s, &["read", "--ecu", "bmt", "21A3"]);
    assert_eq!(c, 0, "{t}");
    assert!(
        t.lines()
            .any(|l| l.contains("Lock state") && l.contains(" locked")),
        "{t}"
    );
    let (c, t) = kline_exec(&s, &["exec", "--ecu", "bmt", "31A10001"]);
    assert_eq!(c, 0, "{t}");
    assert_eq!(
        s.run.control.command("LOCK 26?").as_deref(),
        Some("UNLOCKED")
    );
    // The dashboard button, over the control socket.
    let ctl = s.run.sim.control_path().to_path_buf();
    assert_eq!(runner::send_control(&ctl, "LOCK 26 LOCKED").unwrap(), "OK");
    let (_, t) = kline(&s, &["read", "--ecu", "bmt", "21A3"]);
    assert!(
        t.lines()
            .any(|l| l.contains("Lock state") && l.contains(" locked")),
        "{t}"
    );
}

#[test]
fn an_ecu_goes_off_the_line_and_comes_back() {
    let Some(s) = sim("presence", &[]) else {
        return;
    };
    let ctl = s.run.sim.control_path().to_path_buf();
    assert_eq!(runner::send_control(&ctl, "ECU 26 OFF").unwrap(), "OK");
    assert!(runner::send_control(&ctl, "ECUS?")
        .unwrap()
        .contains("26 bmt off"));
    let (c, t) = kline(
        &s,
        &["--init-attempts", "1", "read", "--ecu", "bmt", "21A3"],
    );
    assert_eq!(c, 4, "no ECU answers, every init fails: {t}");
    // The other ECUs are still there.
    let (c, t) = kline(&s, &["read", "--ecu", "sirius", "21A9"]);
    assert_eq!(c, 0, "{t}");
    assert_eq!(runner::send_control(&ctl, "ECU 26 ON").unwrap(), "OK");
    let (c, t) = kline(&s, &["read", "--ecu", "bmt", "21A3"]);
    assert_eq!(c, 0, "{t}");
}

#[test]
fn injected_faults_reach_the_cli() {
    let Some(s) = sim(
        "faults",
        &[
            "--fault",
            "26:21A3=silent,after=1",
            "--fault",
            "26:21A7=nrc:22",
            "--fault",
            "2C:init=silent",
        ],
    ) else {
        return;
    };
    // First 21 A3 answered, the second not: a line error, exit 5.
    let (c, t) = kline(&s, &["read", "--ecu", "bmt", "21A3", "21A3"]);
    assert_eq!(c, 5, "{t}");
    let (c, t) = kline(&s, &["read", "--ecu", "bmt", "21A7"]);
    assert_eq!(c, 6, "{t}");
    assert!(t.contains("7F 21 22"), "{t}");
    let ab70 = s.var("AB70");
    let (c, t) = kline(
        &s,
        &["--init-attempts", "1", "read", "--xml", &ab70, "2180"],
    );
    assert_eq!(c, 4, "the airbag never answers the wake-up: {t}");
}

#[test]
fn a_capture_replays_as_the_ecu() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../kline-sim/tests/fixtures");
    let cap = fixtures.join("bmt_21a3.jsonl").display().to_string();
    let Some(s) = sim("replay", &["--replay", &format!("26={cap}")]) else {
        return;
    };
    let def = fixtures.join("bmt_min.xml").display().to_string();
    let (c, t) = kline(
        &s,
        &[
            "--trace",
            "read",
            "--xml",
            &def,
            "--variant",
            "single",
            "--session",
            "none",
            "21A3",
        ],
    );
    assert_eq!(c, 0, "{t}");
    assert!(t.contains("61 A3 05 02"), "{t}");
}
