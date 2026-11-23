//! Bosch 5.3 ABS (ISO8 at 0x01, key bytes DC 83): the spec-driven profile and a replay of the
//! car captures of 2026-11-23 (`kline exec 51` / `kline exec 58` with X24_ABS_Bosch5.3.xml).

mod common;

use common::*;
use kline_core::frame::Reply;
use kline_core::session::SessionOutcome;
use kline_sim::bus::BusConfig;
use kline_sim::driver::Driver;
use kline_sim::ecu::{EcuProfile, SpecEcu};
use kline_sim::line::VirtualLine;
use kline_sim::replay::{read_jsonl, ReplayEcu};
use kline_sim::scenario::Scenario;
use kline_sim::timing::Tolerance;
use kline_spec::Value;

fn abs() -> kline_spec::EcuDef {
    fixture("bosch_abs_iso8.xml")
}

/// The values of the car's 51 / 58 replies (2026-11-23).
fn car_scenario() -> Scenario {
    Scenario::new()
        .text("ECU type", "ABS")
        .text("Supplier", "Bosch")
        .set("After-sales id", Value::Hex("16".into()))
        .set("Part number", Value::Hex("7701046407".into()))
        .set("Part index", Value::Text(" ".into()))
        .set("Software", Value::Hex("024912".into()))
        .set("Supplier part number", Value::Hex("0265216703".into()))
        .text("Fault code 1", "front left sensor signal")
        .number("Reference speed 1", 16.0)
        .number("Voltage 1", 13.561)
        .number("Counter 1", 1.0)
}

const CAR_51: [u8; 24] = [
    0x51, 0x05, 0x02, 0x16, 0x77, 0x01, 0x04, 0x64, 0x07, 0x20, 0x02, 0x49, 0x12, 0x02, 0x65, 0x21,
    0x67, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const CAR_58: [u8; 16] = [
    0x58, 0x08, 0x00, 0x10, 0xBF, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

#[test]
fn profile_from_the_definition_is_the_bosch_abs() {
    let p = EcuProfile::from_def(&abs()).unwrap();
    assert!(p.is_bosch_abs());
    assert_eq!(p.positive_sid_offset, 0);
    assert_eq!(p.iso8_tester_present_reply.as_deref(), Some(&[0x36][..]));
    assert_eq!(p.iso8_stop_reply.as_deref(), Some(&[0x3F][..]));
    assert_eq!(EcuProfile::bosch_abs53().keywords, (0xDC, 0x83));
    assert!(!EcuProfile::safir_iso8().is_bosch_abs());
}

#[test]
fn spec_ecu_answers_like_the_car() {
    let def = abs();
    let ecu = SpecEcu::new(EcuProfile::bosch_abs53(), def.clone(), car_scenario()).unwrap();
    let (mut d, journal) = drive(ecu);
    let init = d.init(&cfg(&def)).unwrap();
    assert_eq!(init.keywords(), (0xDC, 0x83));
    assert_eq!(init.address_complement(), None);
    let mut s = d.session(init);
    assert_eq!(
        d.tester_present(&mut s).unwrap(),
        SessionOutcome::TesterPresent(Some(Reply::Positive(vec![0x36])))
    );
    assert_eq!(positive(&d.request(&mut s, &[0x51]).unwrap()), CAR_51);
    assert_eq!(positive(&d.request(&mut s, &[0x58]).unwrap()), CAR_58);
    assert_eq!(
        d.stop(&mut s).unwrap(),
        SessionOutcome::Stopped(Some(Reply::Positive(vec![0x3F])))
    );
    journal.assert_no_writes();
    journal.assert_no_violations();
}

/// Without an approval the core refuses 45 and nothing reaches the ECU; with one, the clear goes
/// out once, the ECU answers `45` (assumption, NOT VERIFIED: no car capture of 45 yet) and the
/// write log records it.
#[cfg(feature = "approved-routines")]
#[test]
fn approved_clear_reaches_the_ecu_and_is_logged() {
    use kline_core::safety::{Approval, Tier};
    use kline_sim::journal::WriteKind;
    let def = abs();
    let ecu = SpecEcu::new(EcuProfile::bosch_abs53(), def.clone(), car_scenario()).unwrap();
    let (mut d, journal) = drive(ecu);
    let init = d.init(&cfg(&def)).unwrap();
    let mut s = d.session(init);
    assert!(d.request(&mut s, &[0x45]).is_err());
    assert!(journal.writes().is_empty());
    let c = s.service_challenge(&[0x45]).unwrap();
    assert_eq!(c.tier(), Tier::Service);
    let code = c.code().to_string();
    let a = Approval::confirm(c, &code).unwrap();
    assert_eq!(positive(&d.request_approved(&mut s, a).unwrap()), [0x45]);
    let w = journal.writes();
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0].kind, WriteKind::ClearDtc);
    assert_eq!(w[0].request_names, ["Fault memory RAZ"]);
}

fn replay(name: &str) -> (Driver<VirtualLine>, kline_sim::journal::Journal) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let log = read_jsonl(std::fs::read(path).unwrap().as_slice()).unwrap();
    let ecu = ReplayEcu::new(log);
    let j = ecu.journal();
    let line = VirtualLine::new(ecu, BusConfig::default(), Tolerance::lenient(), j.clone());
    (Driver::new(line), j)
}

#[test]
fn car_capture_51_replays_and_decodes() {
    let def = abs();
    let (mut d, j) = replay("abs_car_51.jsonl");
    let init = d.init(&cfg(&def)).unwrap();
    assert_eq!(init.keywords(), (0xDC, 0x83));
    let mut s = d.session(init);
    // The car session: two keep-alives while the owner typed the code, 51, Stop.
    for _ in 0..2 {
        assert_eq!(
            d.tester_present(&mut s).unwrap(),
            SessionOutcome::TesterPresent(Some(Reply::Positive(vec![0x36])))
        );
    }
    let r = positive(&d.request(&mut s, &[0x51]).unwrap());
    assert_eq!(r, CAR_51);
    let items = def
        .request("Identification")
        .unwrap()
        .decode_checked(&def, &r)
        .unwrap();
    let part = items.iter().find(|i| i.name == "Part number").unwrap();
    assert_eq!(part.display.as_deref(), Some("7701046407"));
    assert_eq!(
        d.stop(&mut s).unwrap(),
        SessionOutcome::Stopped(Some(Reply::Positive(vec![0x3F])))
    );
    assert!(j.violations().is_empty(), "{:?}", j.violations());
}

#[test]
fn car_capture_58_replays_and_decodes() {
    let def = abs();
    let (mut d, j) = replay("abs_car_58.jsonl");
    let init = d.init(&cfg(&def)).unwrap();
    let mut s = d.session(init);
    assert_eq!(
        d.tester_present(&mut s).unwrap(),
        SessionOutcome::TesterPresent(Some(Reply::Positive(vec![0x36])))
    );
    let r = positive(&d.request(&mut s, &[0x58]).unwrap());
    assert_eq!(r, CAR_58);
    let items = def
        .request("Fault memory")
        .unwrap()
        .decode_checked(&def, &r)
        .unwrap();
    let code = items.iter().find(|i| i.name == "Fault code 1").unwrap();
    assert_eq!(code.display.as_deref(), Some("front left sensor signal"));
    assert_eq!(
        d.stop(&mut s).unwrap(),
        SessionOutcome::Stopped(Some(Reply::Positive(vec![0x3F])))
    );
    assert!(j.violations().is_empty(), "{:?}", j.violations());
}

#[test]
fn real_x24_definition_drives_the_simulator() {
    let Some(bytes) = ddt_file("X24_ABS_Bosch5.3.xml") else {
        return;
    };
    let def = kline_spec::EcuDef::from_xml_bytes(&bytes).unwrap();
    let profile = EcuProfile::from_def(&def).unwrap();
    assert!(profile.is_bosch_abs());
    let sc = Scenario::new().text("Défaut mémoire 1", "signal capteur AVG");
    let ecu = SpecEcu::new(profile, def.clone(), sc).unwrap();
    let (mut d, journal) = drive(ecu);
    let init = d.init(&cfg(&def)).unwrap();
    let mut s = d.session(init);
    let r = positive(&d.request(&mut s, &[0x58]).unwrap());
    assert_eq!(r.len(), 16);
    assert_eq!(&r[..2], &[0x58, 0x08]);
    let r = positive(&d.request(&mut s, &[0x51]).unwrap());
    assert_eq!((r[0], r.len()), (0x51, 24));
    journal.assert_no_writes();
}
