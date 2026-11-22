//! Shared helpers: run the `kline` command logic in-process against kline-sim's VirtualLine
//! (virtual clock: a 5-baud init costs no real time), or the binary against a pty simulator.
#![allow(dead_code)]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use kline_cli::{CliEnv, Prompt};
use kline_host::cmd::Env;
use kline_host::out::Sink;
use kline_host::transport::{Clock, Transport};
use kline_sim::bus::Responder;
use kline_sim::ecu::{EcuProfile, SpecEcu};
use kline_sim::journal::Journal;
use kline_sim::line::{Line, VirtualLine};
use kline_sim::scenario::Scenario;
use kline_spec::{EcuDef, Value};
use serde_json::Value as Json;

pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn fixture(name: &str) -> EcuDef {
    EcuDef::from_xml_bytes(&std::fs::read(fixture_path(name)).unwrap()).unwrap()
}

pub fn sirius_scenario() -> Scenario {
    Scenario::new()
        .text("Calibration", "SIM-0001")
        .set("Supplier code", Value::Hex("0a1b".into()))
        .number("Engine speed", 850.0)
        .number("Vehicle speed", 0.0)
        .number("Injection time", 2400.0)
        .number("Battery voltage", 13.875)
        .text("Idle switch", "closed")
        .number("Coolant temperature", 88.0)
        .number("Air temperature", -5.5)
        .number("Throttle position", 4.5)
        .text("Injector 1 circuit", "fault")
        .text("Coolant sensor circuit", "ok")
        .text("Knock sensor circuit", "fault")
}

pub fn sirius_ecu() -> SpecEcu {
    SpecEcu::new(
        EcuProfile::sirius32(),
        fixture("sirius_cli.xml"),
        sirius_scenario(),
    )
    .unwrap()
}

pub fn bmt_scenario() -> Scenario {
    Scenario::new()
        .text("Part number", "77001X")
        .text("Board type", "type C")
        .text("Remote frame received", "yes")
        .number("Remote count", 2.0)
        .text("Door switch fault", "present")
        .number("Option A", 1.0)
}

pub fn bmt_ecu() -> SpecEcu {
    SpecEcu::new(EcuProfile::bmt(), fixture("bmt_cli.xml"), bmt_scenario()).unwrap()
}

pub fn abs_scenario() -> Scenario {
    Scenario::new()
        .text("Software", "A123")
        .number("Front left speed", 42.5)
        .number("Front right speed", 42.25)
        .text("Pump motor fault", "present")
}

pub fn abs_ecu() -> SpecEcu {
    let def = fixture("abs_fast.xml");
    let profile = EcuProfile::from_def(&def).unwrap();
    SpecEcu::new(profile, def, abs_scenario()).unwrap()
}

pub fn iso8_nofaults_ecu() -> SpecEcu {
    let def = fixture("iso8_nofaults.xml");
    let profile = EcuProfile::from_def(&def).unwrap();
    let sc = Scenario::new()
        .number("Engine speed", 800.0)
        .text("Fault present sensor A", "fault")
        .text("Fault present sensor B", "no");
    SpecEcu::new(profile, def, sc).unwrap()
}

/// kline-host Transport over kline-sim's in-process line.
pub struct Virtual(pub VirtualLine);

impl Transport for Virtual {
    fn now_ms(&self) -> u64 {
        self.0.now_ms()
    }
    fn set_dtr(&mut self, on: bool) -> io::Result<()> {
        self.0.set_dtr(on)
    }
    fn set_rts(&mut self, on: bool) -> io::Result<()> {
        self.0.set_rts(on)
    }
    fn set_break(&mut self, on: bool) -> io::Result<()> {
        self.0.set_break(on)
    }
    fn break_loopback_seen(&mut self) -> io::Result<bool> {
        self.0.break_loopback_seen()
    }
    fn sleep_ms(&mut self, ms: u32) {
        self.0.sleep_ms(ms)
    }
    fn flush_rx(&mut self) -> io::Result<()> {
        self.0.flush_rx()
    }
    fn write(&mut self, bytes: &[u8], inter_byte_ms: u32) -> io::Result<()> {
        self.0.write(bytes, inter_byte_ms)
    }
    fn read(&mut self, count: usize, timeout_ms: u32) -> io::Result<Vec<u8>> {
        self.0.read(count, timeout_ms)
    }
}

/// A [`Virtual`] line whose RX shows nothing for a break (macOS IGNBRK, Hydrogen final review).
pub struct BlindBreak(pub Virtual);

impl Transport for BlindBreak {
    fn now_ms(&self) -> u64 {
        self.0.now_ms()
    }
    fn set_dtr(&mut self, on: bool) -> io::Result<()> {
        self.0.set_dtr(on)
    }
    fn set_rts(&mut self, on: bool) -> io::Result<()> {
        self.0.set_rts(on)
    }
    fn set_break(&mut self, on: bool) -> io::Result<()> {
        self.0.set_break(on)
    }
    fn break_loopback_seen(&mut self) -> io::Result<bool> {
        Ok(false)
    }
    fn sleep_ms(&mut self, ms: u32) {
        self.0.sleep_ms(ms)
    }
    fn flush_rx(&mut self) -> io::Result<()> {
        self.0.flush_rx()
    }
    fn write(&mut self, bytes: &[u8], inter_byte_ms: u32) -> io::Result<()> {
        self.0.write(bytes, inter_byte_ms)
    }
    fn read(&mut self, count: usize, timeout_ms: u32) -> io::Result<Vec<u8>> {
        self.0.read(count, timeout_ms)
    }
}

/// Like [`run`] on a line that never reports the break loopback.
pub fn run_blind_break(ecu: SpecEcu, args: &[&str]) -> (Run, Journal) {
    let j = ecu.journal();
    (
        run_line_inner(
            VirtualLine::with_ecu(ecu),
            "virtual",
            args,
            Opts::default(),
            true,
        ),
        j,
    )
}

/// What the human at the fake terminal does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Human {
    /// Types the code shown on the screen.
    TypesShownCode,
    /// Types something else.
    TypesWrongCode,
}

pub struct FakeTty {
    pub human: Human,
    pub screen: Arc<Mutex<String>>,
}

impl Prompt for FakeTty {
    fn show(&mut self, text: &str) -> io::Result<()> {
        self.screen.lock().unwrap().push_str(text);
        Ok(())
    }
    fn ask(&mut self) -> mpsc::Receiver<io::Result<String>> {
        let (tx, rx) = mpsc::channel();
        let screen = self.screen.lock().unwrap().clone();
        let line = match self.human {
            Human::TypesShownCode => {
                // The latest challenge (an exec with --stop shows two).
                let after = screen.rsplit("Type the code ").next().unwrap();
                format!("{}\n", after.split_whitespace().next().unwrap())
            }
            Human::TypesWrongCode => "AAAAAA\n".to_string(),
        };
        tx.send(Ok(line)).unwrap();
        rx
    }
}

pub struct Run {
    pub code: u8,
    pub stdout: String,
    pub stderr: String,
    pub capture_dir: PathBuf,
    pub screen: String,
    pub opened: bool,
}

impl Run {
    /// stdout parsed as JSON lines.
    pub fn events(&self) -> Vec<Json> {
        self.stdout
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON: {l:?}: {e}")))
            .collect()
    }

    pub fn event(&self, name: &str) -> Vec<Json> {
        self.events()
            .into_iter()
            .filter(|e| e["event"] == name)
            .collect()
    }

    /// The capture files written.
    pub fn captures(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(&self.capture_dir)
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        v.retain(|p| p.extension().is_some_and(|x| x == "jsonl"));
        v.sort();
        v
    }
}

static N: AtomicU32 = AtomicU32::new(0);

pub fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "kline-cli-test-{}-{tag}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Options of one in-process run.
pub struct Opts {
    pub interactive: bool,
    pub human: Human,
    /// Fake Linux sysfs root for the FTDI latency timer (default: a path that does not exist).
    pub sys_root: Option<PathBuf>,
    /// Host clock sleeps wake up this late (us), for selftest's sleep-jitter check.
    pub clock_late_us: u64,
    /// Ctrl-C already pressed when the command starts.
    pub ctrl_c: bool,
    /// What the port listing returns (default: nothing).
    pub ports: Vec<kline_host::ports::PortInfo>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            interactive: false,
            human: Human::TypesShownCode,
            sys_root: None,
            clock_late_us: 0,
            ctrl_c: false,
            ports: Vec::new(),
        }
    }
}

/// Run `kline <args>` in-process with `ecu` on a virtual K line.
pub fn run_with(
    ecu: impl Responder + 'static,
    journal_tol: Option<kline_sim::timing::Tolerance>,
    args: &[&str],
    o: Opts,
) -> Run {
    let tol = journal_tol.unwrap_or_else(kline_sim::timing::Tolerance::lenient);
    run_line_with(
        VirtualLine::new(
            ecu,
            kline_sim::bus::BusConfig::default(),
            tol,
            Journal::new(),
        ),
        args,
        o,
    )
}

/// Run with a SpecEcu at its own (strict) tolerance; returns the run and the ECU journal.
pub fn run(ecu: SpecEcu, args: &[&str]) -> (Run, Journal) {
    run_opts(ecu, args, Opts::default())
}

pub fn run_opts(ecu: SpecEcu, args: &[&str], o: Opts) -> (Run, Journal) {
    let j = ecu.journal();
    (run_line_with(VirtualLine::with_ecu(ecu), args, o), j)
}

/// Like [`run`] with an explicit `--port`.
pub fn run_with_port(ecu: SpecEcu, port: &str, args: &[&str]) -> (Run, Journal) {
    let j = ecu.journal();
    (
        run_line_port(VirtualLine::with_ecu(ecu), port, args, Opts::default()),
        j,
    )
}

pub fn run_line_with(line: VirtualLine, args: &[&str], o: Opts) -> Run {
    run_line_port(line, "virtual", args, o)
}

pub fn run_line_port(line: VirtualLine, port: &str, args: &[&str], o: Opts) -> Run {
    run_line_inner(line, port, args, o, false)
}

fn run_line_inner(line: VirtualLine, port: &str, args: &[&str], o: Opts, blind: bool) -> Run {
    let capture_dir = temp_dir("cap");
    let (out, out_buf) = Sink::buffer();
    let (err, err_buf) = Sink::buffer();
    let line = Arc::new(Mutex::new(Some(line)));
    let opened = Arc::new(Mutex::new(false));
    let opened2 = opened.clone();
    let screen = Arc::new(Mutex::new(String::new()));
    let screen2 = screen.clone();
    let human = o.human;
    let clock_late_us = o.clock_late_us;
    let ctrl_c = o.ctrl_c;
    let ports = o.ports.clone();
    let sys_root = o
        .sys_root
        .clone()
        .unwrap_or_else(|| PathBuf::from("/nonexistent-kline-sysfs"));
    let mut env = CliEnv {
        host: Env {
            stdout: out,
            stderr: err,
            opener: Box::new(move |_spec, _baud, _trace| {
                *opened2.lock().unwrap() = true;
                let l = line.lock().unwrap().take().expect("port opened twice");
                if blind {
                    Ok(Box::new(BlindBreak(Virtual(l))) as Box<dyn Transport>)
                } else {
                    Ok(Box::new(Virtual(l)) as Box<dyn Transport>)
                }
            }),
            list_ports: Box::new(move || Ok(ports.clone())),
            stop: Arc::new(std::sync::atomic::AtomicBool::new(ctrl_c)),
            host_clock: Box::new(move || {
                Box::new(LateClock {
                    now: 0,
                    late_us: clock_late_us,
                }) as Box<dyn Clock>
            }),
            sys_root: sys_root.clone(),
        },
        interactive: o.interactive,
        tty: Box::new(move || {
            Ok(Box::new(FakeTty {
                human,
                screen: screen2.clone(),
            }) as Box<dyn Prompt>)
        }),
    };
    let mut full: Vec<String> = vec![
        "kline".into(),
        "--port".into(),
        port.into(),
        "--capture-dir".into(),
        capture_dir.display().to_string(),
    ];
    full.extend(args.iter().map(|s| s.to_string()));
    let code = kline_cli::run_args(full, &mut env);
    let s = |b: &Arc<Mutex<Vec<u8>>>| String::from_utf8(b.lock().unwrap().clone()).unwrap();
    let r = Run {
        code,
        stdout: s(&out_buf),
        stderr: s(&err_buf),
        capture_dir,
        screen: screen.lock().unwrap().clone(),
        opened: *opened.lock().unwrap(),
    };
    r
}

pub fn xml(name: &str) -> String {
    fixture_path(name).display().to_string()
}

/// `{"name": value}` of the decoded items of one decoded request (JSON).
pub fn items(decoded: &Json) -> std::collections::BTreeMap<String, Json> {
    decoded["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["name"].as_str().unwrap().to_string(), i["value"].clone()))
        .collect()
}

pub fn scenario_json(v: &Value) -> Json {
    match v {
        Value::Number(n) => serde_json::json!(n),
        Value::Text(t) | Value::Hex(t) => serde_json::json!(t),
    }
}

pub fn path_str(p: &Path) -> String {
    p.display().to_string()
}

/// Virtual host clock: every sleep wakes exactly `late_us` late, instantly.
pub struct LateClock {
    pub now: u64,
    pub late_us: u64,
}

impl Clock for LateClock {
    fn now_us(&self) -> u64 {
        self.now
    }
    fn sleep_until_us(&mut self, t: u64) {
        self.now = self.now.max(t) + self.late_us;
    }
}
