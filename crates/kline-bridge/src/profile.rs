//! The two ECUs the bridge talks to, their link settings, and the polling profiles.

use kline_core::{InitKind, KTiming, LinkConfig};

/// ECUs reachable over BLE in protocol v1. Anything else is refused by the codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcuId {
    /// Siemens Sirius 32 injection, 0x7A (docs/protocol.md "The Clio II ECUs").
    Sirius,
    /// BMT body computer (also the immobilizer decoder), 0x26.
    Bmt,
}

impl EcuId {
    pub fn address(self) -> u8 {
        match self {
            EcuId::Sirius => kline_core::safety::SIRIUS_ADDRESS,
            EcuId::Bmt => kline_core::safety::BMT_ADDRESS,
        }
    }

    pub fn from_address(a: u8) -> Option<EcuId> {
        match a {
            a if a == kline_core::safety::SIRIUS_ADDRESS => Some(EcuId::Sirius),
            a if a == kline_core::safety::BMT_ADDRESS => Some(EcuId::Bmt),
            _ => None,
        }
    }

    /// Link settings without a definition (the firmware has no XML): the `<K>` values of both
    /// definitions (docs/protocol.md "The Clio II ECUs" and "Timing values": KWP 5-baud with
    /// Multipoint declared, KW `D0 8F`, FirstRequest 20, InterByte 0, ReplyToRequest 5,
    /// RequestToReply 300, TesterPresent 3000, OnError 300) and the plain-address
    /// [`kline_core::InitVariant::SingleEcu`] init that worked on the car on 2026-11-22
    /// (PROGRESS.md 19:55 and 19:57: 5-baud 7A and 26 OK, fast init no reply).
    pub fn link_config(self) -> LinkConfig {
        let timing = KTiming {
            init: InitKind::KwpFiveBaud { multipoint: true },
            ..KTiming::default()
        };
        let mut cfg = LinkConfig::new(self.address(), timing);
        cfg.expected_keywords = Some((0xD0, 0x8F));
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
