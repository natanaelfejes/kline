//! Host-side runtime of the `clio` and `clio-agent` binaries (docs/cli.md).
//!
//! Everything here is Tier 0: this crate depends on clio-core **without** the
//! `approved-routines` feature and has no way to send a non-read request. The interactive
//! `clio` binary (crates/clio-cli) adds `exec` on top; `clio-agent` adds `prepare`.
//!
//! - [`transport`]: the FTDI KKL cable over `serialport` and the simulator's pty + control
//!   socket, both behind [`transport::Transport`].
//! - [`link`]: runs clio-core's init and session machines with real time, `--trace` and the
//!   JSONL capture ([`capture`]).
//! - [`defs`]: ECU definitions (`--ecu`, `--xml`, `--vehicle`, `--address`).
//! - [`cmd`]: the shared commands and the session lifecycle.

#![forbid(unsafe_code)]

pub mod capture;
pub mod cmd;
#[cfg(unix)]
pub mod control;
pub mod defs;
pub mod error;
pub mod link;
pub mod out;
pub mod ports;
pub mod trace;
pub mod transport;

use clio_core::safety::{RiskInfo, Tier, Warning};
use serde_json::{json, Value};

pub use error::{exit, CliError};

/// Uppercase hex without spaces (`21A9`).
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

/// Uppercase hex with spaces (`21 A9`).
pub fn hex_sp(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Hex bytes; spaces, `:` and `,` between bytes allowed; `0x` prefix allowed.
pub fn parse_hex(s: &str) -> Option<Vec<u8>> {
    let t = s.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    let t: String = t
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':' && *c != ',')
        .collect();
    if !t.len().is_multiple_of(2) || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..t.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&t[i..i + 2], 16).ok())
        .collect()
}

pub fn tier_name(t: Tier) -> &'static str {
    match t {
        Tier::Read => "read (tier 0)",
        Tier::Service => "service (tier 1)",
        Tier::ConfigWrite => "config write (tier 2)",
        Tier::Expert => "expert (tier 3)",
    }
}

pub fn warning_name(w: Warning) -> &'static str {
    match w {
        Warning::StopsEngine => "STOPS_ENGINE",
        Warning::ErasesFaultMemory => "ERASES_FAULT_MEMORY",
        Warning::ErasesLearnedValues => "ERASES_LEARNED_VALUES",
        Warning::ErasesRemotes => "ERASES_REMOTES",
        Warning::MovesActuator => "MOVES_ACTUATOR",
        Warning::EepromWrite => "EEPROM_WRITE",
        Warning::UnknownEffect => "UNKNOWN_EFFECT",
    }
}

pub fn risk_json(r: &RiskInfo) -> Value {
    json!({
        "tier": tier_name(r.tier),
        "tier_number": match r.tier { Tier::Read => 0, Tier::Service => 1, Tier::ConfigWrite => 2, Tier::Expert => 3 },
        "what_it_changes": r.what_it_changes,
        "what_can_go_wrong": r.what_can_go_wrong,
        "reversible": r.reversible,
        "prerequisites": r.prerequisites,
        "warnings": r.warnings.iter().map(|w| warning_name(*w)).collect::<Vec<_>>(),
    })
}

/// The RiskInfo verbatim, for a human.
pub fn risk_human(r: &RiskInfo) -> String {
    let mut s = format!(
        "tier:              {}\nwhat it changes:   {}\nwhat can go wrong: {}\nreversible:        {}",
        tier_name(r.tier),
        r.what_it_changes,
        r.what_can_go_wrong,
        if r.reversible { "yes" } else { "NO" }
    );
    if !r.prerequisites.is_empty() {
        s.push_str("\nprerequisites:");
        for p in r.prerequisites {
            s.push_str(&format!("\n  - {p}"));
        }
    }
    if !r.warnings.is_empty() {
        s.push_str(&format!(
            "\nWARNINGS:          {}",
            r.warnings
                .iter()
                .map(|w| warning_name(*w))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn hex_round_trip_and_separators() {
        assert_eq!(super::parse_hex("21 a9"), Some(vec![0x21, 0xA9]));
        assert_eq!(super::parse_hex("0x31C7000000").map(|v| v.len()), Some(5));
        assert_eq!(super::parse_hex("21:A9,00"), Some(vec![0x21, 0xA9, 0]));
        assert_eq!(super::parse_hex("2"), None);
        assert_eq!(super::parse_hex("Live A9"), None);
        assert_eq!(super::hex(&[0x21, 0xA9]), "21A9");
    }
}
