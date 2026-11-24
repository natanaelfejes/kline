//! The `kline-sim` runner: several ECUs on one line, the control commands, answers and
//! faults from the command line, the BMT lock memory, the ABS keep-alive window, the
//! transcript, `--write-defs`, and the binary itself.
#![cfg(unix)]

mod common;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use common::{cfg, positive};
use kline_core::frame::Reply;
use kline_core::session::SessionOutcome;
use kline_sim::builtin::{Builtin, BUILTINS};
use kline_sim::driver::Driver;
use kline_sim::line::VirtualLine;
use kline_sim::multi::Transcript;
use kline_sim::runner::{Built, Config};

fn config(args: &[&str]) -> Config {
    Config::parse(args.iter().map(|s| s.to_string())).unwrap()
}

fn build(args: &[&str]) -> Built {
    config(args).build(None).unwrap()
}

/// Shared buffer a [`Transcript`] writes into.
#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Buf {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

fn read_once(d: &mut Driver<VirtualLine>, ecu: &Builtin, payload: &[u8]) -> SessionOutcome {
    let init = d.init(&cfg(&ecu.def())).unwrap();
    let mut s = d.session(init);
    let r = d.request(&mut s, payload).unwrap();
    d.stop(&mut s).unwrap();
    d.line.advance_ms(1_000);
    r
}

fn b(name: &str) -> &'static Builtin {
    Builtin::by_name(name).unwrap()
}

#[test]
fn the_whole_car_answers_on_one_line() {
    let built = build(&[]);
    assert_eq!(built.info.len(), 4, "{:?}", built.info);
    let mut d = Driver::new(VirtualLine::from_bus(built.bus));
    let r = positive(&read_once(&mut d, b("sirius"), &[0x21, 0xA9]));
    assert_eq!(&r[..2], &[0x61, 0xA9]);
    let r = positive(&read_once(&mut d, b("bmt"), &[0x21, 0xA3]));
    assert_eq!(r[2] & 0x80, 0x80, "lock memory starts unlocked: {r:02X?}");
    let r = positive(&read_once(&mut d, b("airbag"), &[0x21, 0x89]));
    assert_eq!(&r[..2], &[0x61, 0x89]);
    let r = positive(&read_once(&mut d, b("abs"), &[0x58]));
    assert_eq!(r[1], 0x08, "the car's stored fault code 8: {r:02X?}");
    for (_, _, j) in &built.journals {
        j.assert_no_writes();
    }
}

#[test]
fn every_builtin_definition_decodes_its_own_answers() {
    for bi in &BUILTINS {
        let def = bi.def();
        let mut d = Driver::new(VirtualLine::from_bus(build(&["--ecu", bi.name]).bus));
        let init = d.init(&cfg(&def)).unwrap();
        let mut s = d.session(init);
        let mut n = 0;
        for req in &def.requests {
            let p = &req.sent_bytes.0;
            let read = (p.len() == 2 && p[0] == 0x21) || matches!(p.as_slice(), [0x51] | [0x58]);
            if !read {
                continue;
            }
            let r = positive(&d.request(&mut s, p).unwrap());
            req.decode_checked(&def, &r)
                .unwrap_or_else(|e| panic!("{} {}: {e}", bi.name, req.name));
            n += 1;
        }
        assert!(n >= 2, "{}: {n} reads", bi.name);
    }
}

#[test]
fn answers_and_faults_from_the_command_line() {
    let built = build(&[
        "--ecu",
        "bmt",
        "--answer",
        "26:21A1=61A1FF",
        "--fault",
        "26:21A3=nrc:22,after=1,times=1",
    ]);
    let mut d = Driver::new(VirtualLine::from_bus(built.bus));
    let init = d.init(&cfg(&b("bmt").def())).unwrap();
    let mut s = d.session(init);
    assert_eq!(
        positive(&d.request(&mut s, &[0x21, 0xA1]).unwrap()),
        vec![0x61, 0xA1, 0xFF]
    );
    assert!(matches!(
        d.request(&mut s, &[0x21, 0xA3]).unwrap(),
        SessionOutcome::Reply(Reply::Positive(_))
    ));
    assert!(
        matches!(
            d.request(&mut s, &[0x21, 0xA3]),
            Ok(SessionOutcome::Reply(Reply::Negative { nrc: 0x22, .. })) | Err(_)
        ),
        "second 21 A3 answered 7F 21 22"
    );
    assert!(matches!(
        d.request(&mut s, &[0x21, 0xA3]).unwrap(),
        SessionOutcome::Reply(Reply::Positive(_))
    ));
}

#[test]
fn an_init_fault_silences_one_ecu_only() {
    let built = build(&["--fault", "26:init=silent"]);
    let mut d = Driver::new(VirtualLine::from_bus(built.bus));
    assert!(d.init(&cfg(&b("bmt").def())).is_err());
    d.line.advance_ms(1_000);
    positive(&read_once(&mut d, b("sirius"), &[0x21, 0xA9]));
}

/// The ABS stays awake through the definition's 7000 ms keep-alive interval and sleeps after
/// it plus 1 s of silence.
#[test]
fn the_abs_keeps_its_session_through_the_7000_ms_keep_alive() {
    let built = build(&["--ecu", "abs"]);
    let mut d = Driver::new(VirtualLine::from_bus(built.bus));
    let init = d.init(&cfg(&b("abs").def())).unwrap();
    let mut s = d.session(init);
    positive(&d.request(&mut s, &[0x58]).unwrap());
    d.line.advance_ms(7_000);
    positive(&d.request(&mut s, &[0x58]).unwrap());
    // The boundary: iso8_session_timeout_ms(7000) = 8000 ms after the ECU's last byte. The
    // request returns once the reply is read, so the silence starts a little before `now`.
    assert_eq!(kline_sim::ecu::iso8_session_timeout_ms(7_000), 8_000);
    d.line.advance_ms(7_950);
    positive(&d.request(&mut s, &[0x58]).unwrap());
    d.line.advance_ms(8_050);
    assert!(
        !matches!(
            d.request(&mut s, &[0x58]),
            Ok(SessionOutcome::Reply(Reply::Positive(_)))
        ),
        "asleep after 8050 ms of silence"
    );
}

#[test]
fn ecus_go_off_and_on_and_the_transcript_says_so() {
    let buf = Buf::default();
    let built = config(&[])
        .build(Some(Transcript::new(buf.clone())))
        .unwrap();
    let ctl = built.control.clone();
    let mut d = Driver::new(VirtualLine::from_bus(built.bus));
    assert_eq!(ctl.command("ecu 26 off").as_deref(), Some("OK"));
    assert!(ctl.command("ECUS?").unwrap().contains("26 bmt off"));
    assert!(d.init(&cfg(&b("bmt").def())).is_err());
    d.line.advance_ms(1_000);
    assert_eq!(ctl.command("ECU 26 ON").as_deref(), Some("OK"));
    positive(&read_once(&mut d, b("bmt"), &[0x21, 0xA3]));
    let t = buf.text();
    for want in [
        "ECU 26 bmt gone",
        "the ECU at this address is off the line",
        "ECU 26 bmt back on the line",
        "WAKE 5-baud",
        "TX 26 <- 21 A3",
        "RX 26 bmt -> ",
    ] {
        assert!(t.contains(want), "{want:?} missing from:\n{t}");
    }
}

#[test]
fn the_dashboard_button_and_the_k_line_outputs() {
    // The car's model (default): the memory follows the button only.
    let built = build(&["--ecu", "bmt", "--bmt-lock-initial", "locked"]);
    let ctl = built.control.clone();
    assert_eq!(ctl.command("LOCK 26?").as_deref(), Some("LOCKED"));
    let mut d = Driver::new(VirtualLine::from_bus(built.bus));
    let r = positive(&read_once(&mut d, b("bmt"), &[0x21, 0xA3]));
    assert_eq!(r[2] & 0x80, 0);
    assert_eq!(ctl.command("LOCK 26 UNLOCKED").as_deref(), Some("OK"));
    let r = positive(&read_once(&mut d, b("bmt"), &[0x21, 0xA3]));
    assert_eq!(r[2] & 0x80, 0x80);
    assert_eq!(
        ctl.command("LOCK 7A?"),
        None,
        "no lock memory on the Sirius"
    );
    assert_eq!(ctl.command("FLY 26"), None);
}

#[test]
fn bad_command_lines_are_refused() {
    let parse = |a: &[&str]| Config::parse(a.iter().map(|s| s.to_string()));
    for bad in [
        &["--answer", "26:21A1"][..],
        &["--answer", "ZZ:21A1=positive"],
        &["--fault", "26:21A3=explode"],
        &["--fault", "26:21A3=nrc"],
        &["--fault", "26:init=silent,after=2"],
        &["--bmt-lock-follows", "maybe"],
        &["--ecu"],
        &["--frobnicate"],
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
    for bad in [
        &["--ecu", "bmt", "--ecu", "26=builtin:bmt"][..],
        &["--ecu", "bmt", "--answer", "7A:2701=nrc:22"],
        &["--ecu", "7A=builtin:bmt"],
    ] {
        assert!(config(bad).build(None).is_err(), "{bad:?}");
    }
    let cap = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/bmt_21a3.jsonl")
        .display()
        .to_string();
    let replay = format!("26={cap}");
    assert!(config(&["--replay", &replay, "--fault", "26:*=silent"])
        .build(None)
        .is_err());
    assert!(config(&["--replay", &replay]).build(None).is_ok());
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("kline-sim-runner-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn write_defs_never_overwrites() {
    let d = scratch("defs");
    let files = kline_sim::builtin::write_defs(&d).unwrap();
    assert_eq!((files.written.len(), files.unchanged.len()), (5, 0));
    // The same directory again: nothing to do, not an error.
    let again = kline_sim::builtin::write_defs(&d).unwrap();
    assert_eq!((again.written.len(), again.unchanged.len()), (0, 5));
    assert!(d.join("BMT_X65_ph1_p.xml").exists());
    std::fs::write(d.join("AB70_X65.xml"), "mine").unwrap();
    std::fs::remove_file(d.join("BMT_X65_ph1_p.xml")).unwrap();
    assert!(kline_sim::builtin::write_defs(&d).is_err());
    assert!(!d.join("BMT_X65_ph1_p.xml").exists(), "nothing written");
    assert_eq!(
        std::fs::read_to_string(d.join("AB70_X65.xml")).unwrap(),
        "mine"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn write_defs_refuses_other_definitions_and_symlinks() {
    let d = scratch("defs-other");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("SOMETHING_ELSE.xml"), "<x/>").unwrap();
    assert!(kline_sim::builtin::write_defs(&d).is_err());
    assert!(!d.join("BMT_X65_ph1_p.xml").exists(), "nothing written");
    let _ = std::fs::remove_dir_all(&d);

    let real = scratch("defs-real");
    std::fs::create_dir_all(&real).unwrap();
    let link = scratch("defs-link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(kline_sim::builtin::write_defs(&link).is_err());
    assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
    let _ = std::fs::remove_file(&link);

    // A dangling link under one of the car's names: refused, nothing created behind it.
    std::os::unix::fs::symlink(real.join("elsewhere.xml"), real.join("BMT_X65_ph1_p.xml")).unwrap();
    assert!(kline_sim::builtin::write_defs(&real).is_err());
    assert!(!real.join("elsewhere.xml").exists());
    assert!(!real.join("AB70_X65.xml").exists());
    let _ = std::fs::remove_dir_all(&real);
}

/// An ECU taken off the line mid-reply: the bytes it had not sent yet never arrive.
#[test]
fn ecu_off_drops_the_reply_still_queued() {
    use kline_sim::line::Line;
    let built = build(&["--ecu", "bmt"]);
    let ctl = built.control.clone();
    let mut d = Driver::new(VirtualLine::from_bus(built.bus));
    let init = d.init(&cfg(&b("bmt").def())).unwrap();
    let _s = d.session(init);
    d.line.advance_ms(100);
    d.line.flush_rx().unwrap();
    // 21 A3 to the BMT; its reply starts P2 = 25 ms after the request.
    d.line
        .write(&[0x82, 0x26, 0xF1, 0x21, 0xA3, 0x5D], 0)
        .unwrap();
    let echo = d.line.read(6, 5).unwrap();
    assert_eq!(echo.len(), 6, "echo {echo:02X?}");
    let t = d.line.clock_ms();
    assert_eq!(
        ctl.command_on("ECU 26 OFF", d.line.bus_mut(), t).as_deref(),
        Some("OK")
    );
    let late = d.line.read(16, 200).unwrap();
    assert!(late.is_empty(), "reply bytes after OFF: {late:02X?}");
}

#[test]
fn the_binary_prints_its_port_and_answers_ctl() {
    let bin = env!("CARGO_BIN_EXE_kline-sim");
    let o = Command::new(bin).arg("--help").output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).contains("--bmt-lock-follows"));
    let o = Command::new(bin).arg("--nope").output().unwrap();
    assert_eq!(o.status.code(), Some(2));

    let dir = scratch("bin");
    std::fs::create_dir_all(&dir).unwrap();
    let ctl = dir.join("sim.ctl");
    std::fs::write(dir.join("t.log"), "earlier session\n").unwrap();
    let mut child = Command::new(bin)
        .args(["--control"])
        .arg(&ctl)
        .arg("--transcript")
        .arg(dir.join("t.log"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let first = lines.next().unwrap().unwrap();
    let Some(tty) = first.strip_prefix("kline-sim: simulated K line on ") else {
        let _ = child.kill();
        let _ = child.wait();
        let mut err = String::new();
        let _ = std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err);
        if err.contains("pty") {
            eprintln!("SKIPPED: pty not available here ({err})");
            return;
        }
        panic!("banner: {first:?} {err}");
    };
    assert!(std::path::Path::new(tty).exists(), "{tty}");
    let rest: Vec<String> = lines
        .by_ref()
        .take_while(|l| l.as_ref().is_ok_and(|l| !l.starts_with("Ctrl-C")))
        .map(|l| l.unwrap())
        .collect();
    let all = rest.join("\n");
    assert!(
        all.contains(&format!(
            "kline --port {tty} --sim-control {}",
            ctl.display()
        )),
        "{all}"
    );
    let o = Command::new(bin)
        .arg("ctl")
        .arg(&ctl)
        .args(["ECU", "26", "OFF"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    let o = Command::new(bin)
        .arg("ctl")
        .arg(&ctl)
        .arg("ECUS?")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("26 bmt off") && out.contains("7A sirius on"),
        "{out}"
    );
    let o = Command::new(bin)
        .arg("ctl")
        .arg(&ctl)
        .arg("WHAT")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    // A --control path that is a file is refused and left alone.
    let file = dir.join("not-a-socket");
    std::fs::write(&file, "keep").unwrap();
    let mut c = Command::new(bin)
        .arg("--control")
        .arg(&file)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let t0 = std::time::Instant::now();
    let status = loop {
        if let Some(st) = c.try_wait().unwrap() {
            break Some(st);
        }
        if t0.elapsed() > std::time::Duration::from_secs(10) {
            c.kill().unwrap();
            let _ = c.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(status.and_then(|s| s.code()), Some(3), "refused at once");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep");
    // A second simulator on the same socket is refused while the first runs, and writes
    // no definitions.
    let o = Command::new(bin)
        .arg("--write-defs")
        .arg(dir.join("defs2"))
        .arg("--control")
        .arg(&ctl)
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!dir.join("defs2").exists(), "no definitions written");
    // Without --write-defs or a definition file the banner still says where --defs goes.
    assert!(
        all.contains("--defs DIR") && all.contains("--write-defs DIR"),
        "{all}"
    );
    child.kill().unwrap();
    let _ = child.wait();
    let log = std::fs::read_to_string(dir.join("t.log")).unwrap();
    assert!(
        log.starts_with("earlier session\n"),
        "appended, not truncated: {log}"
    );
    // A killed simulator leaves its socket; the next one removes it.
    assert!(ctl.exists());
    let mut again = Command::new(bin)
        .arg("--control")
        .arg(&ctl)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut l = BufReader::new(again.stdout.take().unwrap()).lines();
    assert!(l
        .next()
        .unwrap()
        .unwrap()
        .starts_with("kline-sim: simulated K line on"));
    again.kill().unwrap();
    let _ = again.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
