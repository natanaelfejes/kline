//! The real `kline` binary with no cable on this OS: `selftest` reports the host timing, then
//! fails with exit 3 (port), never hangs or panics. Runs on Linux, macOS and Windows.

use std::process::Command;

use serde_json::Value;

/// A port name that does not exist on this OS.
fn missing_port() -> &'static str {
    if cfg!(windows) {
        "COM250"
    } else {
        "/dev/kline-no-such-port"
    }
}

fn run(args: &[&str]) -> (i32, Vec<Value>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_kline"))
        .args(args)
        .env(
            "KLINE_CAPTURE_DIR",
            std::env::temp_dir().join("kline-no-cable"),
        )
        .output()
        .unwrap();
    let events = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    (
        out.status.code().unwrap_or(-1),
        events,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn event<'a>(ev: &'a [Value], name: &str) -> Option<&'a Value> {
    ev.iter().find(|e| e["event"] == name)
}

#[test]
fn selftest_without_a_cable_reports_timing_then_exits_3() {
    let (code, ev, err) = run(&["--json", "selftest", "--port", missing_port()]);
    assert_eq!(code, 3, "{ev:?}\n{err}");
    let t = event(&ev, "selftest_timing").expect("timing reported without a port");
    assert_eq!(t["os"], std::env::consts::OS);
    let js = t["sleep_jitter"].as_array().unwrap();
    assert_eq!(js.len(), 3, "{t}");
    // A correct sleep never wakes early, on any OS. (Lateness is reported, not asserted: CI
    // hosts are loaded.)
    assert!(js.iter().all(|j| j["early"] == 0), "{t}");
    eprintln!("host timing on {}: {t}", std::env::consts::OS);
    let e = event(&ev, "error").expect("error event");
    assert_eq!(e["code"], 3, "{e}");
    assert!(event(&ev, "selftest").is_none());
}

#[test]
fn auto_detect_without_a_cable_is_exit_3_with_a_hint() {
    // No --port: auto-detection over the ports this host really has. A developer machine with
    // an FTDI cable plugged in would detect it, so only the no-cable outcome is asserted.
    let (code, ev, err) = run(&["--json", "ports"]);
    assert_eq!(code, 0, "{ev:?}\n{err}");
    let p = event(&ev, "ports").unwrap();
    if !p["detected"].is_null() {
        eprintln!("SKIPPED: a KKL cable is plugged in ({p})");
        return;
    }
    let (code, ev, _) = run(&["--json", "selftest"]);
    assert_eq!(code, 3);
    assert!(
        event(&ev, "selftest_timing").is_some(),
        "timing is measured before port detection: {ev:?}"
    );
    let e = event(&ev, "error").unwrap();
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains("no KKL cable found"),
        "{e}"
    );
}
