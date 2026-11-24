//! Deterministic exit codes (docs/cli.md "Exit codes") and the error type every command returns.

use std::fmt;

use kline_core::safety::{Denied, DenyReason};
use kline_core::session::SessionError;
use kline_core::LinkError;

/// Process exit codes. Stable: scripts and the agent rely on them (docs/cli.md).
pub mod exit {
    /// Success.
    pub const OK: u8 = 0;
    /// Unexpected internal error.
    pub const INTERNAL: u8 = 1;
    /// Command line usage error (clap uses 2 as well).
    pub const USAGE: u8 = 2;
    /// Serial port / transport: no cable found, cannot open, I/O error, several cables.
    pub const PORT: u8 = 3;
    /// Every init attempt failed (no keywords, K line inactive, collision, ...).
    pub const INIT: u8 = 4;
    /// Communication error after a successful init (no reply, collision, bad frame).
    pub const COMM: u8 = 5;
    /// The ECU answered a request negatively (`7F sid nrc`).
    pub const NEGATIVE: u8 = 6;
    /// Refused by the safety policy, the name guard, or because approvals need an
    /// interactive terminal (nothing was sent).
    pub const REFUSED: u8 = 7;
    /// ECU definition problem: file missing or invalid, CAN only, unknown request name.
    pub const DEFINITION: u8 = 8;
    /// Approval not given: wrong confirmation code, aborted or timed out (nothing was sent).
    pub const NOT_APPROVED: u8 = 9;
    /// The reply does not decode with the definition (wrong SID, too short, echo mismatch).
    pub const DECODE: u8 = 10;
    /// Capture / replay file problem (cannot write the capture, unreadable replay log).
    pub const CAPTURE: u8 = 11;
    /// `scan` stopped early (3 missing replies or a security NRC).
    pub const SCAN_STOPPED: u8 = 12;
    /// An approved request WAS written to the line but its reply was lost, damaged or timed
    /// out: its effect is unknown; read the state before anything else (final critique F06).
    pub const SENT_REPLY_LOST: u8 = 13;
}

#[derive(Debug, Clone)]
pub struct CliError {
    pub code: u8,
    /// Short machine-readable kind, e.g. `"port"`, `"denied"`.
    pub kind: &'static str,
    pub message: String,
}

impl CliError {
    pub fn new(code: u8, kind: &'static str, message: impl Into<String>) -> CliError {
        CliError {
            code,
            kind,
            message: message.into(),
        }
    }

    pub fn port(m: impl Into<String>) -> CliError {
        CliError::new(exit::PORT, "port", m)
    }

    pub fn definition(m: impl Into<String>) -> CliError {
        CliError::new(exit::DEFINITION, "definition", m)
    }

    pub fn refused(m: impl Into<String>) -> CliError {
        CliError::new(exit::REFUSED, "refused", m)
    }

    pub fn usage(m: impl Into<String>) -> CliError {
        CliError::new(exit::USAGE, "usage", m)
    }

    pub fn io(e: std::io::Error) -> CliError {
        CliError::io_ref(&e)
    }

    /// A transport error; a stalled write (the USB serial driver did not take the bytes within
    /// the write timeout) says what to check instead of only the OS message.
    pub fn io_ref(e: &std::io::Error) -> CliError {
        let hint = match e.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WriteZero => {
                " (the USB serial driver did not take the bytes in time: cable unplugged or the USB link stalled; replug the cable and check `kline ports`)"
            }
            _ => "",
        };
        CliError::port(format!("transport I/O error: {e}{hint}"))
    }

    pub fn denied(d: &Denied) -> CliError {
        let hint = match d.reason {
            DenyReason::ApprovalRequired { .. } => {
                "; not a read: see `kline risk`, a human can run it with `kline exec`"
            }
            _ => "",
        };
        CliError::refused(format!(
            "request {} refused by the safety policy: {}{hint}",
            crate::hex(&d.payload),
            deny_text(&d.reason)
        ))
    }

    pub fn session(e: &SessionError) -> CliError {
        match e {
            SessionError::Denied(d) => CliError::denied(d),
            SessionError::Link(l) => CliError::new(exit::COMM, "comm", format!("{l}")),
            e => CliError::new(exit::COMM, "comm", format!("{e}")),
        }
    }

    pub fn init(e: &LinkError) -> CliError {
        CliError::new(exit::INIT, "init", format!("init failed: {e}"))
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CliError {}

/// Human text of a refusal reason.
pub fn deny_text(r: &DenyReason) -> String {
    match r {
        DenyReason::Empty => "empty request".into(),
        DenyReason::ServiceNotAllowed { service } => {
            format!("service {service:02X} is not a documented request (expert tier)")
        }
        DenyReason::ParametersNotAllowed { service } => {
            format!("service {service:02X} with these parameters is undocumented (expert tier)")
        }
        DenyReason::Immobilizer => {
            "immobilizer/transponder related: forbidden always (SAFETY.md rule 3)".into()
        }
        DenyReason::ImmobilizerNamed { names } => format!(
            "definition request(s) {} look immobilizer/transponder/key related: forbidden always (SAFETY.md rule 3)",
            crate::sanitize_list(names)
        ),
        DenyReason::BoschAbsRoutine { service, names } if !names.is_empty() => format!(
            "{} (Bosch 5.3 ABS {service:02X}): permanently forbidden: factory fill rig / brake bleed; use a garage (SAFETY.md \"ABS / brake ECU\")",
            names.join(" / ")
        ),
        DenyReason::BoschAbsRoutine { service, .. } => format!(
            "ABS (ISO8 unit at 01) {}: permanently forbidden: a routine can leave a valve or the pump energized and kline cannot guarantee the OFF frame (release on 3F / keep-alive loss NOT VERIFIED); use a garage (SAFETY.md \"ABS / brake ECU\")",
            if *service == 0x41 { "41 RAM download" } else { "71 routine run" }
        ),
        DenyReason::SafetySystemForbidden { name } => format!(
            "{name}: permanently forbidden on this safety-system ECU, in every tier; use a garage (SAFETY.md \"Airbag ECU\" / \"ABS / brake ECU\")"
        ),
        DenyReason::OneApprovedRequestPerSession => {
            "one approved request per session on the ISO8 unit at 01 (ABS): close this session (Stop), read 58, then start a new one".into()
        }
        DenyReason::ProtocolReserved { service } => {
            format!("{service:02X} is a link frame, sent only by init / TesterPresent / Stop")
        }
        DenyReason::ApprovalRequired { tier } => {
            format!("{} tier, needs a human approval", crate::tier_name(*tier))
        }
        DenyReason::ApprovalMismatch => "approval does not match this request".into(),
        DenyReason::PreReadRequired { identifier } => {
            format!("backup read 21 {identifier:02X} required first in this session")
        }
        DenyReason::IdentificationMismatch => {
            "21 80 identification must be read in this session first".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_system_refusals_say_permanently_forbidden_and_what_it_is() {
        let t = kline_core::safety::Target {
            protocol: kline_core::safety::Protocol::Kwp,
            ecu_address: kline_core::safety::AIRBAG_ADDRESS,
            keywords: None,
        };
        let d = kline_core::safety::classify(&t, &[0x3B, 0xD0, 0xFF]).unwrap_err();
        let m = CliError::denied(&d).message;
        assert!(
            m.contains("firing line lock") && m.contains("permanently forbidden"),
            "{m}"
        );
        assert_eq!(CliError::denied(&d).code, exit::REFUSED);
    }

    #[test]
    fn a_stalled_write_gets_a_hint_other_io_errors_do_not() {
        let e = CliError::io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "serial write stalled: 0 of 5 bytes accepted within 210 ms",
        ));
        assert_eq!((e.code, e.kind), (exit::PORT, "port"));
        assert!(
            e.message
                .starts_with("transport I/O error: serial write stalled: 0 of 5 bytes")
                && e.message.contains("replug the cable"),
            "{}",
            e.message
        );
        let e = CliError::io(std::io::ErrorKind::WriteZero.into());
        assert!(
            e.message.contains("did not take the bytes"),
            "{}",
            e.message
        );
        let e = CliError::io(std::io::ErrorKind::BrokenPipe.into());
        assert!(!e.message.contains("replug"), "{}", e.message);
    }
}
