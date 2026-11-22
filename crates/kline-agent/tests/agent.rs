//! kline-agent: built without approvals, and unable to send any non-read (checked on the
//! simulator's write log over a real pty session).

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../kline-cli/tests/fixtures")
        .join(name)
}

/// `cargo tree -p kline-agent -e features`: kline-core must resolve without `approved-routines`.
#[test]
fn agent_is_built_without_approved_routines() {
    let ws = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args([
            "tree",
            "--offline",
            "-p",
            "kline-agent",
            "-e",
            "features",
            "--manifest-path",
        ])
        .arg(&ws)
        .output()
        .expect("run cargo tree");
    let tree = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        tree.contains("kline-core"),
        "tree lists kline-core:\n{tree}"
    );
    assert!(
        !tree.contains("approved-routines"),
        "kline-agent resolves kline-core with approved-routines:\n{tree}"
    );
    assert!(
        !tree.contains("kline-cli"),
        "kline-agent must not depend on kline-cli:\n{tree}"
    );
}

/// The agent binary under test was built with approvals unified in (joint workspace build):
/// it must refuse to start, so the session tests below are skipped. `cargo test -p kline-agent`
/// alone runs them.
#[cfg_attr(not(unix), allow(dead_code))]
fn unified() -> bool {
    if kline_core::safety::APPROVALS_COMPILED {
        eprintln!("SKIPPED: kline-agent built with approved-routines unified in; run `cargo test -p kline-agent` alone");
        true
    } else {
        false
    }
}

/// Finding 4: with `approved-routines` unified in, the agent refuses to start (exit 7) and
/// sends nothing; built alone it runs.
#[test]
fn agent_refuses_to_start_when_approvals_are_compiled_in() {
    let o = Command::new(env!("CARGO_BIN_EXE_kline-agent"))
        .args(["risk", "--xml"])
        .arg(fixture("sirius_cli.xml"))
        .arg("21A9")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    if kline_core::safety::APPROVALS_COMPILED {
        assert_eq!(o.status.code(), Some(7), "{err}");
        assert!(err.contains("refusing to start"), "{err}");
        assert!(
            o.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&o.stdout)
        );
    } else {
        assert_eq!(o.status.code(), Some(0), "{err}");
        assert!(!err.contains("refusing to start"), "{err}");
    }
}

/// The pty simulator sessions: Unix only (kline-sim `pty`); on Windows these are skipped and the
/// in-process simulator e2e (kline-cli tests/e2e.rs) covers the shared code.
#[cfg(unix)]
mod pty_sessions {
    use super::*;
    use kline_sim::bus::{Bus, BusConfig};
    use kline_sim::ecu::{EcuProfile, SpecEcu};
    use kline_sim::journal::Journal;
    use kline_sim::pty::PtySim;
    use kline_sim::scenario::Scenario;
    use kline_sim::timing::Tolerance;
    use kline_spec::EcuDef;

    fn sim(tag: &str) -> Option<(PtySim, Journal)> {
        let def =
            EcuDef::from_xml_bytes(&std::fs::read(fixture("sirius_cli.xml")).unwrap()).unwrap();
        let mut p = EcuProfile::sirius32();
        p.tolerance = Tolerance::lenient();
        let sc = Scenario::new().number("Engine speed", 850.0);
        let ecu = SpecEcu::new(p, def, sc).unwrap();
        let j = ecu.journal();
        let bus = Bus::new(ecu, BusConfig::default(), Tolerance::lenient(), j.clone());
        let ctl =
            std::env::temp_dir().join(format!("kline-agent-{tag}-{}.ctl", std::process::id()));
        let _ = std::fs::remove_file(&ctl);
        match PtySim::spawn(bus, &ctl) {
            Ok(s) => Some((s, j)),
            Err(e) => {
                eprintln!("SKIPPED: pty not available here ({e})");
                None
            }
        }
    }

    fn agent(sim: &PtySim, args: &[&str]) -> (Option<i32>, Vec<serde_json::Value>, String) {
        let caps = std::env::temp_dir().join(format!("kline-agent-caps-{}", std::process::id()));
        let o = Command::new(env!("CARGO_BIN_EXE_kline-agent"))
            .arg("--port")
            .arg(sim.tty_path())
            .arg("--sim-control")
            .arg(sim.control_path())
            .arg("--capture-dir")
            .arg(&caps)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        let ev = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON {l:?}: {e}")))
            .collect();
        (
            o.status.code(),
            ev,
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    }

    #[test]
    fn agent_reads_but_cannot_send_any_non_read() {
        if unified() {
            return;
        }
        let Some((sim, j)) = sim("nonread") else {
            return;
        };
        let x = fixture("sirius_cli.xml").display().to_string();
        // A read works end to end (JSON without --json: the agent always emits JSON lines).
        let (code, ev, err) = agent(&sim, &["read", "--xml", &x, "21A9"]);
        assert_eq!(code, Some(0), "{ev:?}\n{err}");
        assert!(ev.iter().any(|e| e["event"] == "read"));
        for req in [
            "11FF",
            "Reset learning",
            "140000",
            "3BD01005",
            "31A000",
            "31A400",
            "2701",
            "1085",
            "82",
            "3E",
        ] {
            let (code, ev, _) = agent(&sim, &["read", "--xml", &x, req]);
            let ok_tp = req == "3E";
            if ok_tp {
                // 3E is a Tier 0 read; the ECU's TesterPresent reply is fine.
                assert_eq!(code, Some(0), "{req}: {ev:?}");
            } else {
                assert_eq!(code, Some(7), "{req}: {ev:?}");
            }
        }
        // There is no exec in this binary.
        let (code, _, _) = agent(&sim, &["exec", "--xml", &x, "11FF"]);
        assert_eq!(code, Some(2));
        j.assert_no_writes();
    }

    #[test]
    fn prepare_prints_risk_and_the_exact_human_command() {
        if unified() {
            return;
        }
        let Some((sim, j)) = sim("prepare") else {
            return;
        };
        let x = fixture("sirius_cli.xml").display().to_string();
        let (code, ev, _) = agent(&sim, &["prepare", "--xml", &x, "Reset learning"]);
        assert_eq!(code, Some(0), "{ev:?}");
        let p = &ev[0];
        assert_eq!(p["event"], "prepare");
        assert_eq!(p["risk"]["tier_number"], 1);
        assert_eq!(
            p["risk"]["warnings"],
            serde_json::json!(["ERASES_LEARNED_VALUES"])
        );
        let cmd = p["command"].as_str().unwrap();
        assert!(cmd.starts_with("kline --port "), "{cmd}");
        assert!(cmd.ends_with(&format!(" exec --xml {x} 11FF")), "{cmd}");
        // Forbidden: no command at all.
        let (code, ev, _) = agent(&sim, &["prepare", "--xml", &x, "31A0A3"]);
        assert_eq!(code, Some(7));
        assert_eq!(ev[0]["command"], serde_json::Value::Null);
        j.assert_no_writes();
    }
}

#[cfg(not(unix))]
#[test]
fn pty_sessions_are_skipped_on_this_os() {
    eprintln!("SKIPPED: kline-agent pty sessions need a Unix pty; see kline-cli tests/e2e.rs");
}
