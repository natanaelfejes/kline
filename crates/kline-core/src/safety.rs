//! Default-deny, tiered request policy (SAFETY.md rules 2 and 3, owner-approved policy v2 and
//! owner decision 2026-11-22). See docs/protocol.md "Safety".
//!
//! | Tier | What | Who can send it |
//! |---|---|---|
//! | [`Tier::Read`] | documented reads, `10 81`/`10 A0`, `3E` | anyone: [`check`] / `Session::request` (CLI, `--json`, MCP) |
//! | [`Tier::Service`] | clear DTC, output tests, adaptation reset, Sirius `31 A0 00`, BMT remote learning and actuators | a human, one typed `Approval` per request |
//! | [`Tier::ConfigWrite`] | `3B` config writes | a human `Approval`, after the backup read and (BMT) the `21 80` identification the definition was chosen from |
//! | [`Tier::Expert`] | any other request that is not forbidden: undocumented services and identifiers, security access `27`, download/upload/transfer `34`-`37` and flash erase/checksum `31 81`/`31 82` (not on a BMT), other `10 xx` sessions, memory/routine-by-address services on non-immobilizer ECUs (with specific warnings such as [`Warning::MayBrickEcu`]) | a human expert `Approval` that shows the risk |
//! | forbidden | the immobilizer line only (owner decision 2026-11-22, final): on a BMT/UCH/decoder (address `26`/`A6` or a BMT/UCH/decoder definition, [`DefinitionGuard`]) `3D`, `38`-`3A`, `2C`, `34`-`37`, `31`-`33` `81`/`82` (flash erase/checksum), `30`-`33` with `C0`-`C9` (except exact `31C7000000`/`31C8000000`) or with the immobilizer outputs `A6`/`AB`/`AC` ([`BMT_IMMOBILIZER_OUTPUTS`]), `2E`/`2F`, `3B` other than `3B D2` and `3B D2 x2` (AP1CLE, also with a high nibble), `21 A4` with extra bytes; on every other ECU routines `31`-`33`/`38`-`3A` on `A0 A3`; on a Sirius (`7A` or a Sirius definition) also `31`-`33 A3 ..` (function 0xA3, reset of the backed-up memory) and writes `2E`/`2F`/`3B` to the frames `A0`/`A2`/`AE`/`BD`; definition requests named like immobilizer/transponder/key ([`DenyReason::ImmobilizerNamed`]) | nobody, never |
//!
//! Link frames (`81`, `82`, ISO8 `36`/`3F`) are refused as requests ([`DenyReason::ProtocolReserved`]),
//! not as a safety rule: sent as a request they would desynchronise the session state machine.
//!
//! Approvals exist only with the cargo feature `approved-routines` (off by default; interactive
//! CLI only). Without it, and in AI/MCP/`--json` use, [`assess`] still *prepares* any request:
//! it returns its [`RiskInfo`] (or the refusal) so the caller can print it with the exact command
//! for a human, but nothing except Tier 0 can be executed.
//!
//! Only the init machines and `Session::tester_present`/`Session::stop` send link frames.
//!
//! Every frame the core writes carries a [`Permit`]. Decisions use the exact payload bytes and
//! the target first; the loaded definition's [`DefinitionGuard`] can only add refusals, so a
//! definition that lists a write cannot unlock it.

use alloc::vec::Vec;

use thiserror::Error;

/// BMT (UCH) K-line address.
pub const BMT_ADDRESS: u8 = 0x26;
/// Second address the immobilizer line treats as BMT/UCH/decoder (owner decision 2026-11-22:
/// "address 0x26/0xA6").
pub const BMT_ADDRESS_ALT: u8 = 0xA6;
/// Memory write / routine by address / dynamic definition services (`3D`, `38`, `39`, `3A`,
/// `2C`): on a BMT/UCH/decoder they can write the EEPROM that holds the key codes, so they are
/// forbidden there always (owner decision 2026-11-22). On other ECUs they stay Expert.
pub const BMT_MEMORY_SERVICES: [u8; 5] = [0x3D, 0x38, 0x39, 0x3A, 0x2C];
/// BMT `21A4` "ETATS TRANSPONDEUR": transponder/immobilizer *status* flags. A read, Tier 0
/// under policy v2 (diagnose intermittent start refusals). Only the exact two bytes; anything
/// after `21 A4` is refused as immobilizer related.
pub const BMT_TRANSPONDER_READ: [u8; 2] = [0x21, 0xA4];
/// BMT routine function numbers (`31 [fn] ..`, "LANCEMENT DES ROUTINES") that write, learn or
/// code the immobilizer/transponder (policy v2).
pub const BMT_IMMOBILIZER_FUNCTIONS: [u8; 6] = [0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6];
/// "Anything resembling" them: on the BMT, a `30`/`31`/`32`/`33` payload whose second byte is
/// in this range is refused as [`DenyReason::Immobilizer`], except the two exact
/// remote-learning routines. Capability catalogue 9.1: 0xC0..=0xC8 are all immobilizer
/// functions; 0xC9 added by owner decision 2026-11-22 ("31 C0-C6/C9 family").
pub const BMT_IMMOBILIZER_RANGE: core::ops::RangeInclusive<u8> = 0xC0..=0xC9;
/// BMT `31C7000000` erase all remotes and learn remote 1, `31C8000000` learn remote 2.
pub const BMT_REMOTE_LEARNING: [[u8; 5]; 2] = [
    [0x31, 0xC7, 0x00, 0x00, 0x00],
    [0x31, 0xC8, 0x00, 0x00, 0x00],
];
/// BMT actuator outputs for `31 out param count` (lock motors, window). Capability catalogue
/// 9.1 / 7 #14: 0xA0..=0xB3 are the documented outputs.
pub const BMT_ACTUATOR_OUTPUTS: core::ops::RangeInclusive<u8> = 0xA0..=0xB3;
/// BMT actuator outputs that drive the immobilizer line: 166 `0xA6` "Ligne Transpondeur", 171
/// `0xAB` "Led Verlog", 172 `0xAC` "Ligne ECM" (the coded VERLOG line to the engine ECU), in the
/// "sortie a activer" list of BMT_X65_ph1_p and BMT_SAGEM "COMMANDE DES ACTUATEURS". On the
/// BMT, `30`-`33` with one of them as byte 2 is [`DenyReason::Immobilizer`] (SAFETY.md rule 3,
/// step 7 guard review).
pub const BMT_IMMOBILIZER_OUTPUTS: [u8; 3] = [0xA6, 0xAB, 0xAC];
/// Download / upload / transfer services `34`-`37`: on a BMT/UCH/decoder they can overwrite or
/// dump the EEPROM that holds the key codes, so they are forbidden there (SAFETY.md rule 3,
/// same reason as [`BMT_MEMORY_SERVICES`]). On other ECUs they stay Expert.
pub const BMT_TRANSFER_SERVICES: core::ops::RangeInclusive<u8> = 0x34..=0x37;
/// Flash erase (`81`) and flash checksum (`82`) routine numbers of the KWP reprogramming
/// sequence (`31 81 ..` / `31 82 ..`, also via `32`/`33`): forbidden on a BMT/UCH/decoder
/// (SAFETY.md rule 3: they erase the key-code memory there); Expert with
/// [`Warning::MayBrickEcu`] elsewhere.
pub const FLASH_ROUTINES: [u8; 2] = [0x81, 0x82];
/// BMT `3B D2 index value` configuration write and the read that backs it up
/// ("LECTURE CONFIGURATION" `21 A1`). Capability catalogue 3.4. On the BMT every other `3B`,
/// and every `2E`/`2F`, is refused: the BMT config holds a key option (AP1CLE) and the remote type (TRF), and
/// an undocumented identifier cannot be shown not to touch the immobilizer.
pub const BMT_CONFIG_WRITE_LID: u8 = 0xD2;
pub const BMT_CONFIG_READ: [u8; 2] = [0x21, 0xA1];
/// BMT config index 2 is AP1CLE (key related): refused (rule 3), and so is every index whose
/// low nibble is 2 (`12`, `22`, .. `F2`), in case the firmware masks the index byte (security
/// review H1, NOT VERIFIED on a BMT). Other indices above 0x0F stay Expert.
pub const BMT_CONFIG_IMMOBILIZER_INDEX: u8 = 0x02;
/// BMT config index 3 is TRF, the remote-control type: RF (1) or infrared (0); not
/// immobilizer (Technical Note 77 11 204 878 section 88, TIR panel of the BMT screen). Tier 2
/// with [`Warning::ChangesRemoteType`].
pub const BMT_CONFIG_TRF_INDEX: u8 = 0x03;
/// Highest documented BMT config index; 14 and 15 are reserved (catalogue 3.4) and only
/// reachable as [`Tier::Expert`].
pub const BMT_CONFIG_MAX_INDEX: u8 = 13;
/// Sirius special functions (catalogue 4.3). In the definition "Fonctions particulieres" is
/// `31A000` with the function number at FirstByte 2 (1-based), so a start is `31 fn 00`, a stop
/// ("Arret fonctions" `32A000`) `32 fn 00` and a status ("Status fonctions" `31A001`)
/// `31 fn 01` (final critique F02, checked against the Sirius 32 XML). 0xA0 (injector
/// inhibition, `31 A0 00`) stops the engine: Tier 1 with [`Warning::StopsEngine`]. 0xA3
/// ("Reinit memoire sauvegardee", reset of the backed-up memory) can touch the immobilizer
/// "verrou logiciel" area: `31`/`32`/`33 A3 ..` forbidden on a Sirius. Coordinator decision
/// 2026-11-22.
pub const SIRIUS_INJECTOR_INHIBITION: u8 = 0xA0;
pub const SIRIUS_BACKUP_MEMORY_RESET: u8 = 0xA3;
/// Option byte of a Sirius special function start/stop (`31 fn 00`, `32 fn 00`).
pub const SIRIUS_FUNCTION_START: u8 = 0x00;
/// The byte pair `A0 A3` refused after any routine service on every non-immobilizer ECU: an
/// earlier reading of the catalogue put 0xA3 at byte 3 (`31 A0 A3`); kept refused (harmless).
pub const SIRIUS_LEGACY_RESET_PAIR: [u8; 2] = [0xA0, 0xA3];
/// Sirius local identifiers whose frames carry immobilizer / "verrou logiciel" fields
/// (catalogue 4.4: Trame 01 `A0`, Trame 03 `A2`, Trame 15 `AE` verrou type, Trame 30 `BD`).
/// They are `21` frame identifiers. Reading them is Tier 0. On a Sirius ([`is_sirius_ecu`])
/// any write by local identifier (`2E`/`2F`/`3B`) of one of them is refused always (owner
/// decision 2026-11-22). Output control `30` and routines `31`-`33`/`38`-`3A` with the same
/// byte value are other number spaces (`30 A0` fast radiator fan, `31 A2 00 00 00` "SRBLI
/// start test sav") and keep their normal tiers (security review M1). Separately, any routine
/// service on `A0 A3` (reset of the backed-up memory) is refused on every non-immobilizer ECU.
pub const SIRIUS_IMMOBILIZER_LIDS: [u8; 4] = [0xA0, 0xA2, 0xAE, 0xBD];
/// Sirius 32 / 34 injection K-line address (`<Function Address="122">`).
pub const SIRIUS_ADDRESS: u8 = 0x7A;
/// Routine services: start/stop/results by local id (`31`-`33`) and by address (`38`-`3A`).
pub const ROUTINE_SERVICES: [u8; 6] = [0x31, 0x32, 0x33, 0x38, 0x39, 0x3A];
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

/// What the loaded ECU definition says the ECU is, for the immobilizer line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EcuClass {
    /// Not known to be an immobilizer ECU by its definition (the address rules still apply).
    #[default]
    Generic,
    /// A BMT/UCH/immobilizer-decoder definition: the BMT rules apply at any address.
    Immobilizer,
    /// A Sirius injection definition: the Sirius immobilizer-LID rules apply at any address.
    Sirius,
}

/// What the loaded definition adds to the byte policy (SAFETY.md rule 3 for ECUs the address
/// rules do not know): its [`EcuClass`] and the requests whose names look immobilizer /
/// transponder / key related. The byte rules stay primary: the guard only adds refusals, and
/// the names only apply to Tier 1-3. Build one from an `EcuDef` with
/// `kline_core::spec::definition_guard` (feature `spec`); the default guard adds nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefinitionGuard {
    pub class: EcuClass,
    /// `(fixed sent-byte prefix, request name)` of each flagged request.
    flagged: Vec<(Vec<u8>, alloc::string::String)>,
}

impl DefinitionGuard {
    pub fn new(class: EcuClass) -> DefinitionGuard {
        DefinitionGuard {
            class,
            flagged: Vec::new(),
        }
    }

    /// Flag the request `name` that sends `prefix` (its fixed bytes) as immobilizer related.
    /// A prefix shorter than two bytes is ignored: a service byte alone would refuse every
    /// request of that service (e.g. the BMT remote learning `31C7000000`) by accident of the
    /// XML.
    pub fn flag(&mut self, prefix: Vec<u8>, name: alloc::string::String) {
        if prefix.len() >= 2 {
            self.flagged.push((prefix, name));
        }
    }

    /// Add `other`'s refusals: the stronger class (immobilizer over Sirius over generic) and
    /// all its flagged requests. For several candidate definitions of one ECU (security
    /// review L4): the guard of the union only adds refusals.
    pub fn merge(&mut self, other: DefinitionGuard) {
        let rank = |c: EcuClass| match c {
            EcuClass::Generic => 0,
            EcuClass::Sirius => 1,
            EcuClass::Immobilizer => 2,
        };
        if rank(other.class) > rank(self.class) {
            self.class = other.class;
        }
        for f in other.flagged {
            if !self.flagged.contains(&f) {
                self.flagged.push(f);
            }
        }
    }

    /// Names of the flagged requests `payload` would send (their prefix starts it). The two
    /// owner-approved BMT remote-learning routines are exempt on an immobilizer ECU: their BMT
    /// names contain "CLE" (remote key).
    pub fn immobilizer_names(&self, target: &Target, payload: &[u8]) -> Vec<alloc::string::String> {
        if is_immobilizer_ecu(self, target) && is_remote_learning(payload) {
            return Vec::new();
        }
        self.flagged
            .iter()
            .filter(|(p, _)| payload.starts_with(p))
            .map(|(_, n)| n.clone())
            .collect()
    }
}

/// True when the BMT rules apply: address 0x26 or 0xA6, or a BMT/UCH/decoder definition.
pub fn is_immobilizer_ecu(guard: &DefinitionGuard, target: &Target) -> bool {
    matches!(target.ecu_address, BMT_ADDRESS | BMT_ADDRESS_ALT)
        || guard.class == EcuClass::Immobilizer
}

/// True when the Sirius immobilizer rules apply (LID writes, `31`-`33 A3 ..`): address 0x7A or
/// a Sirius definition (and not an immobilizer ECU). `.. A0 A3` routines are refused on every
/// non-immobilizer ECU.
pub fn is_sirius_ecu(guard: &DefinitionGuard, target: &Target) -> bool {
    !is_immobilizer_ecu(guard, target)
        && (target.ecu_address == SIRIUS_ADDRESS || guard.class == EcuClass::Sirius)
}

const NO_GUARD: DefinitionGuard = DefinitionGuard {
    class: EcuClass::Generic,
    flagged: Vec::new(),
};

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
    /// Stops the engine or prevents it from running (Sirius `31 A0 00`; undo with `32 A0 00`).
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
    /// Can leave the ECU unable to run or to be diagnosed again (flash/memory/routine by
    /// address; an interrupted transfer).
    MayBrickEcu,
    /// Can leave the ECU in a programming or other non-default session (normal program not
    /// running) until it times out or the ignition is cycled.
    MayLeaveProgrammingSession,
    /// Security access: unlocks protected services; failed attempts can lock the ECU out for a
    /// delay.
    SecurityAccess,
    /// Changes the remote-control type (BMT config TRF: RF or infrared); a remote of the other
    /// type stops working.
    ChangesRemoteType,
    /// Airbag / restraint ECU (safety system): a wrong write or test can disable the airbags
    /// or pretensioners or light the airbag lamp.
    SafetySystem,
}

/// A short list kept inline (no allocation, `Copy`), so a risk can be extended (airbag ECU)
/// without losing the request's own entries. Holds at most `N` items.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct StaticList<T: Copy + PartialEq + 'static, const N: usize> {
    items: [Option<T>; N],
}

impl<T: Copy + PartialEq + 'static, const N: usize> StaticList<T, N> {
    /// The items of `s` in order. Panics if `s` has more than `N` items (a programming error
    /// in the risk table, covered by the tests).
    pub fn from_slice(s: &[T]) -> Self {
        let mut l = StaticList { items: [None; N] };
        for x in s {
            l.push(*x);
        }
        l
    }

    /// Append `x` unless it is already in the list.
    pub fn push(&mut self, x: T) {
        if self.contains(&x) {
            return;
        }
        let slot = self
            .items
            .iter_mut()
            .find(|i| i.is_none())
            .expect("StaticList capacity");
        *slot = Some(x);
    }

    /// `x` first, then the items (without a duplicate of `x`).
    pub fn with_first(&self, x: T) -> Self {
        let mut l = StaticList { items: [None; N] };
        l.push(x);
        for i in self.iter() {
            l.push(*i);
        }
        l
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.items.iter().filter_map(Option::as_ref)
    }

    pub fn contains(&self, x: &T) -> bool {
        self.iter().any(|i| i == x)
    }

    pub fn len(&self) -> usize {
        self.iter().count()
    }

    pub fn is_empty(&self) -> bool {
        self.items[0].is_none()
    }
}

impl<T: Copy + PartialEq + core::fmt::Debug + 'static, const N: usize> core::fmt::Debug
    for StaticList<T, N>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T: Copy + PartialEq + 'static, const N: usize> PartialEq<[T]> for StaticList<T, N> {
    fn eq(&self, other: &[T]) -> bool {
        self.len() == other.len() && self.iter().zip(other).all(|(a, b)| a == b)
    }
}

impl<T: Copy + PartialEq + 'static, const N: usize, const M: usize> PartialEq<&[T; M]>
    for StaticList<T, N>
{
    fn eq(&self, other: &&[T; M]) -> bool {
        *self == other[..]
    }
}

impl<'a, T: Copy + PartialEq + 'static, const N: usize> IntoIterator for &'a StaticList<T, N> {
    type Item = &'a T;
    type IntoIter =
        core::iter::FilterMap<core::slice::Iter<'a, Option<T>>, fn(&'a Option<T>) -> Option<&'a T>>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter().filter_map(Option::as_ref)
    }
}

/// Warnings of a [`RiskInfo`].
pub type Warnings = StaticList<Warning, 8>;
/// Prerequisites of a [`RiskInfo`].
pub type Prerequisites = StaticList<&'static str, 8>;

/// What a non-read request does, shown to the human before approval and quoted by the
/// approval (sources: ECU definitions and the capability catalogue).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiskInfo {
    pub tier: Tier,
    pub what_it_changes: &'static str,
    pub what_can_go_wrong: &'static str,
    /// Extra risk from the kind of ECU, shown with `what_can_go_wrong`: the airbag text on the
    /// airbag ECU ([`AIRBAG_ADDRESS`]), else `None`.
    pub safety_system: Option<&'static str>,
    pub reversible: bool,
    pub prerequisites: Prerequisites,
    pub warnings: Warnings,
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
    /// A Tier 1-3 request of the loaded definition whose name looks immobilizer / transponder
    /// / key related ([`DefinitionGuard`]). Forbidden always, also in expert mode.
    ImmobilizerNamed { names: Vec<alloc::string::String> },
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
                | DenyReason::ImmobilizerNamed { .. }
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

/// The refusals no approval can lift (also in expert mode), or `None`. Byte and address rules
/// only; [`forbidden_in`] adds the loaded definition's [`DefinitionGuard`].
pub fn forbidden(target: &Target, payload: &[u8]) -> Option<DenyReason> {
    forbidden_in(&NO_GUARD, target, payload)
}

/// [`forbidden`] with the loaded definition's guard: its class (BMT rules at any address) and,
/// for requests that are not Tier 0, its immobilizer-named requests.
pub fn forbidden_in(
    guard: &DefinitionGuard,
    target: &Target,
    payload: &[u8],
) -> Option<DenyReason> {
    let r = forbidden_bytes(guard, target, payload);
    if r.is_some() {
        return r;
    }
    let names = guard.immobilizer_names(target, payload);
    if !names.is_empty() && classify_bytes(guard, target, payload) != Ok(Tier::Read) {
        return Some(DenyReason::ImmobilizerNamed { names });
    }
    None
}

fn forbidden_bytes(guard: &DefinitionGuard, target: &Target, payload: &[u8]) -> Option<DenyReason> {
    let Some(&service) = payload.first() else {
        return Some(DenyReason::Empty);
    };
    let p1 = payload.get(1).copied();
    let bmt = is_immobilizer_ecu(guard, target);
    // Rule 3 first, on any protocol (security review M2 F3b: no protocol confusion).
    if bmt {
        let immo = match service {
            0x30..=0x33 => {
                (p1.is_some_and(|b| BMT_IMMOBILIZER_RANGE.contains(&b))
                    && !is_remote_learning(payload))
                    || p1.is_some_and(|b| BMT_IMMOBILIZER_OUTPUTS.contains(&b))
                    || (service != 0x30 && p1.is_some_and(|b| FLASH_ROUTINES.contains(&b)))
            }
            s if BMT_MEMORY_SERVICES.contains(&s) => true,
            s if BMT_TRANSFER_SERVICES.contains(&s) => true,
            0x2E | 0x2F => true,
            // Index 2 is AP1CLE; `x2` too, in case the firmware masks the index to its low
            // nibble ("Choix fonction" has 16 values: security review H1).
            0x3B => {
                p1 != Some(BMT_CONFIG_WRITE_LID)
                    || payload
                        .get(2)
                        .is_some_and(|i| i & 0x0F == BMT_CONFIG_IMMOBILIZER_INDEX)
            }
            0x21 => payload.len() > 2 && payload[..2] == BMT_TRANSPONDER_READ,
            _ => false,
        };
        if immo {
            return Some(DenyReason::Immobilizer);
        }
    }
    // Sirius rules on any protocol too (re-review M2 finding 1: a Target { Iso8, 0x7A } must not
    // turn `31 A3 00`, `31 A0 A3` or a write to an immobilizer frame into an expert request).
    if !bmt {
        let routine = ROUTINE_SERVICES.contains(&service);
        let legacy_reset =
            routine && payload.len() >= 3 && payload[1..3] == SIRIUS_LEGACY_RESET_PAIR;
        // The real layout: `31`/`32`/`33 A3 ..` is the backed-up memory reset on a Sirius
        // (final critique F02).
        let reset = legacy_reset
            || (matches!(service, 0x31..=0x33)
                && p1 == Some(SIRIUS_BACKUP_MEMORY_RESET)
                && is_sirius_ecu(guard, target));
        // A0/A2/AE/BD are `21` frame identifiers: only a write by local identifier addresses
        // the same frame, and only on a Sirius do those numbers mean the immobilizer frames.
        // Output control `30` and routines use other number spaces (security review M1).
        let immo_write = matches!(service, 0x2E | 0x2F | 0x3B)
            && p1.is_some_and(|b| SIRIUS_IMMOBILIZER_LIDS.contains(&b))
            && is_sirius_ecu(guard, target);
        if reset || immo_write {
            return Some(DenyReason::Immobilizer);
        }
    }
    match (target.protocol, service) {
        (Protocol::Kwp, 0x81 | 0x82) | (Protocol::Iso8, 0x36 | 0x3F) => {
            Some(DenyReason::ProtocolReserved { service })
        }
        // Owner decision 2026-11-22 (final): security access `27`, download/upload/transfer
        // `34`-`37` and other sessions are Expert (with specific warnings), not forbidden.
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
/// injector inhibition, the only `32` in the definitions), `31 A0 00`; BMT `31C7000000`, `31C8000000`, actuator `31 out param count` with
/// `out` in 0xA0..=0xB3. KWP, Tier 2: BMT `3B D2 index 00|01` (index 0..=13, not 2), other
/// ECUs `3B lid` + 1-4 bytes.
pub fn classify(target: &Target, payload: &[u8]) -> Result<Tier, Denied> {
    classify_in(&NO_GUARD, target, payload)
}

/// [`classify`] with the loaded definition's [`DefinitionGuard`].
pub fn classify_in(
    guard: &DefinitionGuard,
    target: &Target,
    payload: &[u8],
) -> Result<Tier, Denied> {
    if let Some(reason) = forbidden_in(guard, target, payload) {
        return Err(Denied::new(payload, reason));
    }
    classify_bytes(guard, target, payload)
}

/// Documented tier on bytes and class only (no name guard).
fn classify_bytes(
    guard: &DefinitionGuard,
    target: &Target,
    payload: &[u8],
) -> Result<Tier, Denied> {
    if let Some(reason) = forbidden_bytes(guard, target, payload) {
        return Err(Denied::new(payload, reason));
    }
    let service = payload[0];
    let len = payload.len();
    let p1 = payload.get(1).copied();
    let bmt = is_immobilizer_ecu(guard, target);
    use Protocol::{Iso8, Kwp};
    use Tier::{ConfigWrite, Read, Service};
    let tier = match (target.protocol, service) {
        // TesterPresent, optional responseRequired byte.
        (Kwp, 0x3E) => (len <= 2).then_some(Read),
        // StartDiagnosticSession: only 10 81 / 10 A0 are Tier 0; any other session is Expert.
        (Kwp, 0x10) => {
            (len == 2 && matches!(p1, Some(DEFAULT_SESSION | RENAULT_DIAG_SESSION))).then_some(Read)
        }
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
        (Kwp, 0x32) if !bmt => (payload
            == [0x32, SIRIUS_INJECTOR_INHIBITION, SIRIUS_FUNCTION_START])
        .then_some(Service),
        // Tier 1, BMT: remote learning, and the actuator command `31 out param count`
        // ("COMMANDE DES ACTUATEURS" 31000000).
        (Kwp, 0x31) if bmt => (is_remote_learning(payload)
            || (len == 4 && p1.is_some_and(|b| BMT_ACTUATOR_OUTPUTS.contains(&b))))
        .then_some(Service),
        // Tier 1, other KWP ECUs (Sirius): only injector inhibition `31 A0 00`.
        (Kwp, 0x31) => (payload == [0x31, SIRIUS_INJECTOR_INHIBITION, SIRIUS_FUNCTION_START])
            .then_some(Service),
        // Tier 2, BMT: `3B D2 index value`, one boolean option (catalogue 3.4). TRF with any
        // value is still a remote-type change (security review L5).
        (Kwp, 0x3B) if bmt => (len == 4
            && payload[2] <= BMT_CONFIG_MAX_INDEX
            && (payload[3] <= 1 || payload[2] == BMT_CONFIG_TRF_INDEX))
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
    tier_of_in(&NO_GUARD, target, payload)
}

/// [`tier_of`] with the loaded definition's [`DefinitionGuard`].
pub fn tier_of_in(
    guard: &DefinitionGuard,
    target: &Target,
    payload: &[u8],
) -> Result<Tier, Denied> {
    match classify_in(guard, target, payload) {
        Ok(t) => Ok(t),
        Err(d) if d.reason.is_forbidden() => Err(d),
        Err(_) => Ok(Tier::Expert),
    }
}

/// Prepare `payload` without sending anything: its [`RiskInfo`], or why it can never be sent.
/// This is what AI/MCP/`--json` callers use for non-read requests: print the risk and the exact
/// command for a human to run in the interactive CLI.
pub fn assess(target: &Target, payload: &[u8]) -> Result<RiskInfo, Denied> {
    assess_in(&NO_GUARD, target, payload)
}

/// [`assess`] with the loaded definition's [`DefinitionGuard`].
pub fn assess_in(
    guard: &DefinitionGuard,
    target: &Target,
    payload: &[u8],
) -> Result<RiskInfo, Denied> {
    let tier = tier_of_in(guard, target, payload)?;
    let info = risk_info(is_immobilizer_ecu(guard, target), payload, tier);
    Ok(
        if target.ecu_address == AIRBAG_ADDRESS && tier != Tier::Read {
            airbag_risk(info)
        } else {
            info
        },
    )
}

/// Airbag ECU address in the X65 addressing (`vehicles/x65/addressing.xml`: 0x2C "Airbag").
/// Not forbidden (owner decision 2026-11-22: only the immobilizer is), but every non-read is
/// marked as a safety-system change.
pub const AIRBAG_ADDRESS: u8 = 0x2C;

/// The request's own risk plus the airbag text, `SafetySystem` (first) and the airbag
/// prerequisites (code review M2 / security review L1: nothing of the request's own risk is
/// dropped). Irreversible: an airbag change must be checked on the car.
fn airbag_risk(info: RiskInfo) -> RiskInfo {
    const WHAT: &str = "AIRBAG ECU (safety system): a wrong write, test or clear can disable the airbags or seat-belt pretensioners, light the airbag lamp, or on an output test fire a squib";
    const PRE: [&str; 3] = [
        "nobody in the car, ignition on, engine off",
        "read and save the airbag fault frames first",
        "after any change, check that the airbag lamp goes out at the next ignition on",
    ];
    let mut prerequisites = info.prerequisites;
    for p in PRE {
        prerequisites.push(p);
    }
    RiskInfo {
        safety_system: Some(WHAT),
        reversible: false,
        prerequisites,
        warnings: info.warnings.with_first(Warning::SafetySystem),
        ..info
    }
}

const NONE: &[&str] = &[];

fn risk_info(bmt: bool, payload: &[u8], tier: Tier) -> RiskInfo {
    let r = |what_it_changes,
             what_can_go_wrong,
             reversible,
             prerequisites: &[&'static str],
             warnings: &[Warning]| RiskInfo {
        tier,
        what_it_changes,
        what_can_go_wrong,
        safety_system: None,
        reversible,
        prerequisites: Prerequisites::from_slice(prerequisites),
        warnings: Warnings::from_slice(warnings),
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
        (Tier::Service, [0x32, SIRIUS_INJECTOR_INHIBITION, SIRIUS_FUNCTION_START]) => r(
            "ends the injector inhibition started by 31 A0 00 (the engine may run again)",
            "the engine can start running if it is being cranked",
            true,
            &["vehicle stationary"],
            &[],
        ),
        (Tier::Service, [0x31, SIRIUS_INJECTOR_INHIBITION, SIRIUS_FUNCTION_START]) if !bmt => r(
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
        (Tier::ConfigWrite, [0x3B, BMT_CONFIG_WRITE_LID, BMT_CONFIG_TRF_INDEX, 0x00]) if bmt => r(
            "switches the BMT to an INFRARED remote (config TRF = 0)",
            "an RF remote stops working; the type may lock once a remote is learned (Technical Note: change it only after clearing the remotes)",
            true,
            &[
                "21 80 read and the definition chosen from it",
                "21 A1 backup read in this session",
                "read back 21 A1 after the write",
            ],
            &[Warning::ChangesRemoteType, Warning::EepromWrite],
        ),
        (Tier::ConfigWrite, [0x3B, BMT_CONFIG_WRITE_LID, BMT_CONFIG_TRF_INDEX, 0x01]) if bmt => r(
            "switches the BMT to an RF remote (config TRF = 1)",
            "an infrared remote stops working; the type may lock once a remote is learned (Technical Note: change it only after clearing the remotes)",
            true,
            &[
                "21 80 read and the definition chosen from it",
                "21 A1 backup read in this session",
                "read back 21 A1 after the write",
            ],
            &[Warning::ChangesRemoteType, Warning::EepromWrite],
        ),
        (Tier::ConfigWrite, [0x3B, BMT_CONFIG_WRITE_LID, BMT_CONFIG_TRF_INDEX, _]) if bmt => r(
            "writes an unknown TRF value (remote type, defined values 0 = infrared, 1 = RF) to the BMT config",
            "unknown TRF value: the BMT may take it as RF or infrared or reject it; a remote of the other type stops working",
            true,
            &[
                "21 80 read and the definition chosen from it",
                "21 A1 backup read in this session",
                "read back 21 A1 after the write",
            ],
            &[Warning::ChangesRemoteType, Warning::EepromWrite],
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
        (Tier::Expert, [0x27, ..]) => r(
            "security access: unlocks protected services (programming, protected writes)",
            "wrong keys count as failed attempts and can lock the ECU out for a delay; an unlocked ECU accepts writes that can brick it",
            false,
            &["you know the seed/key procedure of this exact ECU"],
            &[Warning::SecurityAccess, Warning::MayBrickEcu, Warning::UnknownEffect],
        ),
        (Tier::Expert, [0x34..=0x37, ..]) => r(
            "download/upload/transfer: ECU (re)programming or memory dump",
            "MAY BRICK THE ECU: an interrupted or wrong transfer can leave it unable to run or to be reflashed over K-line; may leave the ECU in a programming session",
            false,
            &["stable supply voltage", "the exact image and procedure for this ECU", "a recovery path (bench flasher)"],
            &[Warning::MayBrickEcu, Warning::MayLeaveProgrammingSession, Warning::EepromWrite, Warning::UnknownEffect],
        ),
        (Tier::Expert, [0x31..=0x33, 0x81 | 0x82, ..]) => r(
            "erases (81) or checks (82) ECU flash: a step of KWP reprogramming",
            "MAY BRICK THE ECU: an erased flash leaves the ECU unable to run until it is reprogrammed; may leave the ECU in a programming session",
            false,
            &["stable supply voltage", "the exact image and procedure for this ECU", "a recovery path (bench flasher)"],
            &[Warning::MayBrickEcu, Warning::MayLeaveProgrammingSession, Warning::UnknownEffect],
        ),
        (Tier::Expert, [0x10, ..]) => r(
            "switches the ECU to another diagnostic session (e.g. programming / extended)",
            "MAY LEAVE THE ECU IN A PROGRAMMING SESSION: the engine/body program may stop running until the session times out or the ignition is cycled; some sessions enable flashing",
            true,
            &["engine off, vehicle stationary", "know how to return to 10 81 or cycle the ignition"],
            &[Warning::MayLeaveProgrammingSession, Warning::UnknownEffect],
        ),
        (Tier::Expert, [0x3D, ..]) => r(
            "writes ECU memory at a raw address (RAM, EEPROM or calibration)",
            "MAY BRICK THE ECU: a wrong address or value can corrupt calibration/EEPROM and stop it running",
            false,
            &["a backup read (23) of the same range", "the memory map of this exact ECU"],
            &[Warning::MayBrickEcu, Warning::EepromWrite, Warning::UnknownEffect],
        ),
        (Tier::Expert, [0x38..=0x3A, ..]) => r(
            "starts/stops/queries a routine at a raw memory address (runs ECU code)",
            "MAY BRICK THE ECU: running code at a wrong address can crash it, erase memory or leave it in a programming state",
            false,
            &["the memory map of this exact ECU"],
            &[Warning::MayBrickEcu, Warning::MayLeaveProgrammingSession, Warning::UnknownEffect],
        ),
        (Tier::Expert, [0x2C, ..]) => r(
            "defines a dynamic local identifier (changes what a 21 read returns)",
            "later reads of that identifier return other data until the ECU is reset; unknown side effects",
            true,
            NONE,
            &[Warning::UnknownEffect],
        ),
        (Tier::Expert, [0x23, ..]) => r(
            "reads ECU memory at a raw address (no write intended)",
            "unknown on this ECU: some addresses are I/O registers where a read has side effects; the dump may contain protected data",
            true,
            NONE,
            &[Warning::UnknownEffect],
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
    check_in(&NO_GUARD, target, payload)
}

/// [`check`] with the loaded definition's [`DefinitionGuard`].
pub fn check_in(
    guard: &DefinitionGuard,
    target: &Target,
    payload: &[u8],
) -> Result<Permit, Denied> {
    match classify_in(guard, target, payload)? {
        Tier::Read => Ok(Permit::new(payload, *target, Tier::Read)),
        tier => Err(Denied::new(payload, DenyReason::ApprovalRequired { tier })),
    }
}

/// The read whose positive reply must exist in the same session before a Tier 2 `payload` is
/// approved (mandatory pre-read backup): `21 A1` for BMT `3B D2 ..`, else `21 lid` for
/// `3B lid ..`.
pub fn config_backup_read(target: &Target, payload: &[u8]) -> Option<[u8; 2]> {
    config_backup_read_in(&NO_GUARD, target, payload)
}

/// [`config_backup_read`] with the loaded definition's [`DefinitionGuard`].
pub fn config_backup_read_in(
    guard: &DefinitionGuard,
    target: &Target,
    payload: &[u8],
) -> Option<[u8; 2]> {
    match payload {
        [0x3B, BMT_CONFIG_WRITE_LID, ..] if is_immobilizer_ecu(guard, target) => {
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
/// AP1CLE, code cle / key code. `wegfahr` (German Wegfahrsperre) is ours; `codemanuel` (BMT
/// "Emission du CODE MANUEL") and `schlussel`/`schluessel` (German key) from security review
/// L3. Deliberately broad:
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
    "codemanuel",
    "schlussel",
    "schluessel",
];
/// Whole words (lowercase, accents folded) with the same meaning: `cle`/`cles`/`clef`/`clefs`
/// (French key, as in "APPRENT CLE 2 TEL", "App seconde cléf TIR") and `key`/`keys`.
pub const IMMOBILIZER_WORDS: &[&str] = &["cle", "cles", "clef", "clefs", "key", "keys"];

/// Fold one character for keyword matching: lowercase ASCII, accents removed; `None` for
/// anything that is not a letter or digit.
pub fn fold_char(c: char) -> Option<char> {
    fold(c)
}

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
