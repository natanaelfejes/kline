//! The `kline-sim` built-in definitions only look like the car's: kline refuses them on any
//! port but the simulator (exit 2, before the port is opened) and `risk` warns about them.

mod common;

use common::*;
use kline_sim::builtin::{write_defs, Builtin};
use kline_sim::ecu::{EcuProfile, SpecEcu};

fn builtin_bmt() -> SpecEcu {
    let b = Builtin::by_name("bmt").unwrap();
    let def = b.def();
    SpecEcu::new(EcuProfile::from_def(&def).unwrap(), def, b.scenario()).unwrap()
}

#[test]
fn a_builtin_definition_is_refused_on_a_real_port() {
    let defs = temp_dir("sim-builtin");
    write_defs(defs.path()).unwrap();
    let x = defs.join("BMT_X65_ph1_p.xml").display().to_string();
    let d = defs.display().to_string();
    for args in [
        vec!["read", "--xml", &x, "21A3"],
        vec!["--defs", &d, "read", "--ecu", "bmt", "21A3"],
        vec!["--defs", &d, "dtc", "--ecu", "bmt"],
        vec![
            "--json", "--defs", &d, "live", "--ecu", "bmt", "--frames", "21A3", "--rate", "1",
        ],
    ] {
        let (r, j) = run(builtin_bmt(), &args);
        assert_eq!(r.code, 2, "{args:?}\n{}\n{}", r.stdout, r.stderr);
        assert!(!r.opened, "the port was opened: {args:?}");
        let msg = format!("{}{}", r.stdout, r.stderr);
        assert!(
            msg.contains("kline-sim built-in definition (not for a car)")
                && msg.contains("--sim-control"),
            "{msg}"
        );
        j.assert_no_writes();
    }
    // exec too (kline only), with a human at the terminal: refused before any approval.
    let (r, j) = run_opts(
        builtin_bmt(),
        &["--defs", &d, "exec", "--ecu", "bmt", "31A00001"],
        Opts {
            interactive: true,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 2, "{}\n{}", r.stdout, r.stderr);
    assert!(!r.opened && r.screen.is_empty(), "{}", r.screen);
    j.assert_no_writes();
}

#[test]
fn a_car_definition_is_not_refused() {
    let (r, _) = run(bmt_ecu(), &["read", "--xml", &xml("bmt_cli.xml"), "21A3"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
}

#[test]
fn risk_warns_about_a_builtin_definition() {
    let defs = temp_dir("sim-builtin-risk");
    write_defs(defs.path()).unwrap();
    let d = defs.display().to_string();
    let (r, _) = run(
        builtin_bmt(),
        &["--defs", &d, "risk", "--ecu", "bmt", "31A00001"],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let all = format!("{}{}", r.stdout, r.stderr);
    assert!(
        all.contains("warning:") && all.contains("kline-sim built-in definition (not for a car)"),
        "{all}"
    );
    let (r, _) = run(
        builtin_bmt(),
        &["--json", "--defs", &d, "risk", "--ecu", "bmt", "31A00001"],
    );
    assert!(
        r.event("warning")
            .iter()
            .any(|e| e["kind"] == "sim_builtin_definition"),
        "{}",
        r.stdout
    );
    // A car definition: no such warning.
    let (r, _) = run(
        bmt_ecu(),
        &["risk", "--xml", &xml("bmt_cli.xml"), "31A00001"],
    );
    assert!(!format!("{}{}", r.stdout, r.stderr).contains("kline-sim built-in"));
}
