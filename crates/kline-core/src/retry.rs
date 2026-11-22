//! Application-level retry and detection policy of DDT2000, as plain data (docs/protocol.md
//! "DDT2000 application behaviour"). None of this exists in DDT2000's protocol layer
//! (KWP2000Diag4 / IOTBD32V3); it lives in the screen player, the ECU scanner and the global
//! failure test. Callers (CLI, ESP32) run the loops; the defaults are DDT2000's.

use crate::timing::InitVariant;

/// The order in which DDT2000 detects the x65 injection (0x7A) and UCH (0x26): fast init
/// first, then a plain 5-baud init (Type I, no Multipoint). RE: DiagScanXml 0x5004ee50 iterates
/// a function's `<ProtocolList>` in file order, trying the `<Search/>` entries; x65_addressing.xml
/// lists `Code 4 KWP2000 FastInit MonoPoint`, then `Code 2 KWP2000 Init 5 Baud Type I`
/// (`MultiPoint="0"`) for both. Our own fallback, MultiEcu, comes after these.
pub const DDT2000_DETECTION_ORDER: [InitVariant; 2] =
    [InitVariant::FastInit, InitVariant::SingleEcu];

/// Retry rules. `Default` gives DDT2000's values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Tries of a request answered with NRC 0x21 busyRepeatRequest or 0x23
    /// routineNotComplete, the first included. RE: DiagScanXml RequestByteWithRetry 0x5007c790
    /// (`iVar6 = 10`), used for the identification during detection.
    pub nrc_tries: u8,
    /// Wait after NRC 0x21 (DDT error 0x7F3 "ECU occuped"). RE: 0x5007c790 `Sleep(200)`.
    pub busy_retry_ms: u32,
    /// Wait after NRC 0x23 (DDT error 0x7FB "ECU working"). RE: 0x5007c790 `Sleep(500)`.
    pub routine_retry_ms: u32,
    /// Init rounds before giving up. RE: DdtDiagScrXml 0x520484a0 "Init retry n/3"; DiagFailures
    /// 0x2404f410 `iVar6 = 3`.
    pub init_attempts: u8,
    /// Wait between init rounds. RE: DdtDiagScrXml 0x520484a0 sets the wait slot `[0x32] = 0x1F4`
    /// after a failed init.
    pub init_retry_ms: u32,
    /// Retries of a refresh cycle in which every request failed, before "Fatal error, No
    /// communication with ECU". RE: DdtDiagScrXml 0x5204ce50 (`param_1[0x39] < 4`,
    /// "Read Data : Retry n/3").
    pub cycle_retries: u8,
    /// Wait before such a retry. RE: DdtDiagScrXml 0x5204ce50 `param_1[0x32] = 0x190`.
    pub cycle_retry_ms: u32,
    /// Wait before the next init after a failed attempt in which the ECU answered (key bytes
    /// or a reply): it may still be in a half-open session. ours (final critique F14):
    /// [`crate::timing::consts::P3_MAX_MS`]; DDT2000 goes on after its usual gap.
    pub answered_init_wait_ms: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            nrc_tries: 10,
            busy_retry_ms: 200,
            routine_retry_ms: 500,
            init_attempts: 3,
            init_retry_ms: 500,
            cycle_retries: 3,
            cycle_retry_ms: 400,
            answered_init_wait_ms: crate::timing::consts::P3_MAX_MS,
        }
    }
}

impl RetryPolicy {
    /// No retries at all (one try of everything).
    pub fn none() -> RetryPolicy {
        RetryPolicy {
            nrc_tries: 1,
            init_attempts: 1,
            cycle_retries: 0,
            ..RetryPolicy::default()
        }
    }

    /// How long to wait before retrying after a negative reply with `nrc`, or `None` when the
    /// NRC is not retried. `tries` is the number of tries made so far (>= 1).
    pub fn nrc_retry_ms(&self, nrc: u8, tries: u8) -> Option<u32> {
        if tries >= self.nrc_tries {
            return None;
        }
        match nrc {
            0x21 => Some(self.busy_retry_ms),
            0x23 => Some(self.routine_retry_ms),
            _ => None,
        }
    }
}

/// When TesterPresent is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TesterPresentMode {
    /// After `TesterPresent@Delay` (3000 ms) of idle line, reply optional. RE: IOTBD32V3 worker.
    #[default]
    Idle,
    /// DDT2000 screen player option "Enable or disable physical TesterPresent request, when
    /// idle": every `interval_ms` since the last TesterPresent whatever else is sent, and the
    /// reply is required (a missing reply fails the exchange). RE: DdtDiagScrXml 0x520484a0
    /// (`2000 < now - last`, request `TesterPresent.WithResponse` on CAN).
    Periodic { interval_ms: u32 },
}

impl TesterPresentMode {
    /// DDT2000's periodic mode: every 2000 ms (RE: DdtDiagScrXml 0x520484a0).
    pub const DDT2000_PERIODIC: TesterPresentMode =
        TesterPresentMode::Periodic { interval_ms: 2000 };
}
