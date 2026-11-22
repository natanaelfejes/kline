//! The simulated ECU's own timing, and how strictly it checks the tester's timing.
//!
//! Source tags (as in docs/protocol.md):
//! - `ISO 9141-2` / `ISO 14230-2`: the K-line standards' W1..W5 and P1..P4 windows. Nothing here
//!   is measured on the car; the real ECUs may be faster, slower or more forgiving.
//! - `XML <K>`: the ECU definition file.
//! - `ours`: a choice of the simulator.

/// Time one byte occupies on the line: 10 bits at 10400 baud = 0.96 ms, rounded up to 1 ms
/// because the simulator's clock counts whole milliseconds (ours).
pub const BYTE_MS: u64 = 1;

/// One 5-baud bit: 200 ms. ISO 9141-2 (5 baud).
pub const FIVE_BAUD_BIT_MS: u64 = 200;

/// When the ECU itself answers (all ms).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcuTiming {
    /// W1: end of the 5-baud address byte (end of its stop bit) to the sync byte 0x55.
    /// ISO 9141-2 / 14230-2 W1 20..300; 60 is ours.
    pub w1_ms: u64,
    /// W2: 0x55 to KW1. ISO W2 5..20; 10 is ours.
    pub w2_ms: u64,
    /// W3: KW1 to KW2. ISO W3 0..20; 10 is ours.
    pub w3_ms: u64,
    /// W4 (ECU side): ~KW2 received to ~address sent. ISO 14230-2 W4 25..50; 30 is ours.
    pub w4_ms: u64,
    /// P1: gap between the ECU's reply bytes. ISO P1 0..20; 0 is ours.
    pub p1_ms: u64,
    /// P2: end of a request to the first reply byte. ISO 14230-2 P2 25..50; 25 is ours.
    pub p2_ms: u64,
    /// Gap between consecutive `7F xx 78` frames and before the final reply. ours.
    pub pending_interval_ms: u64,
    /// P3max: the ECU falls asleep when the tester is silent this long. ISO 14230-2 P3max 5000.
    pub session_timeout_ms: u64,
    /// After a fast-init wake-up, how long the ECU waits for StartCommunication. ours.
    pub start_comm_timeout_ms: u64,
}

impl Default for EcuTiming {
    fn default() -> Self {
        EcuTiming {
            w1_ms: 60,
            w2_ms: 10,
            w3_ms: 10,
            w4_ms: 30,
            p1_ms: 0,
            p2_ms: 25,
            pending_interval_ms: 100,
            session_timeout_ms: 5_000,
            start_comm_timeout_ms: 1_000,
        }
    }
}

/// Strict: the windows of the standards (what a picky real ECU might enforce).
/// Lenient: wide windows, for transports with OS scheduling jitter (pty) and for tests that
/// are about something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToleranceMode {
    Strict,
    Lenient,
}

/// Bounds the simulated ECU checks on the tester's timing. Out of bounds: the ECU records a
/// [`crate::journal::Violation`] and does not answer (a wake-up is not accepted, a frame is
/// ignored).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tolerance {
    pub mode: ToleranceMode,
    /// Strict 5-baud: every edge within this many ms of a 200 ms bit boundary (Strict: 10 =
    /// ISO 9141-2's 5 baud +-5 %). `None`: only the level in the middle of each bit is sampled.
    pub five_baud_edge_ms: Option<u64>,
    /// Minimum idle line before a wake-up. ISO 14230-2 W5 >= 300 (Strict); 0 lenient.
    pub idle_before_wake_ms: u64,
    /// Accepted window for the tester's ~KW2 after the ECU's KW2 (W4). ISO 14230-2 25..50
    /// (Strict); 0..1000 lenient.
    pub w4_window_ms: (u64, u64),
    /// Largest gap between two bytes of one request (P4). ISO 14230-2 P4max 20 (Strict);
    /// 100 lenient. A larger gap discards the partial frame.
    pub p4_max_ms: u64,
    /// Smallest gap between the end of the ECU's reply and the next request (P3min).
    /// Strict: `ReplyToRequest@Delay` of the definition (5 for the car's ECUs, what DDT2000
    /// uses; ISO 14230-2 says 55 for normal timing). Lenient: 0.
    pub p3_min_ms: u64,
    /// Accepted fast-init low pulse (TiniL). ISO 14230-2 25 +- 1 (Strict); 15..40 lenient.
    pub fast_init_low_ms: (u64, u64),
}

impl Tolerance {
    pub fn strict(p3_min_ms: u64) -> Tolerance {
        Tolerance {
            mode: ToleranceMode::Strict,
            five_baud_edge_ms: Some(10),
            idle_before_wake_ms: 300,
            w4_window_ms: (25, 50),
            p4_max_ms: 20,
            p3_min_ms,
            fast_init_low_ms: (24, 26),
        }
    }

    pub fn lenient() -> Tolerance {
        Tolerance {
            mode: ToleranceMode::Lenient,
            five_baud_edge_ms: None,
            idle_before_wake_ms: 0,
            w4_window_ms: (0, 1_000),
            p4_max_ms: 100,
            p3_min_ms: 0,
            fast_init_low_ms: (15, 40),
        }
    }
}
