//! `clio risk --ecu sirius|bmt` against the real DDT2000 files (gitignored `ddt-xml/`, rule 1).
//! Skipped, not failed, when they are absent.

mod common;

use common::*;

fn defs_dir() -> Option<String> {
    let d = std::env::var_os("CLIO_DDT_XML")
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
