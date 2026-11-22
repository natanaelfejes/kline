//! What the simulated ECU saw: the ECU write log, timing violations and notes.
//!
//! A [`Journal`] is a cheap shared handle (`Arc<Mutex<..>>`), so a test keeps a clone while the
//! ECU runs inside an in-process line or a pty thread.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

/// Why a request counts as dangerous. The simulator's own classification (it does not use
/// clio-core's safety module, so a bug there is not mirrored here); based on SAFETY.md rule 2
/// and 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteKind {
    /// 0x10 with a session other than 0x81 / 0xA0 (e.g. programming 0x85).
    SessionChange,
    /// 0x11 ECU reset, 0x20 return to normal, Safir 0x20xx resets.
    Reset,
    /// 0x14 clear diagnostic information.
    ClearDtc,
    /// 0x27 security access.
    Security,
    /// 0x2E, 0x3B, 0x3D, 0x40, 0x41: write data / memory / configuration.
    Write,
    /// 0x2F, 0x30, 0x61: input/output control, actuators.
    IoControl,
    /// 0x31, 0x32, KWP 0x33, ISO8 0x33 outside 50..54: routines.
    Routine,
    /// 0x34..0x37: download / upload / programming.
    Programming,
    /// BMT `21 A4` transponder read (rule 3).
    Immobilizer,
    /// Any other service, or a malformed variant of a read service.
    Unknown,
}

/// One dangerous request the ECU received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRecord {
    /// When the request was complete on the line (sim clock, ms).
    pub t_ms: u64,
    pub ecu_address: u8,
    /// Request payload (service byte + parameters, no header or checksum).
    pub payload: Vec<u8>,
    pub kind: WriteKind,
    /// Names of the definition's requests with these bytes (empty if unknown).
    pub request_names: Vec<String>,
}

impl fmt::Display for WriteRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "t={} ms ECU {:#04x} {:?} {:02X?} {:?}",
            self.t_ms, self.ecu_address, self.kind, self.payload, self.request_names
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationKind {
    /// 5-baud edges off the 200 ms grid, start bit not low, stop bit not high.
    FiveBaudTiming,
    /// Wake-up started before the line was idle long enough (W5).
    IdleBeforeWake,
    /// Fast-init low pulse outside the window.
    FastInitPulse,
    /// ~KW2 outside W4, or not the complement of KW2.
    Handshake,
    /// Gap inside a request larger than P4max.
    InterByte,
    /// Request sent sooner than P3min after the ECU's reply.
    P3Min,
    /// The tester's frame has a wrong checksum or length, or an unsupported format byte.
    BadFrame,
    /// Replay: the tester sent something other than the recorded frame.
    ReplayMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub t_ms: u64,
    pub kind: ViolationKind,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct JournalData {
    pub writes: Vec<WriteRecord>,
    pub violations: Vec<Violation>,
    /// Free-form events (wake-ups, ignored addresses, session timeouts), for debugging.
    pub notes: Vec<(u64, String)>,
}

/// Shared, cloneable handle on the journal.
#[derive(Debug, Clone, Default)]
pub struct Journal(Arc<Mutex<JournalData>>);

impl Journal {
    pub fn new() -> Journal {
        Journal::default()
    }

    fn lock(&self) -> MutexGuard<'_, JournalData> {
        // A panic while holding the lock only happens in a failing test; keep the data.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record a dangerous request and print it to stderr: it must never go unnoticed.
    pub fn record_write(&self, rec: WriteRecord) {
        eprintln!("clio-sim: ECU WRITE LOG: {rec}");
        self.lock().writes.push(rec);
    }

    pub fn violation(&self, t_ms: u64, kind: ViolationKind, detail: impl Into<String>) {
        let detail = detail.into();
        eprintln!("clio-sim: timing/protocol violation at {t_ms} ms: {kind:?}: {detail}");
        self.lock()
            .violations
            .push(Violation { t_ms, kind, detail });
    }

    pub fn note(&self, t_ms: u64, text: impl Into<String>) {
        self.lock().notes.push((t_ms, text.into()));
    }

    pub fn writes(&self) -> Vec<WriteRecord> {
        self.lock().writes.clone()
    }

    pub fn violations(&self) -> Vec<Violation> {
        self.lock().violations.clone()
    }

    pub fn notes(&self) -> Vec<(u64, String)> {
        self.lock().notes.clone()
    }

    /// Panic with the full write log if any dangerous request reached the ECU.
    pub fn assert_no_writes(&self) {
        let w = self.writes();
        if !w.is_empty() {
            let lines: Vec<String> = w.iter().map(|r| r.to_string()).collect();
            panic!(
                "ECU write log is not empty ({} entries):\n{}",
                w.len(),
                lines.join("\n")
            );
        }
    }

    /// Panic if any violation was recorded.
    pub fn assert_no_violations(&self) {
        let v = self.violations();
        assert!(v.is_empty(), "violations recorded: {v:#?}");
    }
}
