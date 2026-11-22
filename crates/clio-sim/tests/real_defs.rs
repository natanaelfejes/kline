//! Against the real (gitignored) definitions: every request the core allows gets a reply that
//! `decode_checked` accepts for every definition with those bytes. Skipped without ddt-xml/.

mod common;

use clio_core::safety::{self, Protocol, Target};
use clio_sim::ecu::{EcuProfile, SpecEcu};
use clio_sim::scenario::Scenario;
use clio_spec::EcuDef;
use common::*;

fn check_file(name: &str) {
    let Some(bytes) = ddt_file(name) else { return };
    let def = EcuDef::from_xml_bytes(&bytes).unwrap();
    let profile = EcuProfile::from_def(&def).unwrap();
    let ecu = SpecEcu::new(profile, def.clone(), Scenario::new()).unwrap();
    let (mut d, journal) = drive(ecu);
    let cfg = cfg(&def);
    let init = d.init(&cfg).unwrap();
    assert_eq!(Some(init.keywords()), cfg.expected_keywords);
    let mut s = d.session(init);
    let target = s.target();
    assert_eq!(
        target,
        Target {
            protocol: Protocol::Kwp,
            ecu_address: cfg.ecu_address,
        }
    );
    let mut sent = 0;
    for req in &def.requests {
        let p = &req.sent_bytes.0;
        if safety::check(&target, p).is_err() {
            continue;
        }
        let reply = positive(&d.request(&mut s, p).unwrap());
        req.decode_checked(&def, &reply)
            .unwrap_or_else(|e| panic!("{name} {:?}: {e}", req.name));
        sent += 1;
    }
    d.stop(&mut s).unwrap();
    assert!(sent > 5, "{name}: only {sent} requests allowed");
    eprintln!("{name}: {sent} allowed requests answered and decoded");
    journal.assert_no_writes();
    journal.assert_no_violations();
}

#[test]
fn sirius32_definition() {
    check_file("SIRIUS32___EMS3132___Soft_RE1_maj.xml");
}

#[test]
fn bmt_x65_definition() {
    check_file("BMT_X65_ph1_p.xml");
}
