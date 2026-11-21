//! Default-deny request policy (SAFETY.md rule 2 and 3). See docs/protocol.md "Safety".
//!
//! Every frame the session writes needs a [`Permit`], and the only ways to get one are
//! [`check`] (the read allowlist) and [`check_approved`] (two BMT routines behind an
//! [`Approval`]). The decision is made on the exact payload bytes, never on request names, so a
//! definition that lists a write cannot unlock it.

use alloc::vec::Vec;
use core::fmt;

use thiserror::Error;

/// BMT (UCH) K-line address. Approved routines exist only there.
pub const BMT_ADDRESS: u8 = 0x26;
/// BMT `21A4` reads the transponder state: immobilizer related, never sent (SAFETY.md rule 3).
pub const BMT_TRANSPONDER_READ: [u8; 2] = [0x21, 0xA4];
/// KWP StartDiagnosticSession default session.
pub const DEFAULT_SESSION: u8 = 0x81;
/// Renault diagnostic session (Sirius 32 `Start Diagnostic Session` is `10 A0`; DDT2000 opens it
/// before reads). Orchestrator policy decision, 2026-11-22.
pub const RENAULT_DIAG_SESSION: u8 = 0xA0;

/// Protocol family the payload is sent with; the allowlists differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Iso8,
    Kwp,
}

/// Who a payload is sent to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub protocol: Protocol,
    pub ecu_address: u8,
}

/// Why a payload was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    /// Nothing to send.
    Empty,
    /// The service byte is not on the allowlist for this protocol.
    ServiceNotAllowed { service: u8 },
    /// The service is allowed, but not with these parameters (or this length).
    ParametersNotAllowed { service: u8 },
    /// Immobilizer / transponder related (SAFETY.md rule 3).
    Immobilizer,
    /// One of the approved BMT routines, sent without an [`Approval`].
    ApprovalRequired,
    /// An [`Approval`] used for an ECU other than the BMT over KWP.
    ApprovalWrongTarget,
}

/// A refused request. Nothing was written.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("request {payload:02X?} denied: {reason:?}")]
pub struct Denied {
    pub payload: Vec<u8>,
    pub reason: DenyReason,
}

/// Proof that a payload passed the policy. Only this module can make one. Not `Clone`: one
/// permit is one write.
#[derive(Debug, PartialEq, Eq)]
pub struct Permit {
    payload: Vec<u8>,
    target: Target,
}

impl Permit {
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub fn target(&self) -> Target {
        self.target
    }
}

/// Check `payload` against the read allowlist.
///
/// KWP: `81`, `82`, `3E [x]`, `10 81`, `10 A0`, `1A [x]`, `17 ..`/`18 ..` (up to 4 bytes),
/// `21 [x]` (except BMT `21A4`). ISO8: `21 [x]`, `33 50`..`33 54`, and the ISO8 keep-alive and
/// stop frames `36` and `3F` as single bytes. Everything else is denied.
pub fn check(target: &Target, payload: &[u8]) -> Result<Permit, Denied> {
    let deny = |reason| Denied {
        payload: payload.to_vec(),
        reason,
    };
    let Some(&service) = payload.first() else {
        return Err(deny(DenyReason::Empty));
    };
    if is_approved_routine(payload) {
        return Err(deny(DenyReason::ApprovalRequired));
    }
    if target.ecu_address == BMT_ADDRESS
        && target.protocol == Protocol::Kwp
        && payload == BMT_TRANSPONDER_READ
    {
        return Err(deny(DenyReason::Immobilizer));
    }
    let len = payload.len();
    let params_ok = match (target.protocol, service) {
        // StartCommunication, StopCommunication.
        (Protocol::Kwp, 0x81 | 0x82) => Some(len == 1),
        // TesterPresent, optional responseRequired byte.
        (Protocol::Kwp, 0x3E) => Some(len <= 2),
        // StartDiagnosticSession: default (81) and Renault diagnostic (A0) sessions only.
        (Protocol::Kwp, 0x10) => {
            Some(len == 2 && (payload[1] == DEFAULT_SESSION || payload[1] == RENAULT_DIAG_SESSION))
        }
        // ReadEcuIdentification, optional identification option.
        (Protocol::Kwp, 0x1A) => Some(len <= 2),
        // ReadStatusOfDTC / ReadDTCByStatus.
        (Protocol::Kwp, 0x17 | 0x18) => Some(len <= 4),
        // ReadDataByLocalIdentifier (KWP and the Safir generic `21` request).
        (_, 0x21) => Some(len <= 2),
        // Safir "salve" reads 3350..3354.
        (Protocol::Iso8, 0x33) => Some(len == 2 && (0x50..=0x54).contains(&payload[1])),
        // ISO8 TesterPresent {36} and Stop {3F}. RE: KWP2000Diag4 ISO8Ecu init.
        (Protocol::Iso8, 0x36 | 0x3F) => Some(len == 1),
        _ => None,
    };
    match params_ok {
        Some(true) => Ok(Permit {
            payload: payload.to_vec(),
            target: *target,
        }),
        Some(false) => Err(deny(DenyReason::ParametersNotAllowed { service })),
        None => Err(deny(DenyReason::ServiceNotAllowed { service })),
    }
}

/// Permit the routine an [`Approval`] stands for. The approval is consumed.
pub fn check_approved(target: &Target, approval: Approval) -> Result<Permit, Denied> {
    let payload = approval.routine.payload();
    if target.ecu_address != BMT_ADDRESS || target.protocol != Protocol::Kwp {
        return Err(Denied {
            payload: payload.to_vec(),
            reason: DenyReason::ApprovalWrongTarget,
        });
    }
    Ok(Permit {
        payload: payload.to_vec(),
        target: *target,
    })
}

fn is_approved_routine(payload: &[u8]) -> bool {
    ApprovedRoutine::ALL.iter().any(|r| r.payload() == payload)
}

/// The two BMT remote-control routines that may be sent with an [`Approval`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovedRoutine {
    /// `31C7000000`: erase all remotes and learn remote 1.
    EraseRemotesAndLearnFirst,
    /// `31C8000000`: learn remote 2.
    LearnSecondRemote,
}

impl ApprovedRoutine {
    pub const ALL: [ApprovedRoutine; 2] = [
        ApprovedRoutine::EraseRemotesAndLearnFirst,
        ApprovedRoutine::LearnSecondRemote,
    ];

    /// The exact request bytes.
    pub fn payload(self) -> &'static [u8] {
        match self {
            ApprovedRoutine::EraseRemotesAndLearnFirst => &[0x31, 0xC7, 0x00, 0x00, 0x00],
            ApprovedRoutine::LearnSecondRemote => &[0x31, 0xC8, 0x00, 0x00, 0x00],
        }
    }

    /// The text the user must type to approve this routine.
    pub fn confirmation_phrase(self) -> &'static str {
        match self {
            ApprovedRoutine::EraseRemotesAndLearnFirst => "ERASE ALL REMOTES AND LEARN REMOTE 1",
            ApprovedRoutine::LearnSecondRemote => "LEARN REMOTE 2",
        }
    }
}

/// Permission to send one approved routine once.
///
/// Not `Clone`, `Copy`, `Default` or deserializable; the only constructor is
/// [`Approval::from_typed_confirmation`]. Intended use (CLI only, interactive, never in `--json`
/// mode or from MCP): show [`ApprovedRoutine::confirmation_phrase`], read a line from the
/// terminal, and pass what the user typed.
pub struct Approval {
    routine: ApprovedRoutine,
}

impl fmt::Debug for Approval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Approval")
            .field("routine", &self.routine)
            .finish()
    }
}

/// The typed text did not match the confirmation phrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("confirmation text does not match; routine not approved")]
pub struct ConfirmationMismatch;

impl Approval {
    /// Approve `routine` if `typed` (trimmed of surrounding whitespace) is exactly its
    /// confirmation phrase.
    pub fn from_typed_confirmation(
        routine: ApprovedRoutine,
        typed: &str,
    ) -> Result<Approval, ConfirmationMismatch> {
        if typed.trim() == routine.confirmation_phrase() {
            Ok(Approval { routine })
        } else {
            Err(ConfirmationMismatch)
        }
    }

    pub fn routine(&self) -> ApprovedRoutine {
        self.routine
    }
}
