//! Default-deny, tiered request policy (SAFETY.md rule 2 and 3, owner-approved policy v2 and
//! owner decision 2026-11-22). See docs/protocol.md "Safety".
//!
//! | Tier | What | Who can send it |
//! |---|---|---|
//! | [`Tier::Read`] | documented reads, `10 81`/`10 A0`, `3E` | anyone: [`check`] / `Session::request` (CLI, `--json`, MCP) |
//! | [`Tier::Service`] | clear DTC, output tests, adaptation reset, Sirius `31 A0 A0`, BMT remote learning and actuators | a human, one typed `Approval` per request |
//! | [`Tier::ConfigWrite`] | `3B` config writes | a human `Approval`, after the backup read and (BMT) the `21 80` identification the definition was chosen from |
//! | [`Tier::Expert`] | any other request that is not forbidden (undocumented service or identifier) | a human expert `Approval` that shows "unknown effect" |
//! | forbidden | immobilizer/transponder functions and config, `27`, `34`-`37`, sessions other than `10 81`/`10 A0`, link frames | nobody, never |
//!
//! Approvals exist only with the cargo feature `approved-routines` (off by default; interactive
//! CLI only). Without it, and in AI/MCP/`--json` use, [`assess`] still *prepares* any request:
//! it returns its [`RiskInfo`] (or the refusal) so the caller can print it with the exact command
//! for a human, but nothing except Tier 0 can be executed.
//!
//! Link frames (`81`, `82`, ISO8 `36`/`3F`) are not requests: only the init machines and
//! `Session::tester_present`/`Session::stop` send them ([`DenyReason::ProtocolReserved`]).
//!
//! Every frame the core writes carries a [`Permit`]. Decisions use the exact payload bytes and
//! the target, never request names, so a definition that lists a write cannot unlock it.

use alloc::vec::Vec;

use thiserror::Error;

/// BMT (UCH) K-line address.
pub const BMT_ADDRESS: u8 = 0x26;
/// BMT `21A4` "ETATS TRANSPONDEUR": transponder/immobilizer *status* flags. A read, Tier 0
/// under policy v2 (diagnose intermittent start refusals). Only the exact two bytes; anything
/// after `21 A4` is refused as immobilizer related.
pub const BMT_TRANSPONDER_READ: [u8; 2] = [0x21, 0xA4];
/// BMT routine function numbers (`31 [fn] ..`, "LANCEMENT DES ROUTINES") that write, learn or
/// code the immobilizer/transponder (policy v2).
pub const BMT_IMMOBILIZER_FUNCTIONS: [u8; 6] = [0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6];
/// "Anything resembling" them: on the BMT, a `30`/`31`/`32` payload whose second byte is in
/// this range is refused as [`DenyReason::Immobilizer`], except the two exact remote-learning
/// routines. Capability catalogue 9.1: 0xC0..=0xC8 are all immobilizer functions.
pub const BMT_IMMOBILIZER_RANGE: core::ops::RangeInclusive<u8> = 0xC0..=0xC8;
/// BMT `31C7000000` erase all remotes and learn remote 1, `31C8000000` learn remote 2.
pub const BMT_REMOTE_LEARNING: [[u8; 5]; 2] = [
    [0x31, 0xC7, 0x00, 0x00, 0x00],
    [0x31, 0xC8, 0x00, 0x00, 0x00],
];
/// BMT actuator outputs for `31 out param count` (lock motors, window). Capability catalogue
/// 9.1 / 7 #14: 0xA0..=0xB3 are the documented outputs.
pub const BMT_ACTUATOR_OUTPUTS: core::ops::RangeInclusive<u8> = 0xA0..=0xB3;
/// BMT `3B D2 index value` configuration write and the read that backs it up
/// ("LECTURE CONFIGURATION" `21 A1`). Capability catalogue 3.4. On the BMT every other `3B`,
/// and every `2E`/`2F`, is refused: the BMT config holds key/remote options (AP1CLE, TRF) and
/// an undocumented identifier cannot be shown not to touch the immobilizer.
pub const BMT_CONFIG_WRITE_LID: u8 = 0xD2;
pub const BMT_CONFIG_READ: [u8; 2] = [0x21, 0xA1];
/// BMT config index 2 is AP1CLE (key related): refused (rule 3).
pub const BMT_CONFIG_IMMOBILIZER_INDEX: u8 = 0x02;
/// Highest documented BMT config index; 14 and 15 are reserved (catalogue 3.4) and only
/// reachable as [`Tier::Expert`].
pub const BMT_CONFIG_MAX_INDEX: u8 = 13;
/// Sirius special functions `31 A0 fn` (catalogue 4.3). 0xA0 (injector inhibition) stops the
/// engine: Tier 1 with [`Warning::StopsEngine`]. 0xA3 (reset of the backed-up memory) can touch
/// the immobilizer "verrou logiciel" area: forbidden always. Coordinator decision 2026-11-22.
pub const SIRIUS_SPECIAL_FUNCTION: u8 = 0xA0;
pub const SIRIUS_INJECTOR_INHIBITION: u8 = 0xA0;
pub const SIRIUS_BACKUP_MEMORY_RESET: u8 = 0xA3;
/// Sirius local identifiers whose frames carry immobilizer / "verrou logiciel" fields
/// (catalogue 4.4: Trame 01 `A0`, Trame 03 `A2`, Trame 15 `AE` verrou type, Trame 30 `BD`).
/// Reading them is Tier 0; any `2E`/`2F`/`3B` with one of them is refused always.
pub const SIRIUS_IMMOBILIZER_LIDS: [u8; 4] = [0xA0, 0xA2, 0xAE, 0xBD];
/// ECU identification read; the definition used for a Tier 2 write must have been chosen from
/// this session's reply to it.
pub const IDENTIFICATION_READ: [u8; 2] = [0x21, 0x80];
/// KWP StartDiagnosticSession default session.
pub const DEFAULT_SESSION: u8 = 0x81;
/// Renault diagnostic session (Sirius 32 `Start Diagnostic Session` is `10 A0`; DDT2000 opens it
/// before reads). Orchestrator policy decision, 2026-11-22.
pub const RENAULT_DIAG_SESSION: u8 = 0xA0;
/// Safir "salve" reads `33 xx` present in Safir_1.xml and Safir_2.xml (there is no `33 53`).
pub const SAFIR_SALVES: [u8; 4] = [0x50, 0x51, 0x52, 0x54];

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

/// Policy tier of a payload that is not forbidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Tier 0: documented reads. Autonomous use allowed (MCP, `--json`).
    Read,
    /// Tier 1: documented service actions. Human typed approval per request, interactive TTY.
    Service,
    /// Tier 2: documented configuration writes. Approval plus backup read and identification in
    /// the same session.
    ConfigWrite,
    /// Tier 3: anything else that is not forbidden. Human expert approval, unknown effect.
    Expert,
}

/// Machine-readable warning flags of a [`RiskInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Warning {
    /// Stops the engine or prevents it from running (Sirius `31 A0 A0`; undo with `32 A0 00`).
    StopsEngine,
    /// Erases stored fault codes / fault evidence.
    ErasesFaultMemory,
    /// Erases learned adaptation values.
    ErasesLearnedValues,
    /// Erases the learned remote controls.
    ErasesRemotes,
    /// Drives an output (relay, valve, fan, motor, lamp).
    MovesActuator,
    /// Writes non-volatile ECU memory; an interrupted write can corrupt it.
    EepromWrite,
    /// Undocumented request: the effect is unknown.
    UnknownEffect,
}

/// What a non-read request does, shown to the human before approval and quoted by the
/// approval (sources: ECU definitions and the capability catalogue).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiskInfo {
    pub tier: Tier,
    pub what_it_changes: &'static str,
    pub what_can_go_wrong: &'static str,
    pub reversible: bool,
    pub prerequisites: &'static [&'static str],
    pub warnings: &'static [Warning],
}

/// Why a payload was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    /// Nothing to send.
    Empty,
    /// Not a documented request; reachable only as [`Tier::Expert`] with an expert approval.
    ServiceNotAllowed { service: u8 },
    /// Documented service, undocumented parameters or length; reachable only as
    /// [`Tier::Expert`].
    ParametersNotAllowed { service: u8 },
    /// Writes, learns or codes the immobilizer/transponder, or may (SAFETY.md rule 3).
    /// Forbidden always, also in expert mode.
    Immobilizer,
    /// Security access `27`, download/upload/transfer `34`-`37`, a diagnostic session other than
    /// `10 81`/`10 A0`. Forbidden always, also in expert mode.
    Forbidden { service: u8 },
    /// A link-layer frame (`81`, `82`, ISO8 `36`/`3F`): sent only by the init machines and
    /// `Session::tester_present` / `Session::stop`, never as a request.
    ProtocolReserved { service: u8 },
    /// Tier 1, 2 or 3: needs a human `Approval` of this tier.
    ApprovalRequired { tier: Tier },
    /// The `Approval` is for another session, target, payload, tier or risk, or was already used.
    ApprovalMismatch,
    /// Tier 2: the backup read (`21 identifier`) was not answered positively earlier in this
    /// session.
    PreReadRequired { identifier: u8 },
    /// Tier 2: `21 80` was not read in this session, or the definition was chosen for another
    /// identification reply than the latest one.
    IdentificationMismatch,
}

impl DenyReason {
    /// True for refusals that no approval can lift.
    pub fn is_forbidden(&self) -> bool {
        matches!(
            self,
            DenyReason::Empty
                | DenyReason::Immobilizer
                | DenyReason::Forbidden { .. }
                | DenyReason::ProtocolReserved { .. }
        )
    }
}

/// A refused request. Nothing was written.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("request {payload:02X?} denied: {reason:?}")]
pub struct Denied {
    pub payload: Vec<u8>,
    pub reason: DenyReason,
}

impl Denied {
    pub(crate) fn new(payload: &[u8], reason: DenyReason) -> Denied {
        Denied {
            payload: payload.to_vec(),
            reason,
        }
    }
}

/// Proof that a payload passed the policy for a target. Only this crate can make one, and only
/// this crate can turn one into a write. Not `Clone`: one permit is one write.
#[derive(Debug, PartialEq, Eq)]
pub struct Permit {
    payload: Vec<u8>,
    target: Target,
    tier: Tier,
}

impl Permit {
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub fn target(&self) -> Target {
        self.target
    }

    pub fn tier(&self) -> Tier {
        self.tier
    }

    pub(crate) fn new(payload: &[u8], target: Target, tier: Tier) -> Permit {
        Permit {
            payload: payload.to_vec(),
            target,
            tier,
        }
    }
}

/// Link-layer frames the machines send themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtocolFrame {
    /// KWP StartCommunication `81` (fast init).
    StartCommunication,
    /// KWP `3E` / ISO8 `36`.
    TesterPresent,
    /// KWP StopCommunication `82` / ISO8 `3F`.
    Stop,
}

/// Permit for a link-layer frame. ISO8 `36` and `3F` are protocol-required (RE: KWP2000Diag4
/// ISO8Ecu init); they are not in the Safir definitions and are not reachable as requests.
pub(crate) fn protocol_permit(target: &Target, frame: ProtocolFrame) -> Permit {
    let payload: &[u8] = match (target.protocol, frame) {
        (Protocol::Kwp, ProtocolFrame::StartCommunication) => &[0x81],
        (Protocol::Kwp, ProtocolFrame::TesterPresent) => &[0x3E],
        (Protocol::Kwp, ProtocolFrame::Stop) => &[0x82],
        (Protocol::Iso8, ProtocolFrame::TesterPresent) => &[0x36],
        (Protocol::Iso8, ProtocolFrame::Stop) => &[0x3F],
        // ISO8 has no StartCommunication and FastInit is KWP only; Exchange::new would refuse
        // the protocol mismatch of a KWP framing anyway.
        (Protocol::Iso8, ProtocolFrame::StartCommunication) => &[0x81],
    };
    Permit::new(payload, *target, Tier::Read)
}

fn is_remote_learning(payload: &[u8]) -> bool {
    BMT_REMOTE_LEARNING.iter().any(|r| r[..] == *payload)
}

/// The refusals no approval can lift (also in expert mode), or `None`.
pub fn forbidden(target: &Target, payload: &[u8]) -> Option<DenyReason> {
    let Some(&service) = payload.first() else {
        return Some(DenyReason::Empty);
    };
    let p1 = payload.get(1).copied();
    let bmt = target.ecu_address == BMT_ADDRESS;
    // Rule 3 first, on any protocol (security review M2 F3b: no protocol confusion).
    if bmt {
        let immo = match service {
            0x30..=0x32 => {
                p1.is_some_and(|b| BMT_IMMOBILIZER_RANGE.contains(&b))
                    && !is_remote_learning(payload)
            }
            0x2E | 0x2F => true,
            0x3B => {
                p1 != Some(BMT_CONFIG_WRITE_LID)
                    || payload.get(2) == Some(&BMT_CONFIG_IMMOBILIZER_INDEX)
            }
            0x21 => payload.len() > 2 && payload[..2] == BMT_TRANSPONDER_READ,
            _ => false,
        };
        if immo {
            return Some(DenyReason::Immobilizer);
        }
    }
    // Sirius rules on any protocol too (re-review M2 finding 1: a Target { Iso8, 0x7A } must not
    // turn `31 A0 A3` or a write to an immobilizer frame into an expert request).
    if !bmt {
        let reset = payload.len() >= 3
            && payload[..3] == [0x31, SIRIUS_SPECIAL_FUNCTION, SIRIUS_BACKUP_MEMORY_RESET];
        let immo_write = matches!(service, 0x2E | 0x2F | 0x3B)
            && p1.is_some_and(|b| SIRIUS_IMMOBILIZER_LIDS.contains(&b));
        if reset || immo_write {
            return Some(DenyReason::Immobilizer);
        }
    }
    match (target.protocol, service) {
        (Protocol::Kwp, 0x81 | 0x82) | (Protocol::Iso8, 0x36 | 0x3F) => {
            Some(DenyReason::ProtocolReserved { service })
        }
        (_, 0x27) | (Protocol::Kwp, 0x34..=0x37) | (Protocol::Iso8, 0x34 | 0x35 | 0x37) => {
            Some(DenyReason::Forbidden { service })
        }
        // SAFETY.md rule 3: sessions other than 10 81 / 10 A0 are forbidden always, whatever
        // the protocol (re-review M2 finding 1).
        (_, 0x10)
            if !(payload.len() == 2
                && matches!(p1, Some(DEFAULT_SESSION | RENAULT_DIAG_SESSION))) =>
        {
            Some(DenyReason::Forbidden { service })
        }
        _ => None,
    }
}

/// Documented tier of `payload` for `target`, or why it is refused. Undocumented requests are
/// refused here with `ServiceNotAllowed`/`ParametersNotAllowed`; [`assess`] reports them as
/// [`Tier::Expert`].
///
/// KWP, Tier 0: `3E [x]`, `10 81`, `10 A0`, `1A xx`, `21 xx` exactly two bytes (any ECU, BMT
/// `21A4` included; `21 A1 xx` would write CALTERMI on the BMT), `17`/`18` with 1-3 parameter
/// bytes. ISO8 (Safir), Tier 0: `21` (bare), `33 50`, `33 51`, `33 52`, `33 54`.
/// KWP, Tier 1: `14 xx xx`; not BMT: `30 lid` + 1-4 bytes, `11 FF`, exactly `32 A0 00` (end of
/// injector inhibition, the only `32` in the definitions), `31 A0 A0`; BMT `31C7000000`, `31C8000000`, actuator `31 out param count` with
/// `out` in 0xA0..=0xB3. KWP, Tier 2: BMT `3B D2 index 00|01` (index 0..=13, not 2), other
/// ECUs `3B lid` + 1-4 bytes.
pub fn classify(target: &Target, payload: &[u8]) -> Result<Tier, Denied> {
    if let Some(reason) = forbidden(target, payload) {
        return Err(Denied::new(payload, reason));
    }
    let service = payload[0];
    let len = payload.len();
    let p1 = payload.get(1).copied();
    let bmt = target.ecu_address == BMT_ADDRESS;
    use Protocol::{Iso8, Kwp};
    use Tier::{ConfigWrite, Read, Service};
    let tier = match (target.protocol, service) {
        // TesterPresent, optional responseRequired byte.
        (Kwp, 0x3E) => (len <= 2).then_some(Read),
        // StartDiagnosticSession: forbidden() already refused everything but 10 81 / 10 A0.
        (Kwp, 0x10) => Some(Read),
        // ReadEcuIdentification / ReadDataByLocalIdentifier: exactly one identifier byte.
        (Kwp, 0x1A | 0x21) => (len == 2).then_some(Read),
        // ReadStatusOfDTC / ReadDTCByStatus.
        (Kwp, 0x17 | 0x18) => (2..=4).contains(&len).then_some(Read),
        // Safir generic request: the definitions only contain bare `21`.
        (Iso8, 0x21) => (len == 1).then_some(Read),
        // Safir salves present in the definitions.
        (Iso8, 0x33) => (len == 2 && p1.is_some_and(|b| SAFIR_SALVES.contains(&b))).then_some(Read),
        // Tier 1: ClearDiagnosticInformation with a 2-byte group (BMT 14FF00, Sirius 140000).
        (Kwp, 0x14) => (len == 3).then_some(Service),
        // Tier 1: InputOutputControlByLocalIdentifier (Sirius 30D0xxxx). The BMT definitions
        // have no `30`, `11` or `32`: Expert there (re-review M2 finding 2).
        (Kwp, 0x30) if !bmt => (3..=6).contains(&len).then_some(Service),
        // Tier 1: Renault "Reinit apprentissages" (adaptation reset), only `11 FF`.
        (Kwp, 0x11) if !bmt => (p1 == Some(0xFF) && len == 2).then_some(Service),
        // Tier 1: only `32 A0 00` (Sirius/S2000 "end of injector inhibition"); any other `32`
        // is undocumented: Expert (re-review M2 finding 2).
        (Kwp, 0x32) if !bmt => {
            (payload == [0x32, SIRIUS_SPECIAL_FUNCTION, 0x00]).then_some(Service)
        }
        // Tier 1, BMT: remote learning, and the actuator command `31 out param count`
        // ("COMMANDE DES ACTUATEURS" 31000000).
        (Kwp, 0x31) if bmt => (is_remote_learning(payload)
            || (len == 4 && p1.is_some_and(|b| BMT_ACTUATOR_OUTPUTS.contains(&b))))
        .then_some(Service),
        // Tier 1, other KWP ECUs (Sirius): only injector inhibition `31 A0 A0`.
        (Kwp, 0x31) => (payload == [0x31, SIRIUS_SPECIAL_FUNCTION, SIRIUS_INJECTOR_INHIBITION])
            .then_some(Service),
        // Tier 2, BMT: `3B D2 index value`, one boolean option (catalogue 3.4).
        (Kwp, 0x3B) if bmt => (len == 4 && payload[2] <= BMT_CONFIG_MAX_INDEX && payload[3] <= 1)
            .then_some(ConfigWrite),
        // Tier 2: WriteDataByLocalIdentifier (Sirius 3BD010).
        (Kwp, 0x3B) => (3..=6).contains(&len).then_some(ConfigWrite),
        _ => {
            return Err(Denied::new(
                payload,
                DenyReason::ServiceNotAllowed { service },
            ))
        }
    };
    tier.ok_or_else(|| Denied::new(payload, DenyReason::ParametersNotAllowed { service }))
}

/// Tier of any `payload`: its documented tier, [`Tier::Expert`] for an undocumented request,
/// or the refusal for a forbidden one.
pub fn tier_of(target: &Target, payload: &[u8]) -> Result<Tier, Denied> {
    match classify(target, payload) {
        Ok(t) => Ok(t),
        Err(d) if d.reason.is_forbidden() => Err(d),
        Err(_) => Ok(Tier::Expert),
    }
}

/// Prepare `payload` without sending anything: its [`RiskInfo`], or why it can never be sent.
/// This is what AI/MCP/`--json` callers use for non-read requests: print the risk and the exact
/// command for a human to run in the interactive CLI.
pub fn assess(target: &Target, payload: &[u8]) -> Result<RiskInfo, Denied> {
    let tier = tier_of(target, payload)?;
    Ok(risk_info(target, payload, tier))
}

const NONE: &[&str] = &[];

fn risk_info(target: &Target, payload: &[u8], tier: Tier) -> RiskInfo {
    let bmt = target.ecu_address == BMT_ADDRESS;
    let r = |what_it_changes, what_can_go_wrong, reversible, prerequisites, warnings| RiskInfo {
        tier,
        what_it_changes,
        what_can_go_wrong,
        reversible,
        prerequisites,
        warnings,
    };
    match (tier, payload) {
        (Tier::Read, _) => r("nothing (read)", "none known", true, NONE, &[]),
        (Tier::Service, [0x14, ..]) => r(
            "erases the stored fault codes and fault flags",
            "fault evidence is lost; intermittent faults stay invisible until they recur",
            false,
            &["read and save the fault frames first"],
            &[Warning::ErasesFaultMemory],
        ),
        (Tier::Service, [0x30, ..]) => r(
            "drives an output (fuel pump relay, fan, valve, lamp) while the test runs",
            "pump/fan/valve runs in the wrong engine state; battery drain",
            true,
            &[
                "engine off, ignition on",
                "read 21 AE (equipment) first",
                "few cycles, send the stop variant after",
            ],
            &[Warning::MovesActuator],
        ),
        (Tier::Service, [0x11, 0xFF]) => r(
            "erases learned adaptations (idle, mixture, throttle position)",
            "rough idle and poor running until the ECU relearns",
            false,
            &["drive cycle afterwards to relearn"],
            &[Warning::ErasesLearnedValues],
        ),
        (Tier::Service, [0x32, SIRIUS_SPECIAL_FUNCTION, 0x00]) => r(
            "ends the injector inhibition started by 31 A0 A0 (the engine may run again)",
            "the engine can start running if it is being cranked",
            true,
            &["vehicle stationary"],
            &[],
        ),
        (Tier::Service, [0x31, SIRIUS_SPECIAL_FUNCTION, SIRIUS_INJECTOR_INHIBITION]) if !bmt => r(
            "inhibits injection: the engine stops / will not run until 32 A0 00 or ignition cycle",
            "engine stops; never while driving",
            true,
            &["vehicle stationary"],
            &[Warning::StopsEngine],
        ),
        (Tier::Service, [0x31, 0xC7, ..]) => r(
            "erases all learned remote controls and learns remote 1",
            "no remote works until each one is relearned (31 C8 for remote 2)",
            false,
            &["all remotes at hand", "follow the remote pairing procedure"],
            &[Warning::ErasesRemotes],
        ),
        (Tier::Service, [0x31, 0xC8, ..]) => r(
            "learns remote 2",
            "remote not learned if the sequence timing is missed",
            false,
            &["right after 31 C7 with remote 1"],
            &[],
        ),
        (Tier::Service, [0x31, ..]) => r(
            "drives a body output (door lock motor, window, lights) as given in byte 2",
            "lock or window moves; keep hands clear",
            true,
            &["doors and window clear"],
            &[Warning::MovesActuator],
        ),
        (Tier::ConfigWrite, [0x3B, BMT_CONFIG_WRITE_LID, ..]) if bmt => r(
            "rewrites one BMT configuration option (index in byte 3, value in byte 4) in EEPROM",
            "index meaning differs between BMT_X65_ph1_p and BMT_SAGEM: a wrong definition changes another option; an interrupted write can corrupt the config",
            true,
            &[
                "21 80 read and the definition chosen from it",
                "21 A1 backup read in this session",
                "read back 21 A1 after the write",
            ],
            &[Warning::EepromWrite],
        ),
        (Tier::ConfigWrite, _) => r(
            "writes an ECU setting (e.g. Sirius idle correction) in EEPROM",
            "wrong setting changes engine behaviour; an interrupted write can corrupt it",
            true,
            &[
                "21 80 read and the definition chosen from it",
                "21 <identifier> backup read in this session",
                "read back after the write",
            ],
            &[Warning::EepromWrite],
        ),
        _ => r(
            "unknown effect: undocumented service or identifier",
            "unknown: may write EEPROM, move actuators, erase data or drop communication",
            false,
            &["you know what this exact request does on this ECU"],
            &[Warning::UnknownEffect],
        ),
    }
}

/// Permit a Tier 0 (read) payload. Every other tier is refused with
/// [`DenyReason::ApprovalRequired`]; forbidden payloads with their reason.
pub fn check(target: &Target, payload: &[u8]) -> Result<Permit, Denied> {
    match classify(target, payload)? {
        Tier::Read => Ok(Permit::new(payload, *target, Tier::Read)),
        tier => Err(Denied::new(payload, DenyReason::ApprovalRequired { tier })),
    }
}

/// The read whose positive reply must exist in the same session before a Tier 2 `payload` is
/// approved (mandatory pre-read backup): `21 A1` for BMT `3B D2 ..`, else `21 lid` for
/// `3B lid ..`.
pub fn config_backup_read(target: &Target, payload: &[u8]) -> Option<[u8; 2]> {
    match payload {
        [0x3B, BMT_CONFIG_WRITE_LID, ..] if target.ecu_address == BMT_ADDRESS => {
            Some(BMT_CONFIG_READ)
        }
        [0x3B, lid, ..] => Some([0x21, *lid]),
        _ => None,
    }
}

/// The typed text did not match the confirmation code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("confirmation code does not match; request not approved")]
pub struct ConfirmationMismatch;

#[cfg(feature = "approved-routines")]
pub(crate) use approval::random_u64 as approval_random;
#[cfg(feature = "approved-routines")]
pub(crate) use approval::ChallengeIds;
#[cfg(feature = "approved-routines")]
pub use approval::{Approval, ApprovalChallenge};

/// Human approval of one Tier 1/2 request (feature `approved-routines`, off by default).
///
/// Only the interactive `kline` CLI may enable the feature. It must read the confirmation from
/// `/dev/tty` (not stdin), and must never offer it with `--json`; an MCP server or any other
/// non-interactive binary must be built without the feature, so no approval type exists there.
#[cfg(feature = "approved-routines")]
mod approval {
    use alloc::string::String;
    use alloc::vec::Vec;
    use core::fmt;

    use super::{ConfirmationMismatch, RiskInfo, Target, Tier};

    /// Characters of a confirmation code: no 0/O, 1/I/L, 2/Z, 5/S, 8/B look-alikes. ours.
    const CODE_ALPHABET: &[u8] = b"ACDEFGHJKMNPQRTUVWXY34679";
    /// Length of a confirmation code. ours.
    const CODE_LEN: usize = 6;
    /// Length of an expert confirmation code. ours: longer, so it cannot be typed by habit.
    const EXPERT_CODE_LEN: usize = 10;

    /// Random number from the OS. `None` when the OS source fails (then nothing is approved).
    pub(crate) fn random_u64() -> Option<u64> {
        getrandom::u64().ok()
    }

    fn code_from(mut r: u64, len: usize) -> String {
        let n = CODE_ALPHABET.len() as u64;
        (0..len)
            .map(|_| {
                let c = CODE_ALPHABET[(r % n) as usize] as char;
                r /= n;
                c
            })
            .collect()
    }

    /// The random values of one challenge: session nonce, request id, code seed.
    pub(crate) struct ChallengeIds {
        pub(crate) session: u64,
        pub(crate) request: u64,
        pub(crate) code: u64,
    }

    /// A pending approval request made by `Session::approval_challenge`: show
    /// [`ApprovalChallenge::payload`] with what it does and erases, and
    /// [`ApprovalChallenge::code`], then pass what the user typed on the terminal to
    /// [`Approval::confirm`]. A new challenge on the same session invalidates the previous one.
    pub struct ApprovalChallenge {
        pub(crate) session: u64,
        pub(crate) request: u64,
        pub(crate) target: Target,
        pub(crate) tier: Tier,
        pub(crate) payload: Vec<u8>,
        pub(crate) identification: Option<Vec<u8>>,
        pub(crate) risk: RiskInfo,
        code: String,
    }

    impl ApprovalChallenge {
        pub(crate) fn new(
            ids: ChallengeIds,
            target: Target,
            tier: Tier,
            payload: &[u8],
            identification: Option<&[u8]>,
            risk: RiskInfo,
        ) -> ApprovalChallenge {
            let len = if tier == Tier::Expert {
                EXPERT_CODE_LEN
            } else {
                CODE_LEN
            };
            ApprovalChallenge {
                session: ids.session,
                request: ids.request,
                target,
                tier,
                payload: payload.to_vec(),
                identification: identification.map(<[u8]>::to_vec),
                risk,
                code: code_from(ids.code, len),
            }
        }

        /// What the request changes and can break; must be shown in full before asking.
        pub fn risk(&self) -> &RiskInfo {
            &self.risk
        }

        /// The exact request bytes that will be sent.
        pub fn payload(&self) -> &[u8] {
            &self.payload
        }

        pub fn target(&self) -> Target {
            self.target
        }

        pub fn tier(&self) -> Tier {
            self.tier
        }

        /// Random code the user must type, different for every challenge.
        pub fn code(&self) -> &str {
            &self.code
        }
    }

    impl fmt::Debug for ApprovalChallenge {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("ApprovalChallenge")
                .field("target", &self.target)
                .field("tier", &self.tier)
                .field("payload", &self.payload)
                .finish_non_exhaustive()
        }
    }

    /// Permission to send one exact payload once, to one target, in one session.
    ///
    /// Not `Clone`, `Copy`, `Default` or deserializable; the only constructor is
    /// [`Approval::confirm`], which consumes the challenge. Consumed by
    /// `Session::request_approved` whether or not the request goes out.
    pub struct Approval {
        pub(crate) session: u64,
        pub(crate) request: u64,
        pub(crate) target: Target,
        pub(crate) tier: Tier,
        pub(crate) payload: Vec<u8>,
        pub(crate) identification: Option<Vec<u8>>,
        pub(crate) risk: RiskInfo,
    }

    impl Approval {
        /// Approve the challenge if `typed` (trimmed of surrounding whitespace) is exactly its
        /// code.
        pub fn confirm(
            challenge: ApprovalChallenge,
            typed: &str,
        ) -> Result<Approval, ConfirmationMismatch> {
            if typed.trim() != challenge.code {
                return Err(ConfirmationMismatch);
            }
            Ok(Approval {
                session: challenge.session,
                request: challenge.request,
                target: challenge.target,
                tier: challenge.tier,
                payload: challenge.payload,
                identification: challenge.identification,
                risk: challenge.risk,
            })
        }

        pub fn payload(&self) -> &[u8] {
            &self.payload
        }

        pub fn tier(&self) -> Tier {
            self.tier
        }

        /// The risk the human confirmed; `Session::request_approved` refuses the request if the
        /// policy's current risk for it differs.
        pub fn risk(&self) -> &RiskInfo {
            &self.risk
        }
    }

    impl fmt::Debug for Approval {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("Approval")
                .field("target", &self.target)
                .field("tier", &self.tier)
                .field("payload", &self.payload)
                .finish_non_exhaustive()
        }
    }
}

/// True when this build of kline-core has the `approved-routines` feature (approval types
/// exist). Lets a binary that must never approve anything (kline-agent) detect a build in which
/// cargo unified the feature in from another workspace member.
pub const APPROVALS_COMPILED: bool = cfg!(feature = "approved-routines");

/// Substrings (lowercase, accents folded, spaces and punctuation removed) that mark a request
/// or data name as immobilizer / transponder / key related. Coordinator scope addition
/// 2026-11-22 (generic ECUs): anti-demarrage, transpondeur, immo, VERLOG, verrou, APPTRP,
/// AP1CLE, code cle / key code. `wegfahr` (German Wegfahrsperre) is ours. Deliberately broad:
/// "verrou" also matches central locking ("verrouillage"), which is refused too.
pub const IMMOBILIZER_SUBSTRINGS: &[&str] = &[
    "antidemarr",
    "transpond",
    "immo",
    "verlog",
    "verrou",
    "apptrp",
    "ap1cle",
    "codecle",
    "clecode",
    "keycode",
    "wegfahr",
];
/// Whole words (lowercase, accents folded) with the same meaning: `cle`/`cles` (French key, as
/// in "APPRENT CLE 2 TEL") and `key`/`keys`.
pub const IMMOBILIZER_WORDS: &[&str] = &["cle", "cles", "key", "keys"];

fn fold(c: char) -> Option<char> {
    let c = match c {
        'à' | 'â' | 'ä' | 'á' | 'À' | 'Â' | 'Ä' | 'Á' => 'a',
        'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'e',
        'î' | 'ï' | 'í' | 'Î' | 'Ï' | 'Í' => 'i',
        'ô' | 'ö' | 'ó' | 'Ô' | 'Ö' | 'Ó' => 'o',
        'ù' | 'û' | 'ü' | 'ú' | 'Ù' | 'Û' | 'Ü' | 'Ú' => 'u',
        'ç' | 'Ç' => 'c',
        c => c.to_ascii_lowercase(),
    };
    c.is_ascii_alphanumeric().then_some(c)
}

/// The immobilizer/transponder/key keyword `name` contains ([`IMMOBILIZER_SUBSTRINGS`],
/// [`IMMOBILIZER_WORDS`]), if any. Used to refuse Tier 1-3 requests of ECU definitions by
/// name (SAFETY.md rule 3 for ECUs the address-based rules above do not know).
pub fn immobilizer_keyword(name: &str) -> Option<&'static str> {
    let compact: alloc::string::String = name.chars().filter_map(fold).collect();
    if let Some(k) = IMMOBILIZER_SUBSTRINGS
        .iter()
        .find(|k| compact.contains(**k))
    {
        return Some(k);
    }
    let mut word = alloc::string::String::new();
    for c in name.chars().chain(core::iter::once(' ')) {
        match fold(c) {
            Some(f) => word.push(f),
            None => {
                if let Some(k) = IMMOBILIZER_WORDS.iter().find(|k| **k == word) {
                    return Some(k);
                }
                word.clear();
            }
        }
    }
    None
}
