//! The name guard of the car's definitions as compiled byte prefixes.
//!
//! The CLI refuses Tier 1-3 requests whose definition names look immobilizer related
//! (kline-core `spec::definition_guard`, SAFETY.md rule 3 "Name guard"). The firmware has no
//! definitions, so the guard's *byte prefixes* (not the names, not the XML) are compiled in
//! here, per ECU address, as the union over the definitions the CLI would load for this car:
//!
//! | Address | Definitions |
//! |---|---|
//! | 0x7A | SIRIUS32___EMS3132___Soft_RE1_maj.xml |
//! | 0x26 | BMT_X65_ph1_p.xml, BMT_SAGEM.xml |
//! | 0x01 | X24_ABS_Bosch5.3.xml, J64_Bosch5.3_4X4_V2.xml (no flagged request) |
//! | 0x2C | AB70_X65.xml, AB80_X65_X90_V1.xml |
//!
//! `tests/definition_parity.rs` rebuilds this table from `ddt-xml/` (skipped when absent) and
//! checks that it is exactly the definitions' guard, and that the bridge's tier or refusal of
//! every request in those files equals the CLI's.

/// `(address, fixed sent-byte prefix)` of every flagged request.
pub const NAME_GUARD_PREFIXES: &[(u8, &[u8])] = &[
    (0x7A, &[0x21, 0xA0]),
    (0x7A, &[0x21, 0xA2]),
    (0x7A, &[0x21, 0xAE]),
    (0x26, &[0x21, 0xA1]),
    (0x26, &[0x21, 0xA4]),
    (0x26, &[0x31, 0x00, 0x00, 0x00, 0x00]),
    (0x26, &[0x31, 0xA6]),
    (0x26, &[0x31, 0xAB]),
    (0x26, &[0x31, 0xC7, 0x00, 0x00, 0x00]),
    (0x26, &[0x31, 0xC8, 0x00, 0x00, 0x00]),
    (0x26, &[0x3B, 0xD2, 0x02]),
    (0x2C, &[0x21, 0x80]),
    (0x2C, &[0x21, 0x83]),
    (0x2C, &[0x21, 0x84]),
    (0x2C, &[0x21, 0x87]),
    (0x2C, &[0x3B, 0xD0]),
    (0x2C, &[0x3B, 0xD0, 0x00]),
    (0x2C, &[0x3B, 0xD0, 0xFF]),
    (0x2C, &[0x3B, 0xD6]),
    (0x2C, &[0x3B, 0xE0]),
];

/// The label the bridge's guard gives a compiled prefix (the names stay in the definitions).
pub const COMPILED_NAME: &str =
    "immobilizer-related request of the car's definition (compiled name guard)";
