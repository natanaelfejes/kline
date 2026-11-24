//! The simulated car of the `kline-sim` runner: hand-written definitions (not derived from any
//! Renault file) and their values, for the four ECUs the owner's sessions use.
//!
//! | Name | Address | Definition | Written by `--write-defs` as |
//! |---|---|---|---|
//! | `sirius` | 7A | `tests/fixtures/builtin/sirius.xml` | `SIRIUS32___EMS3132___Soft_RE1_maj.xml` |
//! | `bmt` | 26 | `tests/fixtures/builtin/bmt.xml` | `BMT_X65_ph1_p.xml` |
//! | `airbag` | 2C | `tests/fixtures/builtin/airbag_b.xml` | `AB80_X65_X90_V1.xml` (and `airbag_a.xml` as `AB70_X65.xml`) |
//! | `abs` | 01 | `tests/fixtures/bosch_abs_iso8.xml` | `X24_ABS_Bosch5.3.xml` |
//!
//! The file names are the ones `kline --ecu sirius|bmt` and the session scripts expect in the
//! definitions directory, so `kline --defs DIR` finds the built-ins there. The contents are only
//! shaped like the car's ECUs (addresses, key bytes, request bytes from docs/protocol.md); the
//! names, positions and scalings are invented.

use std::path::{Path, PathBuf};

use kline_spec::{EcuDef, Value};

use crate::scenario::Scenario;

/// One built-in ECU.
pub struct Builtin {
    pub name: &'static str,
    pub address: u8,
    /// The definition the simulator answers from.
    pub xml: &'static str,
    /// Files `--write-defs` writes: (file name, contents).
    pub files: &'static [(&'static str, &'static str)],
    scenario: fn() -> Scenario,
}

const SIRIUS_XML: &str = include_str!("../tests/fixtures/builtin/sirius.xml");
const BMT_XML: &str = include_str!("../tests/fixtures/builtin/bmt.xml");
const AIRBAG_A_XML: &str = include_str!("../tests/fixtures/builtin/airbag_a.xml");
const AIRBAG_B_XML: &str = include_str!("../tests/fixtures/builtin/airbag_b.xml");
const ABS_XML: &str = include_str!("../tests/fixtures/bosch_abs_iso8.xml");

fn sirius_values() -> Scenario {
    Scenario::new()
        .text("Calibration", "SIM-7A01")
        .number("Engine speed", 0.0)
        .number("Vehicle speed", 0.0)
        .number("Battery voltage", 12.625)
        .number("Coolant temperature", 20.0)
        .number("Air temperature", 18.0)
        .number("Throttle position", 0.0)
        .text("Injector 1 circuit", "ok")
        .text("Coolant sensor circuit", "ok")
}

fn bmt_values() -> Scenario {
    Scenario::new()
        .text("Part number", "SIM026")
        .text("Driver door", "closed")
        .number("Option A", 1.0)
        .number("Supply voltage", 12.5)
        .text("Remote frame received", "no")
        .number("Remote count", 2.0)
        .text("Start allowed", "yes")
        .number("Speed", 0.0)
        .text("Door switch fault", "absent")
}

fn airbag_values() -> Scenario {
    Scenario::new()
        .set("Part number", Value::Hex("7700000001".into()))
        .set("Software", Value::Hex("0700".into()))
        .text("Driver pretensioner circuit", "open circuit")
        .text("Supply below 9 V", "yes")
        .number("Crash counter", 1.0)
        .number("Supply voltage", 12.5)
        .number("Driver pretensioner impedance", 12.75)
        .number("Driver airbag impedance", 4.5)
        .text("Passenger airbag fitted", "yes")
        .text("Warning lamp", "on")
}

/// The car's Bosch 5.3 ABS values of 2026-11-23 (one stored fault, docs/cli.md "ABS").
fn abs_values() -> Scenario {
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

pub static BUILTINS: [Builtin; 4] = [
    Builtin {
        name: "sirius",
        address: 0x7A,
        xml: SIRIUS_XML,
        files: &[("SIRIUS32___EMS3132___Soft_RE1_maj.xml", SIRIUS_XML)],
        scenario: sirius_values,
    },
    Builtin {
        name: "bmt",
        address: 0x26,
        xml: BMT_XML,
        files: &[("BMT_X65_ph1_p.xml", BMT_XML)],
        scenario: bmt_values,
    },
    Builtin {
        name: "airbag",
        address: 0x2C,
        xml: AIRBAG_B_XML,
        files: &[
            ("AB70_X65.xml", AIRBAG_A_XML),
            ("AB80_X65_X90_V1.xml", AIRBAG_B_XML),
        ],
        scenario: airbag_values,
    },
    Builtin {
        name: "abs",
        address: 0x01,
        xml: ABS_XML,
        files: &[("X24_ABS_Bosch5.3.xml", ABS_XML)],
        scenario: abs_values,
    },
];

impl Builtin {
    pub fn by_name(name: &str) -> Option<&'static Builtin> {
        BUILTINS.iter().find(|b| b.name.eq_ignore_ascii_case(name))
    }

    pub fn def(&self) -> EcuDef {
        EcuDef::from_xml_bytes(self.xml.as_bytes()).expect("built-in definition parses")
    }

    pub fn scenario(&self) -> Scenario {
        (self.scenario)()
    }
}

/// What [`write_defs`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct WrittenDefs {
    pub written: Vec<PathBuf>,
    /// Already there with exactly the built-in contents.
    pub unchanged: Vec<PathBuf>,
}

fn refuse(msg: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::AlreadyExists, msg)
}

/// Write every built-in definition into `dir` under the car's file names. Nothing is written
/// (an error) when `dir` or any of those names is a symbolic link, when one of those files
/// exists with other contents (the directory may hold the real definitions), or when `dir`
/// holds any other `.xml` file. A file that already has exactly the built-in contents is left
/// as it is, so the same directory can be used again.
pub fn write_defs(dir: &Path) -> std::io::Result<WrittenDefs> {
    let is_link = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink());
    if is_link(dir) {
        return Err(refuse(format!(
            "{} is a symbolic link; --write-defs wants a plain directory",
            dir.display()
        )));
    }
    std::fs::create_dir_all(dir)?;
    let files: Vec<(PathBuf, &str)> = BUILTINS
        .iter()
        .flat_map(|b| b.files.iter())
        .map(|(n, x)| (dir.join(n), *x))
        .collect();
    let mut out = WrittenDefs::default();
    for (p, x) in &files {
        if is_link(p) {
            return Err(refuse(format!(
                "{} is a symbolic link; --write-defs never writes through one",
                p.display()
            )));
        }
        if p.exists() {
            if std::fs::read(p)? != x.as_bytes() {
                return Err(refuse(format!(
                    "{} exists with other contents; --write-defs never overwrites a definition (use an empty directory)",
                    p.display()
                )));
            }
            out.unchanged.push(p.clone());
        }
    }
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        let xml = p.extension().is_some_and(|x| x.eq_ignore_ascii_case("xml"));
        if xml && !files.iter().any(|(f, _)| f == &p) {
            return Err(refuse(format!(
                "{} holds other definitions ({}); --write-defs wants a directory of its own",
                dir.display(),
                p.display()
            )));
        }
    }
    for (p, x) in &files {
        if !out.unchanged.contains(p) {
            std::fs::write(p, x)?;
            out.written.push(p.clone());
        }
    }
    Ok(out)
}
