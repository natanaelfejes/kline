//! Host-side runtime of the `kline` and `kline-agent` binaries (docs/cli.md).
//!
//! Everything here is Tier 0: this crate depends on kline-core **without** the
//! `approved-routines` feature and has no way to send a non-read request. The interactive
//! `kline` binary (crates/kline-cli) adds `exec` on top; `kline-agent` adds `prepare`.
//!
//! - [`transport`]: the FTDI KKL cable over `serialport` and the simulator's pty + control
//!   socket, both behind [`transport::Transport`].
//! - [`link`]: runs kline-core's init and session machines with real time, `--trace` and the
//!   JSONL capture ([`capture`]).
//! - [`defs`]: ECU definitions (`--ecu`, `--xml`, `--vehicle`, `--address`).
//! - [`cmd`]: the shared commands and the session lifecycle.

// Only `hrtimer`'s Windows `timeBeginPeriod` FFI is allowed unsafe code (two integer calls).
#![deny(unsafe_code)]

#[cfg(feature = "ble")]
pub mod ble;
pub mod capture;
pub mod cmd;
#[cfg(unix)]
pub mod control;
pub mod defs;
pub mod error;
pub mod hrtimer;
pub mod latency;
pub mod link;
pub mod out;
pub mod ports;
pub mod selftest;
pub mod trace;
pub mod transport;

use kline_core::safety::{RiskInfo, Tier, Warning};
use serde_json::{json, Value};

pub use error::{exit, CliError};

/// The git commit this crate was built from: short hash, `-dirty` when the work tree had
/// changes, or `unknown` without git (build.rs, re-check R10). Recorded in every capture header.
pub const GIT_COMMIT: &str = env!("KLINE_GIT_COMMIT");
/// The package version, recorded next to [`GIT_COMMIT`].
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Uppercase hex without spaces (`21A9`).
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

/// One shell word, quoted only when needed (for printed command lines).
pub fn shell_word(w: &str) -> String {
    if !w.is_empty()
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=,@+".contains(c))
    {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', "'\\''"))
    }
}

/// Words joined as one shell command line ([`shell_word`]).
pub fn shell_join(words: &[String]) -> String {
    words
        .iter()
        .map(|w| shell_word(w))
        .collect::<Vec<_>>()
        .join(" ")
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

/// `s` safe to print on a terminal: control characters (C0, DEL, C1 such as the 8-bit CSI
/// `9B`), escape sequences and bidirectional overrides are replaced by a visible `\u{..}`
/// escape, so a crafted definition name cannot redraw or hide part of a security prompt.
pub fn sanitize(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        let bidi = matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
        if c.is_control() || bidi {
            o.push_str(&format!("\\u{{{:x}}}", c as u32));
        } else {
            o.push(c);
        }
    }
    o
}

/// Names joined with " / ", each [`sanitize`]d.
pub fn sanitize_list(names: &[String]) -> String {
    names
        .iter()
        .map(|n| sanitize(n))
        .collect::<Vec<_>>()
        .join(" / ")
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
        Warning::MayBrickEcu => "MAY_BRICK_ECU",
        Warning::MayLeaveProgrammingSession => "MAY_LEAVE_PROGRAMMING_SESSION",
        Warning::SecurityAccess => "SECURITY_ACCESS",
        Warning::ChangesRemoteType => "CHANGES_REMOTE_TYPE",
        Warning::SafetySystem => "SAFETY_SYSTEM",
    }
}

pub fn risk_json(r: &RiskInfo) -> Value {
    json!({
        "tier": tier_name(r.tier),
        "tier_number": match r.tier { Tier::Read => 0, Tier::Service => 1, Tier::ConfigWrite => 2, Tier::Expert => 3 },
        "what_it_changes": r.what_it_changes,
        "what_can_go_wrong": r.what_can_go_wrong,
        "safety_system": r.safety_system,
        "reversible": r.reversible,
        "prerequisites": r.prerequisites.iter().collect::<Vec<_>>(),
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
    if let Some(t) = r.safety_system {
        s.push_str(&format!("\nSAFETY SYSTEM:     {t}"));
    }
    if !r.prerequisites.is_empty() {
        s.push_str("\nprerequisites:");
        for p in &r.prerequisites {
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
    fn sanitize_escapes_controls_escape_sequences_and_bidi() {
        let s =
            super::sanitize("Reset\nAPPROVED \u{1b}[2J\u{1b}]0;x\u{7}\u{9b}31m\u{202e}ok\u{7f}");
        assert_eq!(
            s,
            "Reset\\u{a}APPROVED \\u{1b}[2J\\u{1b}]0;x\\u{7}\\u{9b}31m\\u{202e}ok\\u{7f}"
        );
        assert!(!s.chars().any(|c| c.is_control()));
        assert_eq!(super::sanitize("Réinit flags é"), "Réinit flags é");
    }

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
