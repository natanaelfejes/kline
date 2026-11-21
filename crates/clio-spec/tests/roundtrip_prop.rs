//! Property: for every scaled or list data of the three definitions used on the car,
//! `encode(interpret(raw)) == raw` and `encode_exact(interpret(raw)) == raw` over the whole raw
//! range.

mod common;

use clio_spec::{DataDef, Raw};
use common::ddt_ecu;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};

const FILES: &[&str] = &["Safir_1.xml", "Safir_2.xml", "BMT_X65_ph1_p.xml"];

fn round_trips(d: &DataDef, raw: u64) -> Result<(), TestCaseError> {
    let raw = Raw::Int(raw);
    let bytes = d
        .raw_to_bytes(&raw)
        .map_err(|e| TestCaseError::fail(e.to_string()))?;
    let (value, _display) = d
        .interpret(&bytes)
        .ok_or_else(|| TestCaseError::fail("no value"))?;
    let back = d
        .encode(&value)
        .map_err(|e| TestCaseError::fail(format!("{}: encode {value:?}: {e}", d.name)))?;
    prop_assert_eq!(&back, &raw, "{} via {:?}", d.name, value);
    // Decoded values lie on the Step grid, so the strict encoder accepts them too.
    let exact = d
        .encode_exact(&value)
        .map_err(|e| TestCaseError::fail(format!("{}: encode_exact {value:?}: {e}", d.name)))?;
    prop_assert_eq!(exact, raw, "{} via {:?} (exact)", d.name, value);
    Ok(())
}

#[test]
fn encode_inverts_decode_for_scaled_and_list_data() {
    for file in FILES {
        let Some(ecu) = ddt_ecu(file) else { continue };
        let defs: Vec<DataDef> = ecu
            .datas
            .iter()
            .filter(|d| d.scaled.is_some() || !d.list.is_empty())
            .cloned()
            .collect();
        assert!(!defs.is_empty(), "{file}");
        assert!(defs.iter().all(|d| d.bits_count <= 64 && !d.ascii));

        let strategy = (0..defs.len()).prop_flat_map(|i| {
            let bits = defs[i].bits_count;
            let max = if bits >= 64 {
                u64::MAX
            } else {
                (1u64 << bits) - 1
            };
            (Just(i), 0..=max)
        });
        let mut runner = TestRunner::new(Config {
            cases: 20_000,
            failure_persistence: None,
            ..Config::default()
        });
        runner
            .run(&strategy, |(i, raw)| round_trips(&defs[i], raw))
            .unwrap_or_else(|e| panic!("{file}: {e}"));

        // Range ends explicitly (proptest may not hit them for every data).
        for d in &defs {
            let max = (1u64 << d.bits_count) - 1;
            for raw in [0, max, max / 2, max / 2 + 1] {
                round_trips(d, raw).unwrap_or_else(|e| panic!("{file}: {e}"));
            }
        }
    }
}
