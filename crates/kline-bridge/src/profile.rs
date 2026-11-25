//! The two ECUs the bridge talks to, their link settings, and the polling profiles.

use kline_core::safety::Protocol;
use kline_core::{InitKind, KTiming, LinkConfig};

/// ECUs reachable over BLE in protocol v1. Anything else is refused by the codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcuId {
    /// Siemens Sirius 32 injection, 0x7A (docs/protocol.md "The Clio II ECUs").
    Sirius,
    /// BMT body computer (also the immobilizer decoder), 0x26.
    Bmt,
    /// Bosch 5.3 ABS, ISO8 at 0x01, key bytes `DC 83` (docs/protocol.md, car 2026-11-23).
    Abs,
    /// Airbag, 0x2C (AB70 / AB80 definitions; which one fits this car is NOT VERIFIED).
    Airbag,
}

/// Every ECU the bridge knows, for tables and tests.
pub const ALL_ECUS: [EcuId; 4] = [EcuId::Sirius, EcuId::Bmt, EcuId::Abs, EcuId::Airbag];

impl EcuId {
    pub fn address(self) -> u8 {
        match self {
            EcuId::Sirius => kline_core::safety::SIRIUS_ADDRESS,
            EcuId::Bmt => kline_core::safety::BMT_ADDRESS,
            EcuId::Abs => kline_core::safety::ABS_ADDRESS,
            EcuId::Airbag => kline_core::safety::AIRBAG_ADDRESS,
        }
    }

    pub fn from_address(a: u8) -> Option<EcuId> {
        ALL_ECUS.into_iter().find(|e| e.address() == a)
    }

    /// Framing of the init the bridge runs for this ECU.
    pub fn protocol(self) -> Protocol {
        match self {
            EcuId::Abs => Protocol::Iso8,
            _ => Protocol::Kwp,
        }
    }

    /// Key bytes the definition declares (`<K>` KW1/KW2): `D0 8F` for the KWP ECUs, `DC 83`
    /// for the Bosch 5.3 ABS (kline-core `BOSCH_ABS_KEYWORDS`). The policy decides on the key
    /// bytes the init actually received; these are only for the checks before a session.
    pub fn keywords(self) -> (u8, u8) {
        match self {
            EcuId::Abs => kline_core::safety::BOSCH_ABS_KEYWORDS,
            _ => (0xD0, 0x8F),
        }
    }

    /// Link settings without a definition (the firmware has no XML), the `<K>` values of the
    /// car's definitions (listed by kline-core's `spec::link_config`, checked by
    /// `tests/definition_parity.rs` when `ddt-xml/` is present):
    /// - Sirius, BMT: KWP 5-baud, Multipoint declared, KW `D0 8F`, FirstRequest 20,
    ///   InterByte 0, ReplyToRequest 5, RequestToReply 300, TesterPresent 3000, OnError 300,
    ///   KW2 timeout 300. Plain-address `SingleEcu` init, which worked on the car on
    ///   2026-11-22 (PROGRESS.md 19:55 and 19:57: 5-baud 7A and 26 OK, fast init no reply).
    /// - Airbag: the same KWP class, with AB70's InterByte 3 and ReplyToRequest 20 (the slower
    ///   of the two airbag definitions; AB80 has 0 and 5).
    /// - ABS: ISO8 5-baud, KW `DC 83`, FirstRequest 100, InterByte 5, ReplyToRequest 300,
    ///   RequestToReply 9300, TesterPresent 7000, OnError 300, KW2 timeout 1500
    ///   (X24_ABS_Bosch5.3.xml).
    pub fn link_config(self) -> LinkConfig {
        let timing = match self {
            EcuId::Sirius | EcuId::Bmt => KTiming {
                init: InitKind::KwpFiveBaud { multipoint: true },
                ..KTiming::default()
            },
            EcuId::Airbag => KTiming {
                init: InitKind::KwpFiveBaud { multipoint: true },
                inter_byte_ms: 3,
                reply_to_request_ms: 20,
                ..KTiming::default()
            },
            EcuId::Abs => KTiming {
                init: InitKind::Iso8FiveBaud,
                first_request_ms: 100,
                inter_byte_ms: 5,
                reply_to_request_ms: 300,
                request_to_reply_timeout_ms: 9_300,
                tester_present_ms: 7_000,
                on_error_ms: 300,
                kw2_timeout_ms: 1_500,
                ..KTiming::default()
            },
        };
        let mut cfg = LinkConfig::new(self.address(), timing);
        cfg.expected_keywords = Some(self.keywords());
        cfg
    }
}

/// What the bridge polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Profile {
    /// No polling.
    Off = 0,
    /// Driving: Sirius `21AA, 21A9, 21AA, 21AA` (MASTER-PLAN-v2 W4; ~21 Hz single-frame rate
    /// measured, so 21AA ~15 Hz and 21A9 ~5 Hz).
    Drive = 1,
    /// Parked: BMT `21A3, 21A0, 21A2` (lock state, doors, battery; BMT-ANALYSIS section 6).
    Parked = 2,
    /// Diagnostics on request only (no polling; the session stays open for requests).
    Diagnostics = 3,
}

impl Profile {
    pub fn from_u8(b: u8) -> Option<Profile> {
        Some(match b {
            0 => Profile::Off,
            1 => Profile::Drive,
            2 => Profile::Parked,
            3 => Profile::Diagnostics,
            _ => return None,
        })
    }

    /// The ECU this profile polls.
    pub fn ecu(self) -> Option<EcuId> {
        match self {
            Profile::Drive => Some(EcuId::Sirius),
            Profile::Parked => Some(EcuId::Bmt),
            Profile::Off | Profile::Diagnostics => None,
        }
    }

    /// The local identifiers read in turn (`21 lid`, Tier 0).
    pub fn schedule(self) -> &'static [u8] {
        match self {
            Profile::Drive => &DRIVE_SCHEDULE,
            Profile::Parked => &PARKED_SCHEDULE,
            Profile::Off | Profile::Diagnostics => &[],
        }
    }
}

/// Sirius live frames: `21AA` (rpm, injection, fuel) three times per `21A9` (speed, coolant,
/// battery). MASTER-PLAN-v2 W4.
pub const DRIVE_SCHEDULE: [u8; 4] = [0xAA, 0xA9, 0xAA, 0xAA];
/// BMT parked frames: `21A3` lock state, `21A0` inputs (doors), `21A2` values (battery).
pub const PARKED_SCHEDULE: [u8; 3] = [0xA3, 0xA0, 0xA2];

/// Live extras (the "advanced" drive profile, `LIVE_EXTRAS`): the other Sirius frames that
/// carry something the app can show, read one at a time between drive cycles, in turn. All are
/// Tier 0 reads `21 lid`. From the 2026-11-22 drive analysis (SIRIUS-ANALYSIS 4.2 and 6) and
/// the definition's item list: `A0` input faults (present and stored), `A1` output faults,
/// `A2` input and state flags, `A4` output states (fan, fuel pump relay, warning lamps),
/// `A8` fault summary, `AB` idle regulation, `AD` OBD status (misfire, oxygen sensor heaters).
/// Not read: the copies `A3`, `A5` to `A7`, the constant `AC`, `AE`, `AF`, `82`, `E0`, the
/// counters `C6`/`C7`, and `BD` (immobilizer related; reading would be Tier 0, but the app
/// has no use for it).
pub const EXTRA_SCHEDULE: [u8; 7] = [0xA0, 0xA1, 0xA2, 0xA4, 0xA8, 0xAB, 0xAD];
