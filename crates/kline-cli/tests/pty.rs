//! The real `kline` binary against kline-sim on a pseudo-terminal (data over the pty, break /
//! DTR / loopback over the control socket), in real time.
#![cfg(unix)]

mod common;

use std::process::{Command, Stdio};

use common::*;
use kline_sim::bus::{Bus, BusConfig};
use kline_sim::ecu::{EcuProfile, SpecEcu};
use kline_sim::journal::Journal;
use kline_sim::pty::PtySim;
use kline_sim::timing::Tolerance;

fn spawn(tag: &str) -> Option<(PtySim, Journal)> {
    let mut p = EcuProfile::sirius32();
    p.tolerance = Tolerance::lenient();
    let ecu = SpecEcu::new(p, fixture("sirius_cli.xml"), sirius_scenario()).unwrap();
    let j = ecu.journal();
    let bus = Bus::new(ecu, BusConfig::default(), Tolerance::lenient(), j.clone());
    let ctl = std::env::temp_dir().join(format!("kline-cli-{tag}-{}.ctl", std::process::id()));
    let _ = std::fs::remove_file(&ctl);
    match PtySim::spawn(bus, &ctl) {
        Ok(s) => Some((s, j)),
        Err(e) => {
            eprintln!("SKIPPED: pty not available here ({e})");
            None
        }
    }
}

fn kline(sim: &PtySim, caps: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kline"))
        .arg("--port")
        .arg(sim.tty_path())
        .arg("--sim-control")
        .arg(sim.control_path())
        .arg("--capture-dir")
        .arg(caps)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn binary_runs_live_over_the_pty_with_trace() {
    let Some((sim, j)) = spawn("live") else {
        return;
    };
    let caps = temp_dir("pty");
    let x = xml("sirius_cli.xml");
    let o = kline(
        &sim,
        &caps,
        &[
            "--json", "--trace", "live", "--xml", &x, "--frames", "21A9", "--count", "2", "--rate",
            "4",
        ],
    );
    let out = String::from_utf8_lossy(&o.stdout);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{out}\n{err}");
    let ev: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let live: Vec<_> = ev.iter().filter(|e| e["event"] == "live").collect();
    assert_eq!(live.len(), 2);
    assert_eq!(
        items(&live[0]["decoded"][0])["Engine speed"],
        serde_json::json!(850.0)
    );
    assert!(
        err.contains("BREAK LOOPBACK seen") && err.contains("TX 82 7A F1 21 A9 B7"),
        "{err}"
    );
    j.assert_no_writes();
    let n = std::fs::read_dir(&caps).unwrap().count();
    assert_eq!(n, 3, "capture, trace and output file");
}

/// Step 7b FIX-1 over the real control socket: `--rts low` reaches the simulator once.
#[test]
fn binary_rts_low_goes_over_the_control_socket() {
    let Some((sim, j)) = spawn("rts") else {
        return;
    };
    let caps = temp_dir("pty-rts");
    let x = xml("sirius_cli.xml");
    let o = kline(
        &sim,
        &caps,
        &["--rts", "low", "--trace", "connect", "--xml", &x],
    );
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert!(err.contains("PORT dtr=1 rts=0"), "{err}");
    let rts: Vec<String> = j
        .notes()
        .into_iter()
        .map(|(_, n)| n)
        .filter(|n| n.starts_with("RTS"))
        .collect();
    assert_eq!(rts, vec!["RTS low".to_string()]);
}

#[test]
fn binary_exec_is_refused_without_a_tty() {
    let Some((sim, j)) = spawn("exec") else {
        return;
    };
    let caps = temp_dir("pty-exec");
    for args in [
        vec!["exec", "--xml", &*xml("sirius_cli.xml"), "11FF"],
        vec!["--json", "exec", "--xml", &*xml("sirius_cli.xml"), "11FF"],
    ] {
        let o = kline(&sim, &caps, &args);
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(o.status.code(), Some(7), "{err}");
        let out = String::from_utf8_lossy(&o.stdout);
        assert!(
            err.contains("interactive terminal") || out.contains("never runs with --json"),
            "{err}{out}"
        );
    }
    j.assert_no_writes();
    assert_eq!(
        std::fs::read_dir(&caps).unwrap().count(),
        0,
        "no session was opened"
    );
}
