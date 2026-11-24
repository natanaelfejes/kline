//! A simulated ECU driven by a kline-spec [`EcuDef`] and a [`Scenario`].

use kline_spec::{EcuDef, KProtocol};

use crate::bus::{Out, Responder, Wake};
use crate::classify::classify;
use crate::frame::{encode_reply, Assembler, Feed, Parsed, WireFormat};
use crate::journal::{Journal, ViolationKind, WriteRecord};
use crate::scenario::{positive_response_with, request_matches, Scenario};
use crate::timing::{EcuTiming, Tolerance, BYTE_MS};
use crate::SimError;

/// XOR the Multipoint tester applies to the 5-baud address. RE: KWP2000Diag4 (docs/protocol.md).
pub const MULTIPOINT_XOR: u8 = 0x80;
/// Sync byte after a 5-baud address. ISO 9141-2.
pub const SYNC: u8 = 0x55;

/// NRC serviceNotSupported. ISO 14230-3.
pub const NRC_SERVICE_NOT_SUPPORTED: u8 = 0x11;
/// NRC subFunctionNotSupported-invalidFormat. ISO 14230-3.
pub const NRC_SUB_FUNCTION_NOT_SUPPORTED: u8 = 0x12;
/// NRC generalReject (the simulator could not build a reply). ISO 14230-3.
pub const NRC_GENERAL_REJECT: u8 = 0x10;
/// NRC requestCorrectlyReceived-ResponsePending. ISO 14230-3.
pub const NRC_RESPONSE_PENDING: u8 = 0x78;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcuProtocol {
    /// KWP2000 after a 5-baud init with the ~KW2 / ~address handshake (Sirius 32, BMT).
    Kwp5Baud,
    /// ISO 9141 5-baud init without handshake, ISO8 framing (Safir).
    Iso8,
    /// KWP2000 fast init only.
    KwpFast,
}

/// Which 5-baud address byte wakes the ECU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressAccept {
    /// Only the plain address (0x7A / 0x26): what `InitVariant::SingleEcu` sends.
    Plain,
    /// Only `address ^ 0x80` (0xFA / 0xA6): what `InitVariant::MultiEcu` sends.
    Xor80,
    Both,
}

/// How the ECU behaves on the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EcuProfile {
    pub name: String,
    pub address: u8,
    pub protocol: EcuProtocol,
    /// Key bytes after 0x55 (5-baud).
    pub keywords: (u8, u8),
    pub accept: AddressAccept,
    /// Also wake on a fast-init pulse + StartCommunication (KWP only).
    pub accept_fast_init: bool,
    /// Key bytes in the `C1 KW1 KW2` StartCommunication reply. NOT VERIFIED for the car.
    pub fast_init_keywords: (u8, u8),
    /// Reply framing: short lengths in the KWP format byte.
    pub length_in_format: bool,
    /// Payload the ECU answers ISO8 TesterPresent `36` with (None: silent). The Bosch 5.3 ABS
    /// answers `36` (car capture 2026-11-23); Safir is unknown.
    pub iso8_tester_present_reply: Option<Vec<u8>>,
    /// Payload the ECU answers ISO8 Stop `3F` with before it sleeps (None: silent). The
    /// Bosch 5.3 ABS answers `3F` (car capture 2026-11-23); Safir is unknown.
    pub iso8_stop_reply: Option<Vec<u8>>,
    /// Added to the request SID to make the positive response SID: 0x40 (KWP, assumed for
    /// Safir), 0 for the Bosch 5.3 ABS (`51` -> `51 ..`, car capture 2026-11-23).
    pub positive_sid_offset: u8,
    pub timing: EcuTiming,
    pub tolerance: Tolerance,
}

/// Positive response SID = request SID + 0x40. ISO 14230-3.
pub const POSITIVE_SID_OFFSET: u8 = 0x40;
/// Bosch 5.3 ABS address in the X65 addressing.
pub const BOSCH_ABS_ADDRESS: u8 = 0x01;
/// Bosch 5.3 ABS key bytes: car 2026-11-23 and X24_ABS_Bosch5.3.xml KW1 220 / KW2 131.
pub const BOSCH_ABS_KEYWORDS: (u8, u8) = (0xDC, 0x83);
/// Strict P3min of the ABS profile: ISO 9141-2 P3min (55 ms). Not the X24 definition's
/// `ReplyToRequest@Delay` (300): that is the tester's own pause, and on the car the ABS
/// accepted the first request 200 ms after its key bytes (FirstRequest 100 + flush), so a
/// 300 ms ECU-side minimum would be wrong.
pub const BOSCH_ABS_P3_MIN_MS: u64 = 55;

/// `ReplyToRequest@Delay` of the car's ECUs (XML <K> of SIRIUS32 and BMT_X65), used as the
/// strict P3min of the named profiles.
pub const CAR_REPLY_TO_REQUEST_MS: u64 = 5;

impl EcuProfile {
    /// Siemens Sirius 32 at 0x7A: KWP 5-baud, `55 D0 8F`, ~KW2 handshake (docs/protocol.md).
    pub fn sirius32() -> EcuProfile {
        EcuProfile {
            name: "Sirius 32".into(),
            address: 0x7A,
            protocol: EcuProtocol::Kwp5Baud,
            keywords: (0xD0, 0x8F),
            accept: AddressAccept::Both,
            accept_fast_init: false,
            fast_init_keywords: (0xD0, 0x8F),
            // KW1 0xD0 keeps the length in the format byte (docs/protocol.md "Framing").
            length_in_format: true,
            iso8_tester_present_reply: None,
            iso8_stop_reply: None,
            positive_sid_offset: POSITIVE_SID_OFFSET,
            timing: EcuTiming::default(),
            tolerance: Tolerance::strict(CAR_REPLY_TO_REQUEST_MS),
        }
    }

    /// BMT / UCH at 0x26: KWP 5-baud multipoint, `55 D0 8F`.
    pub fn bmt() -> EcuProfile {
        EcuProfile {
            name: "BMT".into(),
            address: 0x26,
            ..EcuProfile::sirius32()
        }
    }

    /// Sagem Safir 2 at 0x7A: ISO8, `55 C2 85`, no handshake (docs/protocol.md).
    pub fn safir_iso8() -> EcuProfile {
        EcuProfile {
            name: "Safir 2".into(),
            address: 0x7A,
            protocol: EcuProtocol::Iso8,
            keywords: (0xC2, 0x85),
            accept: AddressAccept::Plain,
            accept_fast_init: false,
            fast_init_keywords: (0xC2, 0x85),
            length_in_format: false,
            iso8_tester_present_reply: None,
            iso8_stop_reply: None,
            positive_sid_offset: POSITIVE_SID_OFFSET,
            timing: EcuTiming::default(),
            tolerance: Tolerance::strict(CAR_REPLY_TO_REQUEST_MS),
        }
    }

    /// Bosch 5.3 ABS at 0x01 (this car): ISO8, `55 DC 83`, no handshake; answers `36` with
    /// `36`, `3F` with `3F` (then sleeps) and a request with its own SID (`51` -> `51 ..`).
    /// All from the car capture of 2026-11-23 (docs/protocol.md "Bosch 5.3 ABS"). P3min:
    /// [`BOSCH_ABS_P3_MIN_MS`].
    /// The Bosch 5.3 ABS profile; its session lasts through the 7000 ms keep-alive of the
    /// definition (X24_ABS_Bosch5.3 `TesterPresent@Delay`), as on the car.
    pub fn bosch_abs53() -> EcuProfile {
        EcuProfile {
            name: "Bosch ABS 5.3".into(),
            address: BOSCH_ABS_ADDRESS,
            keywords: BOSCH_ABS_KEYWORDS,
            fast_init_keywords: BOSCH_ABS_KEYWORDS,
            iso8_tester_present_reply: Some(vec![0x36]),
            iso8_stop_reply: Some(vec![0x3F]),
            positive_sid_offset: 0,
            tolerance: Tolerance::strict(BOSCH_ABS_P3_MIN_MS),
            timing: EcuTiming {
                session_timeout_ms: iso8_session_timeout_ms(BOSCH_ABS_TESTER_PRESENT_MS),
                ..EcuTiming::default()
            },
            ..EcuProfile::safir_iso8()
        }
    }

    /// Whether this is the Bosch 5.3 ABS behaviour (ISO8 at 0x01 with key bytes DC 83).
    pub fn is_bosch_abs(&self) -> bool {
        self.protocol == EcuProtocol::Iso8
            && self.address == BOSCH_ABS_ADDRESS
            && self.keywords == BOSCH_ABS_KEYWORDS
    }

    /// Profile from a definition's `<Function Address>` and `<K>` (read here, not through
    /// kline-core's mapping). Strict tolerance with P3min = `ReplyToRequest@Delay` (0 if absent).
    pub fn from_def(def: &EcuDef) -> Result<EcuProfile, SimError> {
        let address = def
            .function
            .as_ref()
            .map(|f| f.address)
            .ok_or(SimError::NoAddress)?;
        let k = def.k.as_ref().ok_or(SimError::NoKLine)?;
        let protocol = match k.protocol() {
            KProtocol::Iso8 => EcuProtocol::Iso8,
            KProtocol::KwpSlowInit => EcuProtocol::Kwp5Baud,
            KProtocol::KwpFastInit => EcuProtocol::KwpFast,
            KProtocol::Unknown => return Err(SimError::NoKLine),
        };
        let keywords = k.key_words().ok_or(SimError::NoKeywords)?;
        let p3 = k.reply_to_request_delay.map_or(0, u64::from);
        let bosch_abs = protocol == EcuProtocol::Iso8
            && address == BOSCH_ABS_ADDRESS
            && keywords == BOSCH_ABS_KEYWORDS;
        Ok(EcuProfile {
            name: def.name.clone(),
            address,
            protocol,
            keywords,
            accept: if protocol == EcuProtocol::Iso8 {
                AddressAccept::Plain
            } else {
                AddressAccept::Both
            },
            accept_fast_init: protocol == EcuProtocol::KwpFast,
            fast_init_keywords: keywords,
            length_in_format: keywords.0 == 0xD0 || keywords.0 & 1 != 0,
            iso8_tester_present_reply: bosch_abs.then(|| vec![0x36]),
            iso8_stop_reply: bosch_abs.then(|| vec![0x3F]),
            positive_sid_offset: if bosch_abs { 0 } else { POSITIVE_SID_OFFSET },
            timing: EcuTiming {
                session_timeout_ms: match (protocol, k.tester_present_delay) {
                    (EcuProtocol::Iso8, Some(tp)) => iso8_session_timeout_ms(u64::from(tp)),
                    _ => EcuTiming::default().session_timeout_ms,
                },
                ..EcuTiming::default()
            },
            tolerance: Tolerance::strict(if bosch_abs { BOSCH_ABS_P3_MIN_MS } else { p3 }),
        })
    }

    pub fn wire_format(&self) -> WireFormat {
        match self.protocol {
            EcuProtocol::Iso8 => WireFormat::Iso8,
            _ => WireFormat::Kwp {
                length_in_format: self.length_in_format,
            },
        }
    }

    fn accepts(&self, byte: u8) -> bool {
        let plain = byte == self.address;
        let xor = byte == self.address ^ MULTIPOINT_XOR;
        match self.accept {
            AddressAccept::Plain => plain,
            AddressAccept::Xor80 => xor,
            AddressAccept::Both => plain || xor,
        }
    }
}

/// Init failure modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitFault {
    /// No answer to any wake-up.
    Silent,
    /// Answer `55 kw1 kw2` with these key bytes instead of the profile's.
    WrongKeywords(u8, u8),
    /// KWP: never send ~address after ~KW2.
    NoAddressComplement,
    /// Send this byte instead of the 0x55 sync byte (wrong baud or a line problem); the key
    /// bytes follow as usual.
    BadSync(u8),
}

/// Reply failure modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyFault {
    /// Corrupt the checksum byte of the reply.
    BadChecksum,
    /// `count` x `7F sid 78`, then the real reply.
    ResponsePending { count: u8 },
    /// `7F sid nrc` instead of the reply.
    Negative { nrc: u8 },
    /// Drop the last `count` bytes of the reply frame.
    DropBytes { count: usize },
    /// Send the reply `delay_ms` after the request instead of after P2.
    Slow { delay_ms: u64 },
    /// No reply at all.
    NoReply,
}

/// A reply fault applied to requests starting with `on` (None: every request, including
/// TesterPresent and Stop), `times` times (None: always).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultRule {
    pub on: Option<Vec<u8>>,
    pub fault: ReplyFault,
    pub times: Option<u32>,
    /// Matching requests answered normally before the rule applies ([`FaultRule::after`]).
    pub skip: u32,
}

impl FaultRule {
    pub fn on(prefix: &[u8], fault: ReplyFault) -> FaultRule {
        FaultRule {
            on: Some(prefix.to_vec()),
            fault,
            times: None,
            skip: 0,
        }
    }

    pub fn once(mut self) -> FaultRule {
        self.times = Some(1);
        self
    }

    /// Answer the first `n` matching requests normally, then apply the fault (e.g. a read
    /// that worked a while and then starts failing).
    pub fn after(mut self, n: u32) -> FaultRule {
        self.skip = n;
        self
    }
}

/// A scheduled scenario change ([`SpecEcu::with_change_after`]).
#[derive(Debug, Clone)]
struct ScenarioChange {
    on: Vec<u8>,
    delay_ms: u64,
    data: String,
    value: kline_spec::Value,
    armed_at: Option<u64>,
    done: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Asleep,
    AwaitInvKw2 { kw2: u8, kw2_end: u64, addr: u8 },
    AwaitStartComm { deadline: u64 },
    Ready,
}

/// The Bosch 5.3 ABS keep-alive interval: X24_ABS_Bosch5.3 `TesterPresent@Delay` 7000 ms; the
/// car kept its ISO8 session with it (capture 2026-11-23, docs/protocol.md "Bosch 5.3 ABS").
pub const BOSCH_ABS_TESTER_PRESENT_MS: u64 = 7_000;

/// Session timeout of an ISO8 ECU whose definition declares a keep-alive interval of `tp_ms`:
/// the ECU must stay awake through that interval (it does on the car for the ABS), so it sleeps
/// only after `tp_ms` plus 1 s of silence. ours. KWP ECUs keep P3max (ISO 14230-2, 5000 ms).
pub fn iso8_session_timeout_ms(tp_ms: u64) -> u64 {
    tp_ms.max(EcuTiming::default().session_timeout_ms) + 1_000
}

/// The simulated ECU. Attach it to a [`crate::bus::Bus`] (usually through
/// [`crate::line::VirtualLine`] or [`crate::pty::PtySim`]).
pub struct SpecEcu {
    profile: EcuProfile,
    def: EcuDef,
    scenario: Scenario,
    journal: Journal,
    init_fault: Option<InitFault>,
    faults: Vec<FaultRule>,
    changes: Vec<ScenarioChange>,
    /// Scenario changes a positive reply makes ([`SpecEcu::with_effect`]).
    effects: Vec<(Vec<u8>, String, kline_spec::Value)>,
    state: State,
    asm: Assembler,
    last_rx_byte: u64,
    frame_rejected: bool,
    last_tx_end: u64,
    last_activity: u64,
}

impl SpecEcu {
    /// The scenario is checked against the definition (unknown names and off-grid values are
    /// errors here, not wrong bytes later).
    pub fn new(profile: EcuProfile, def: EcuDef, scenario: Scenario) -> Result<SpecEcu, SimError> {
        scenario.check(&def)?;
        if let Some(f) = &def.function {
            if f.address != profile.address {
                return Err(SimError::AddressMismatch {
                    profile: profile.address,
                    definition: f.address,
                });
            }
        }
        Ok(SpecEcu {
            profile,
            def,
            scenario,
            journal: Journal::new(),
            init_fault: None,
            faults: Vec::new(),
            changes: Vec::new(),
            effects: Vec::new(),
            state: State::Asleep,
            asm: Assembler::default(),
            last_rx_byte: 0,
            frame_rejected: false,
            last_tx_end: 0,
            last_activity: 0,
        })
    }

    pub fn with_init_fault(mut self, f: InitFault) -> SpecEcu {
        self.init_fault = Some(f);
        self
    }

    pub fn with_fault(mut self, rule: FaultRule) -> SpecEcu {
        self.faults.push(rule);
        self
    }

    /// Change scenario value `data` to `value` `delay_ms` after the ECU first receives a
    /// request starting with `on` (e.g. a remote pressed 2 s after `31 C7`: `21 A3` then
    /// reports the frame). Checked against the definition when it applies; an invalid value
    /// is a journal note and the old value stays.
    pub fn with_change_after(
        mut self,
        on: &[u8],
        delay_ms: u64,
        data: &str,
        value: kline_spec::Value,
    ) -> SpecEcu {
        self.changes.push(ScenarioChange {
            on: on.to_vec(),
            delay_ms,
            data: data.to_string(),
            value,
            armed_at: None,
            done: false,
        });
        self
    }

    /// Set scenario value `data` to `value` whenever the ECU executes a request starting with
    /// `on`: it answered positively (a reply lost on the line still counts as executed; a
    /// negative reply does not). E.g. a BMT whose lock memory follows K-line `31 A0 00 01`.
    pub fn with_effect(mut self, on: &[u8], data: &str, value: kline_spec::Value) -> SpecEcu {
        self.effects.push((on.to_vec(), data.to_string(), value));
        self
    }

    fn apply_effects(&mut self, t: u64, payload: &[u8]) {
        for i in 0..self.effects.len() {
            let (on, data, value) = &self.effects[i];
            if !payload.starts_with(on) {
                continue;
            }
            let next = self.scenario.clone().set(data, value.clone());
            match next.check(&self.def) {
                Ok(()) => self.scenario = next,
                Err(e) => self
                    .journal
                    .note(t, format!("effect on {data} refused: {e}")),
            }
        }
    }

    /// Arm the changes `payload` triggers and apply the ones that are due at `t`.
    fn apply_changes(&mut self, t: u64, payload: &[u8]) {
        for c in &mut self.changes {
            if c.armed_at.is_none() && payload.starts_with(&c.on) {
                c.armed_at = Some(t);
            }
        }
        for i in 0..self.changes.len() {
            let c = &self.changes[i];
            if c.done || c.armed_at.is_none_or(|a| t < a + c.delay_ms) {
                continue;
            }
            let next = self.scenario.clone().set(&c.data, c.value.clone());
            match next.check(&self.def) {
                Ok(()) => self.scenario = next,
                Err(e) => self
                    .journal
                    .note(t, format!("scenario change {} refused: {e}", c.data)),
            }
            self.changes[i].done = true;
        }
    }

    /// Shared handle on the write log / violations / notes.
    pub fn journal(&self) -> Journal {
        self.journal.clone()
    }

    pub fn profile(&self) -> &EcuProfile {
        &self.profile
    }

    fn fmt(&self) -> WireFormat {
        self.profile.wire_format()
    }

    fn sent(&mut self, end: u64) {
        self.last_tx_end = self.last_tx_end.max(end);
        self.last_activity = self.last_activity.max(end);
    }

    fn take_fault(&mut self, payload: &[u8]) -> Option<ReplyFault> {
        let rule = self
            .faults
            .iter_mut()
            .find(|r| r.times != Some(0) && r.on.as_ref().is_none_or(|p| payload.starts_with(p)))?;
        if rule.skip > 0 {
            rule.skip -= 1;
            return None;
        }
        if let Some(n) = &mut rule.times {
            *n -= 1;
        }
        Some(rule.fault)
    }

    /// The reply payload for a request, or None for no reply. Also returns whether the ECU
    /// falls asleep after it.
    fn answer(&mut self, t: u64, payload: &[u8]) -> (Option<Vec<u8>>, bool) {
        self.apply_changes(t, payload);
        let iso8 = self.profile.protocol == EcuProtocol::Iso8;
        let sid = payload[0];
        let matches: Vec<&kline_spec::Request> = self
            .def
            .requests
            .iter()
            .filter(|r| request_matches(&self.def, r, payload))
            .collect();
        if let Some(kind) = classify(iso8, self.profile.address, payload) {
            self.journal.record_write(WriteRecord {
                t_ms: t,
                ecu_address: self.profile.address,
                payload: payload.to_vec(),
                kind,
                request_names: matches.iter().map(|r| r.name.clone()).collect(),
            });
        }
        let (kw1, kw2) = self.profile.keywords;
        match (iso8, payload) {
            (false, [0x81]) => return (Some(vec![0xC1, kw1, kw2]), false),
            (false, [0x82]) => return (Some(vec![0xC2]), true),
            (false, [0x3E, ..]) => return (Some(vec![0x7E]), false),
            (true, [0x36]) => return (self.profile.iso8_tester_present_reply.clone(), false),
            (true, [0x3F]) => return (self.profile.iso8_stop_reply.clone(), true),
            _ => {}
        }
        if !matches.is_empty() {
            let offset = self.profile.positive_sid_offset;
            return match positive_response_with(
                &self.def,
                &matches,
                payload,
                &self.scenario,
                offset,
            ) {
                Ok(r) => (Some(r), false),
                Err(e) => {
                    self.journal
                        .note(t, format!("cannot build reply to {payload:02X?}: {e}"));
                    (Some(vec![0x7F, sid, NRC_GENERAL_REJECT]), false)
                }
            };
        }
        let sid_known = self
            .def
            .requests
            .iter()
            .any(|r| r.sent_bytes.0.first() == Some(&sid));
        let nrc = if sid_known {
            NRC_SUB_FUNCTION_NOT_SUPPORTED
        } else {
            NRC_SERVICE_NOT_SUPPORTED
        };
        (Some(vec![0x7F, sid, nrc]), false)
    }

    fn handle_frame(&mut self, t: u64, p: Parsed, out: &mut Out) {
        if let Some(tgt) = p.target {
            if tgt != self.profile.address {
                self.journal.note(
                    t,
                    format!("frame for {tgt:#04x} ignored: {:02X?}", p.payload),
                );
                return;
            }
        }
        if p.payload.is_empty() {
            return;
        }
        let tester = p.source.unwrap_or(0);
        let (reply, sleep_after) = self.answer(t, &p.payload);
        let fault = self.take_fault(&p.payload);
        if sleep_after {
            self.state = State::Asleep;
            self.journal.note(t, "session stopped");
        }
        let Some(reply) = reply else { return };
        if reply.first() != Some(&0x7F) && !matches!(fault, Some(ReplyFault::Negative { .. })) {
            self.apply_effects(t, &p.payload);
        }
        let (fmt, ecu, tm) = (self.fmt(), self.profile.address, self.profile.timing);
        let wire = |pl: &[u8]| encode_reply(fmt, ecu, tester, pl);
        let sid = p.payload[0];
        let start = t + tm.p2_ms;
        let end = match fault {
            None => out.send(start, &wire(&reply), tm.p1_ms),
            Some(ReplyFault::NoReply) => return,
            Some(ReplyFault::Slow { delay_ms }) => out.send(t + delay_ms, &wire(&reply), tm.p1_ms),
            Some(ReplyFault::Negative { nrc }) => {
                out.send(start, &wire(&[0x7F, sid, nrc]), tm.p1_ms)
            }
            Some(ReplyFault::ResponsePending { count }) => {
                let mut s = start;
                for _ in 0..count {
                    let e = out.send(s, &wire(&[0x7F, sid, NRC_RESPONSE_PENDING]), tm.p1_ms);
                    s = e + tm.pending_interval_ms;
                }
                out.send(s, &wire(&reply), tm.p1_ms)
            }
            Some(ReplyFault::BadChecksum) => {
                let mut w = wire(&reply);
                if let Some(cs) = w.last_mut() {
                    *cs = cs.wrapping_add(1);
                }
                out.send(start, &w, tm.p1_ms)
            }
            Some(ReplyFault::DropBytes { count }) => {
                let mut w = wire(&reply);
                w.truncate(w.len().saturating_sub(count));
                out.send(start, &w, tm.p1_ms)
            }
        };
        self.sent(end);
    }

    fn on_frame_byte(&mut self, t: u64, b: u8, out: &mut Out) {
        let tol = self.profile.tolerance;
        let byte_start = t.saturating_sub(BYTE_MS);
        if !self.asm.is_empty() {
            let gap = byte_start.saturating_sub(self.last_rx_byte);
            if gap > tol.p4_max_ms {
                self.journal.violation(
                    t,
                    ViolationKind::InterByte,
                    format!("{gap} ms between request bytes, max {}", tol.p4_max_ms),
                );
                self.asm.clear();
            }
        }
        if self.asm.is_empty() {
            self.frame_rejected = false;
            let earliest = self.last_tx_end + tol.p3_min_ms;
            if byte_start < earliest {
                self.journal.violation(
                    t,
                    ViolationKind::P3Min,
                    format!(
                        "request started {} ms after the reply ended, P3min {}",
                        byte_start as i64 - self.last_tx_end as i64,
                        tol.p3_min_ms
                    ),
                );
                self.frame_rejected = true;
            }
        }
        self.last_rx_byte = t;
        self.last_activity = self.last_activity.max(t);
        match self.asm.push(b, self.fmt()) {
            Feed::More => {}
            Feed::Bad(e) => self.journal.violation(t, ViolationKind::BadFrame, e),
            Feed::Frame(p) if self.frame_rejected => {
                self.journal
                    .note(t, format!("ignored request {:02X?}", p.payload));
            }
            Feed::Frame(p) => match self.state {
                State::AwaitStartComm { .. } => {
                    if p.payload == [0x81] && p.target == Some(self.profile.address) {
                        let (k1, k2) = self.profile.fast_init_keywords;
                        let tester = p.source.unwrap_or(0);
                        let w =
                            encode_reply(self.fmt(), self.profile.address, tester, &[0xC1, k1, k2]);
                        let end =
                            out.send(t + self.profile.timing.p2_ms, &w, self.profile.timing.p1_ms);
                        self.sent(end);
                        self.state = State::Ready;
                    } else {
                        self.journal.note(
                            t,
                            format!("expected StartCommunication, got {:02X?}", p.payload),
                        );
                    }
                }
                _ => self.handle_frame(t, p, out),
            },
        }
    }
}

impl Responder for SpecEcu {
    fn on_wake(&mut self, wake: Wake, out: &mut Out) {
        self.asm.clear();
        if self.init_fault == Some(InitFault::Silent) {
            self.journal.note(0, "wake-up ignored (InitFault::Silent)");
            return;
        }
        match wake {
            Wake::FiveBaud { byte, t_end, .. } => {
                if self.profile.protocol == EcuProtocol::KwpFast || !self.profile.accepts(byte) {
                    self.journal
                        .note(t_end, format!("5-baud address {byte:#04x} not accepted"));
                    self.state = State::Asleep;
                    return;
                }
                let tm = self.profile.timing;
                let (k1, k2) = match self.init_fault {
                    Some(InitFault::WrongKeywords(a, b)) => (a, b),
                    _ => self.profile.keywords,
                };
                let sync = match self.init_fault {
                    Some(InitFault::BadSync(b)) => b,
                    _ => SYNC,
                };
                let e55 = out.send(t_end + tm.w1_ms, &[sync], 0);
                let e1 = out.send(e55 + tm.w2_ms, &[k1], 0);
                let e2 = out.send(e1 + tm.w3_ms, &[k2], 0);
                self.sent(e2);
                self.state = if self.profile.protocol == EcuProtocol::Iso8 {
                    State::Ready
                } else {
                    State::AwaitInvKw2 {
                        kw2: k2,
                        kw2_end: e2,
                        addr: byte,
                    }
                };
            }
            Wake::Fast { t_high, .. } => {
                if !self.profile.accept_fast_init || self.profile.protocol == EcuProtocol::Iso8 {
                    self.journal.note(t_high, "fast init not accepted");
                    self.state = State::Asleep;
                    return;
                }
                self.last_tx_end = t_high;
                self.last_activity = t_high;
                self.state = State::AwaitStartComm {
                    deadline: t_high + self.profile.timing.start_comm_timeout_ms,
                };
            }
        }
    }

    fn on_byte(&mut self, t: u64, b: u8, out: &mut Out) {
        match self.state {
            State::Asleep => self
                .journal
                .note(t, format!("asleep, byte {b:#04x} ignored")),
            State::AwaitInvKw2 { kw2, kw2_end, addr } => {
                let after = t.saturating_sub(BYTE_MS).saturating_sub(kw2_end);
                let (lo, hi) = self.profile.tolerance.w4_window_ms;
                if b != !kw2 || !(lo..=hi).contains(&after) {
                    self.journal.violation(
                        t,
                        ViolationKind::Handshake,
                        format!(
                            "got {b:#04x} {after} ms after KW2, want {:#04x} within {lo}..={hi}",
                            !kw2
                        ),
                    );
                    self.state = State::Asleep;
                    return;
                }
                self.last_activity = t;
                if self.init_fault == Some(InitFault::NoAddressComplement) {
                    self.state = State::Asleep;
                    return;
                }
                let end = out.send(t + self.profile.timing.w4_ms, &[!addr], 0);
                self.sent(end);
                self.state = State::Ready;
            }
            State::AwaitStartComm { .. } | State::Ready => self.on_frame_byte(t, b, out),
        }
    }

    fn next_timer(&self) -> Option<u64> {
        match self.state {
            State::Asleep => None,
            State::AwaitInvKw2 { kw2_end, .. } => {
                Some(kw2_end + self.profile.tolerance.w4_window_ms.1 + BYTE_MS + 1)
            }
            State::AwaitStartComm { deadline } => Some(deadline),
            State::Ready => Some(self.last_activity + self.profile.timing.session_timeout_ms),
        }
    }

    fn on_timer(&mut self, t: u64, _out: &mut Out) {
        let what = match self.state {
            State::AwaitInvKw2 { .. } => {
                let hi = self.profile.tolerance.w4_window_ms.1;
                self.journal.violation(
                    t,
                    ViolationKind::Handshake,
                    format!("no ~KW2 within W4max {hi} ms"),
                );
                "no ~KW2, back to sleep"
            }
            State::AwaitStartComm { .. } => "no StartCommunication, back to sleep",
            State::Ready => "tester silent for P3max, session timed out",
            State::Asleep => return,
        };
        self.journal.note(t, what);
        self.asm.clear();
        self.state = State::Asleep;
    }
}
