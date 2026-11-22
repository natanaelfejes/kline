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
        CliError::port(format!("transport I/O error: {e}"))
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
        DenyReason::Forbidden { service } => {
            format!("service {service:02X} (security access, flashing or session change) is forbidden always")
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
