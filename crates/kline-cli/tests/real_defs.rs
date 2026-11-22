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
