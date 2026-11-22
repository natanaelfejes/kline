//! `kline risk --ecu sirius|bmt` against the real DDT2000 files (gitignored `ddt-xml/`, rule 1).
//! Skipped, not failed, when they are absent.

mod common;

use common::*;

fn defs_dir() -> Option<String> {
    let d = std::env::var_os("KLINE_DDT_XML")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ddt-xml")
        });
    if d.join("BMT_X65_ph1_p.xml").exists()
        && d.join("SIRIUS32___EMS3132___Soft_RE1_maj.xml").exists()
    {
        Some(d.display().to_string())
    } else {
        eprintln!(
            "SKIPPED: {} has no DDT2000 XML files; put them in ddt-xml/",
            d.display()
        );
        None
    }
}

fn risk(defs: &str, ecu: &str, req: &str) -> Run {
    run(
        sirius_ecu(),
        &["--json", "--defs", defs, "risk", "--ecu", ecu, req],
    )
    .0
}

#[test]
fn owner_approved_bmt_remote_learning_passes_the_name_guard() {
    let Some(d) = defs_dir() else { return };
    for req in ["31C7000000", "31C8000000"] {
        let r = risk(&d, "bmt", req);
        assert_eq!(r.code, 0, "{req}: {}", r.stdout);
        let e = &r.event("risk")[0];
        assert_eq!(e["risk"]["tier_number"], 1);
        // The real names contain "CLE" (remote key): only the exact payload on 0x26 is exempt.
        assert!(e["names"][0].as_str().unwrap().contains("CLE"), "{e}");
    }
}

#[test]
fn ordinary_sirius_service_requests_are_not_flagged_by_name() {
    let Some(d) = defs_dir() else { return };
    for (req, tier) in [("140000", 1), ("11FF", 1), ("32A000", 1), ("3BD010", 2)] {
        let r = risk(&d, "sirius", req);
        assert_eq!(r.code, 0, "{req}: {}", r.stdout);
        assert_eq!(r.event("risk")[0]["risk"]["tier_number"], tier, "{req}");
    }
    // Reading the immobilizer frames stays Tier 0; any write to them is refused (rule 3).
    assert_eq!(
        risk(&d, "sirius", "21A0").event("risk")[0]["risk"]["tier_number"],
        0
    );
    assert_eq!(risk(&d, "sirius", "3BA000").code, 7);
    assert_eq!(risk(&d, "bmt", "21A401").code, 7);
}

#[test]
fn safir_dtc_reads_its_fault_named_frames() {
    let Some(d) = defs_dir() else { return };
    let path = std::path::Path::new(&d).join("Safir_1.xml");
    if !path.exists() {
        eprintln!("SKIPPED: no Safir_1.xml");
        return;
    }
    let def = kline_spec::EcuDef::from_xml_bytes(&std::fs::read(&path).unwrap()).unwrap();
    let profile = kline_sim::ecu::EcuProfile::from_def(&def).unwrap();
    let ecu =
        kline_sim::ecu::SpecEcu::new(profile, def, kline_sim::scenario::Scenario::new()).unwrap();
    let p = path.display().to_string();
    let (r, j) = run(ecu, &["--json", "dtc", "--xml", &p]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let reqs: Vec<String> = r
        .event("dtc")
        .iter()
        .map(|e| e["request"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(reqs, ["3350", "3352", "3351"], "{}", r.stdout);
    j.assert_no_writes();
}

#[test]
fn the_two_real_bmt_definitions_differ_where_the_docs_say() {
    let Some(d) = defs_dir() else { return };
    let dir = std::path::Path::new(&d);
    if !dir.join("BMT_SAGEM.xml").exists() {
        eprintln!("SKIPPED: no BMT_SAGEM.xml");
        return;
    }
    let load = |f: &str| kline_host::defs::LoadedDef {
        path: dir.join(f),
        def: kline_spec::EcuDef::from_xml_bytes(&std::fs::read(dir.join(f)).unwrap()).unwrap(),
    };
    let (a, b) = (load("BMT_X65_ph1_p.xml"), load("BMT_SAGEM.xml"));
    let h: Vec<String> = kline_host::defs::candidate_differences(&[&a, &b])
        .iter()
        .map(|d| d.human())
        .collect();
    for want in [
        "21 A1 byte 3 mask 0x80: CARCLIENT (BMT_X65_ph1_p) vs CONDROUL (BMT_SAGEM)",
        "21 A1 byte 4 mask 0x01: CONDROUL (BMT_X65_ph1_p) vs - (BMT_SAGEM)",
        "21 A1 byte 4 mask 0x02: ALARMELEDCPE (BMT_X65_ph1_p) vs - (BMT_SAGEM)",
        "3B D2 00 00 sent \"Choix fonction\" = 7: CARCLIENT (BMT_X65_ph1_p) vs CONDROUL (BMT_SAGEM)",
    ] {
        assert!(h.iter().any(|l| l == want), "missing {want:?} in {h:#?}");
    }
    // 21 80 has the same layout in both: it cannot decide (docs/cli.md).
    assert!(!h.iter().any(|l| l.starts_with("21 80")), "{h:#?}");
    // TRF sits at the same place in both.
    assert!(!h.iter().any(|l| l.contains("TRF")), "{h:#?}");
}

/// The real ph1_p BMT definition in the simulator, answering like `scenario`.
fn real_bmt(
    defs: &std::path::Path,
    file: &str,
    sc: kline_sim::scenario::Scenario,
) -> kline_sim::ecu::SpecEcu {
    let def = kline_spec::EcuDef::from_xml_bytes(&std::fs::read(defs.join(file)).unwrap()).unwrap();
    kline_sim::ecu::SpecEcu::new(kline_sim::ecu::EcuProfile::bmt(), def, sc).unwrap()
}

#[test]
fn real_bmt_reads_show_flag_meanings_and_the_open_definition_choice() {
    let Some(d) = defs_dir() else { return };
    let dir = std::path::PathBuf::from(&d);
    if !dir.join("BMT_SAGEM.xml").exists() {
        eprintln!("SKIPPED: no BMT_SAGEM.xml");
        return;
    }
    let sc = kline_sim::scenario::Scenario::new()
        .number("TRF", 1.0)
        .number("CARCLIENT", 1.0)
        .number("TIR RF RECU", 1.0);
    let (r, j) = run(
        real_bmt(&dir, "BMT_X65_ph1_p.xml", sc.clone()),
        &["--defs", &d, "read", "--ecu", "bmt", "21A1"],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    for needle in [
        "definition choice unresolved: BMT_X65_ph1_p, BMT_SAGEM; values below decoded with BMT_X65_ph1_p",
        "(1 = RF remote (inferred, NOT VERIFIED))",
        "decoded differently by the candidates:",
        "BMT_SAGEM: CONDROUL 1",
    ] {
        assert!(r.stdout.contains(needle), "missing {needle:?} in\n{}", r.stdout);
    }
    j.assert_no_writes();
    let (r, _) = run(
        real_bmt(&dir, "BMT_X65_ph1_p.xml", sc.clone()),
        &["--defs", &d, "read", "--ecu", "bmt", "21A3"],
    );
    assert!(
        r.stdout.contains("(1 = a remote frame was received)"),
        "{}",
        r.stdout
    );
    let (r, _) = run(
        real_bmt(&dir, "BMT_X65_ph1_p.xml", sc),
        &["--defs", &d, "ident", "--ecu", "bmt"],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert!(
        r.stdout
            .contains("21 A1 byte 3 mask 0x80: CARCLIENT (BMT_X65_ph1_p) vs CONDROUL (BMT_SAGEM)"),
        "{}",
        r.stdout
    );
    if std::env::var_os("KLINE_SHOW").is_some() {
        eprintln!("{}", r.stdout);
    }
}

/// The car command of the remote learning (docs/cli.md) against the real BMT definition: one
/// 31 C7, then only 21 A3 every 500 ms, the remote press shown as TIR RF RECU/VAL/OK 0 -> 1
/// with their meanings, then Stop (final critique F01).
#[test]
fn real_bmt_remote_learning_hold_and_watch() {
    let Some(d) = defs_dir() else { return };
    let dir = std::path::PathBuf::from(&d);
    let sc = kline_sim::scenario::Scenario::new()
        .number("TIR RF RECU", 0.0)
        .number("TIR RF VAL", 0.0)
        .number("TIR RF OK", 0.0);
    let c7 = [0x31, 0xC7, 0x00, 0x00, 0x00];
    let mut ecu = real_bmt(&dir, "BMT_X65_ph1_p.xml", sc);
    for item in ["TIR RF RECU", "TIR RF VAL", "TIR RF OK"] {
        ecu = ecu.with_change_after(&c7, 3000, item, kline_spec::Value::Number(1.0));
    }
    let j = ecu.journal();
    let r = run_line_with(
        kline_sim::line::VirtualLine::with_ecu(ecu),
        &[
            "--trace",
            "--defs",
            &d,
            "exec",
            "--ecu",
            "bmt",
            "31C7000000",
            "--hold",
            "30",
            "--watch",
            "21A3",
        ],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    for needle in [
        "holding the session for 30 s",
        "TIR RF RECU 0 -> 1  (1 = a remote frame was received)",
        "TIR RF VAL 0 -> 1  (1 = the remote frame is valid, well-formed)",
        "TIR RF OK 0 -> 1  (1 = remote accepted, a learned remote)",
        "hold ended after 30 s",
    ] {
        assert!(
            r.stdout.contains(needle),
            "missing {needle:?} in\n{}",
            r.stdout
        );
    }
    let tx = tx_payloads(&r.stderr);
    let i = tx.iter().position(|p| p == &c7).unwrap();
    let rest = &tx[i + 1..];
    assert_eq!(rest.last(), Some(&vec![0x82]));
    assert!(rest[..rest.len() - 1]
        .iter()
        .all(|p| p == &vec![0x21, 0xA3]));
    let w = j.writes();
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0].payload, c7.to_vec());
    if std::env::var_os("KLINE_SHOW").is_some() {
        eprintln!("{}", r.stdout);
    }
}
