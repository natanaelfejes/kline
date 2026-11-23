//! `kline` against the simulated Bosch 5.3 ABS (ISO8 at 0x01, key bytes DC 83), with the
//! hand-written `bosch_abs_iso8.xml` and the values of the car's replies of 2026-11-23.

mod common;

use common::*;

fn x() -> String {
    xml("bosch_abs_iso8.xml")
}

#[test]
fn connect_has_no_handshake_and_matches_the_key_bytes() {
    let (r, j) = run(bosch_abs_ecu(), &["--json", "connect", "--xml", &x()]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let c = &r.event("connected")[0];
    assert_eq!(c["protocol"], "iso8");
    assert_eq!(
        (c["kw1"].as_str(), c["kw2"].as_str()),
        (Some("DC"), Some("83"))
    );
    assert_eq!(c["address_complement"], serde_json::Value::Null);
    assert_eq!(c["keywords_match"], true);
    j.assert_no_writes();
    j.assert_no_violations();
}

#[test]
fn ident_decodes_the_identification() {
    let (r, j) = run(bosch_abs_ecu(), &["--json", "ident", "--xml", &x()]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let e = &r.event("ident")[0];
    assert_eq!(e["request"], "51");
    assert_eq!(
        e["reply"],
        "510502167701046407200249120265216703000000000000"
    );
    let it = items(&e["decoded"][0]);
    assert_eq!(it["Part number"], "7701046407");
    assert_eq!(it["Software"], "024912");
    assert_eq!(it["Supplier part number"], "0265216703");
    j.assert_no_writes();
}

#[test]
fn dtc_reports_one_stored_fault_with_its_slot() {
    let (r, j) = run(bosch_abs_ecu(), &["--json", "dtc", "--xml", &x()]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let d = &r.event("dtc")[0];
    assert_eq!(d["request"], "58");
    let active = d["active"].as_array().unwrap();
    assert_eq!(active.len(), 1, "{d}");
    assert_eq!(active[0]["name"], "Fault code 1");
    assert_eq!(active[0]["display"], "front left sensor signal");
    let ctx: Vec<(&str, &str)> = active[0]["context"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["name"].as_str().unwrap(), c["display"].as_str().unwrap()))
        .collect();
    assert_eq!(
        ctx,
        [
            ("Stop switch 1", "released"),
            ("Reference speed 1", "16"),
            ("Voltage 1", "13.56"),
            ("Counter 1", "1")
        ]
    );
    let s = &r.event("dtc_summary")[0];
    assert_eq!(s["active"], 1);
    assert_eq!(s["clear_plan"]["ours"]["payload"], "45");
    // Human output: the slot on one line.
    let (r, _) = run(bosch_abs_ecu(), &["dtc", "--xml", &x()]);
    assert!(
        r.stdout.contains(
            "Fault code 1 = front left sensor signal  (Stop switch 1 released, Reference speed 1 16 km/h, Voltage 1 13.56 V, Counter 1 1)"
        ),
        "{}",
        r.stdout
    );
    j.assert_no_writes();
}

#[test]
fn read_51_and_58_are_tier_0_and_the_rest_is_refused() {
    let (r, j) = run(
        bosch_abs_ecu(),
        &["--json", "read", "--xml", &x(), "58", "51"],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(r.event("read").len(), 2, "{}", r.stdout);
    j.assert_no_writes();
    // Clear, download and routine run are not reads: refused before the port is opened.
    for req in ["45", "41040000030000F0", "71", "21", "3350"] {
        let (r, j) = run(bosch_abs_ecu(), &["--json", "read", "--xml", &x(), req]);
        assert_eq!(r.code, 7, "{req}: {}", r.stdout);
        assert!(!r.opened, "{req}");
        assert!(j.writes().is_empty());
    }
}

#[test]
fn risk_of_the_non_reads() {
    let tier = |req: &str| {
        let (r, _) = run(bosch_abs_ecu(), &["--json", "risk", "--xml", &x(), req]);
        assert_eq!(r.code, 0, "{req}: {}\n{}", r.stdout, r.stderr);
        let e = &r.event("risk")[0]["risk"];
        (
            e["tier"].as_str().unwrap().to_string(),
            e["warnings"].to_string(),
            e["safety_system"].as_str().unwrap_or("").to_string(),
        )
    };
    let (t, w, s) = tier("45");
    assert!(t.starts_with("service"), "{t}");
    assert!(
        w.contains("ERASES_FAULT_MEMORY") && w.contains("SAFETY_SYSTEM"),
        "{w}"
    );
    assert!(s.contains("ABS / BRAKE ECU"), "{s}");
    // RAM download and routine run: permanently forbidden (owner decision 2026-11-23).
    for req in ["41040000030000F0", "71", "71FF22FF220000FF22FF22"] {
        let (r, j) = run(bosch_abs_ecu(), &["--json", "risk", "--xml", &x(), req]);
        assert_eq!(r.code, 7, "{req}: {}", r.stdout);
        let e = &r.event("risk")[0];
        assert_eq!(e["allowed"], false, "{e}");
        assert!(
            e["refused"]
                .as_str()
                .unwrap()
                .contains("permanently forbidden"),
            "{e}"
        );
        assert!(!r.opened);
        j.assert_no_writes();
    }
    // The bleed frames are named in the refusal.
    let (r, _) = run(
        bosch_abs_ecu(),
        &["--json", "risk", "--xml", &x(), "71FF30FF32000000000000"],
    );
    let refused = r.event("risk")[0]["refused"].as_str().unwrap().to_string();
    assert!(
        refused.contains("MES_PURGE_GARAGE_ARD_ON")
            && refused
                .contains("permanently forbidden: factory fill rig / brake bleed; use a garage"),
        "{refused}"
    );
    // The keep-alive is a protocol frame, never a request.
    let (r, _) = run(bosch_abs_ecu(), &["--json", "risk", "--xml", &x(), "36"]);
    assert_eq!(r.code, 7, "{}", r.stdout);
}

#[test]
fn clear_dtc_sends_45_only_after_approval() {
    let (r, j) = run_opts(
        bosch_abs_ecu(),
        &["clear-dtc", "--xml", &x()],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("will send: 45"), "{}", r.stdout);
    assert!(
        r.screen.contains("APPROVAL REQUIRED: service (tier 1)"),
        "{}",
        r.screen
    );
    assert!(r.screen.contains("ABS / BRAKE ECU"), "{}", r.screen);
    let w = j.writes();
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0].payload, [0x45]);
    // A wrong code sends nothing.
    let (r, j) = run_opts(
        bosch_abs_ecu(),
        &["clear-dtc", "--xml", &x()],
        Opts {
            interactive: true,
            human: Human::TypesWrongCode,
            ..Opts::default()
        },
    );
    assert_ne!(r.code, 0);
    assert!(j.writes().is_empty());
}

#[test]
fn real_x24_definition_ident_and_dtc() {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../ddt-xml/X24_ABS_Bosch5.3.xml");
    let Ok(bytes) = std::fs::read(&p) else {
        eprintln!(
            "SKIPPED: {} not available; put the DDT2000 XML files in ddt-xml/",
            p.display()
        );
        return;
    };
    let def = kline_spec::EcuDef::from_xml_bytes(&bytes).unwrap();
    let profile = kline_sim::ecu::EcuProfile::from_def(&def).unwrap();
    let sc = kline_sim::scenario::Scenario::new()
        .text("Défaut mémoire 1", "signal capteur AVG")
        .number("Vitesse de référence 1", 16.0)
        .number("compteur mémoire 1", 1.0);
    let ecu = || kline_sim::ecu::SpecEcu::new(profile.clone(), def.clone(), sc.clone()).unwrap();
    let path = p.display().to_string();
    let (r, j) = run(ecu(), &["--json", "dtc", "--xml", &path]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let d = &r.event("dtc")[0];
    assert_eq!(d["request"], "58");
    let active = d["active"].as_array().unwrap();
    assert_eq!(active.len(), 1, "{d}");
    assert_eq!(active[0]["display"], "signal capteur AVG");
    assert_eq!(active[0]["context"].as_array().unwrap().len(), 5, "{d}");
    assert_eq!(
        r.event("dtc_summary")[0]["clear_plan"]["ours"]["payload"],
        "45"
    );
    let (r, j2) = run(ecu(), &["--json", "ident", "--xml", &path]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(r.event("ident")[0]["request"], "51");
    j.assert_no_writes();
    j2.assert_no_writes();
}

/// Review of the ABS branch: `exec 41.. --stop 71..` must not get a download and a run out.
/// With the fixture (key bytes DC 83 declared) it is refused before the port is opened.
#[test]
fn exec_download_with_stop_run_is_refused_before_init() {
    let (r, j) = run_opts(
        bosch_abs_ecu(),
        &[
            "--trace",
            "exec",
            "--xml",
            &x(),
            "41040000030000F0",
            "--stop",
            "71",
        ],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 7, "{}\n{}\n{}", r.stdout, r.stderr, r.screen);
    assert!(r.stderr.contains("permanently forbidden"), "{}", r.stderr);
    assert!(!r.opened);
    assert!(!r.stderr.contains("] TX "), "{}", r.stderr);
    assert!(j.writes().is_empty());
    // An approvable request with a non-read --stop: refused before the port is opened too.
    let (r, j) = run_opts(
        bosch_abs_ecu(),
        &["exec", "--xml", &x(), "45", "--stop", "45"],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 7, "{}\n{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("--stop is not available"), "{}", r.stderr);
    assert!(!r.opened);
    assert!(j.writes().is_empty());
}

/// When the definition does not declare the key bytes, the preview cannot know the unit is the
/// Bosch 5.3 ABS; kline-core decides on the key bytes the ECU answered (DC 83) and refuses the
/// download after the init: only the init and the 3F stop go out.
#[test]
fn exec_download_is_refused_by_the_session_on_the_answered_key_bytes() {
    let src = std::fs::read_to_string(fixture_path("bosch_abs_iso8.xml")).unwrap();
    let nokw = src.replace(r#"<KW1 Value="220"/><KW2 Value="131" Timeout="1500"/>"#, "");
    assert_ne!(nokw, src);
    let dir = temp_dir("abs-nokw");
    let path = dir.join("abs_nokw.xml");
    std::fs::write(&path, nokw).unwrap();
    let path = path.display().to_string();
    let (r, j) = run_opts(
        bosch_abs_ecu(),
        &["--trace", "exec", "--xml", &path, "41040000030000F0"],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 7, "{}\n{}\n{}", r.stdout, r.stderr, r.screen);
    assert!(r.opened);
    assert!(r.stderr.contains("permanently forbidden"), "{}", r.stderr);
    // ISO8 frames on the wire: only the stop `02 3F 41` after the 5-baud init.
    let tx: Vec<&str> = r
        .stderr
        .lines()
        .filter_map(|l| l.split_once("] TX ").map(|(_, f)| f))
        .collect();
    assert_eq!(tx, ["02 3F 41 (inter-byte 5 ms)"], "{}", r.stderr);
    assert!(j.writes().is_empty());
    assert!(!r.screen.contains("APPROVAL REQUIRED"), "{}", r.screen);
}

/// A lost reply to the ABS fault memory erase `45` points at the fault memory read `58`.
#[test]
fn a_lost_45_reply_suggests_reading_58() {
    use kline_sim::ecu::{FaultRule, ReplyFault};
    let ecu = bosch_abs_ecu().with_fault(FaultRule::on(&[0x45], ReplyFault::NoReply));
    let (r, j) = run_opts(
        ecu,
        &["exec", "--xml", &x(), "45"],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 13, "{}\n{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("WAS SENT"), "{}", r.stderr);
    assert!(
        r.stderr
            .contains("first read the state (e.g. kline read --xml")
            && r.stderr.contains("bosch_abs_iso8.xml 58)"),
        "{}",
        r.stderr
    );
    assert_eq!(j.writes().len(), 1);
}
