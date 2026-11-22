//! Recorded-frame replay: a JSONL log of what a tester wrote and read, played back as the ECU.
//!
//! Log format (one JSON object per line, docs/sim.md):
//!
//! ```text
//! {"t_ms":300,"dir":"tx","kind":"five_baud","hex":"7A"}
//! {"t_ms":2283,"dir":"rx","hex":"55D08F"}
//! {"t_ms":2308,"dir":"tx","hex":"70"}
//! {"t_ms":2340,"dir":"rx","hex":"7085"}
//! ```
//!
//! - `t_ms`: tester clock. `tx`: when the write began. `rx`: when the read returned (its last
//!   byte had arrived).
//! - `dir`: `tx` (tester -> ECU) or `rx` (ECU -> tester, as the tester read it: the cable's
//!   echo of `tx` bytes may be included and is stripped on replay).
//! - `kind` (tx only, optional): `five_baud` (hex = the address byte bit-banged at 5 baud) or
//!   `fast_init` (hex empty) mark a wake-up; `t_ms` is the first break.
//! - `hex`: bytes as hex, spaces allowed.
//!
//! On replay the ECU waits for the next `tx` entry (a wake-up, or the same bytes from the
//! tester), then sends the `rx` bytes that follow it, echo stripped, at the recorded delays.
//! Anything else is a [`ViolationKind::ReplayMismatch`] and the replay goes silent.

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::bus::{Out, Responder, Wake};
use crate::journal::{Journal, ViolationKind};
use crate::timing::BYTE_MS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dir {
    Tx,
    Rx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    FiveBaud,
    FastInit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub t_ms: u64,
    pub dir: Dir,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<EntryKind>,
    #[serde(with = "hex_str")]
    pub hex: Vec<u8>,
}

impl LogEntry {
    pub fn new(t_ms: u64, dir: Dir, bytes: &[u8]) -> LogEntry {
        LogEntry {
            t_ms,
            dir,
            kind: None,
            hex: bytes.to_vec(),
        }
    }

    pub fn with_kind(mut self, kind: EntryKind) -> LogEntry {
        self.kind = Some(kind);
        self
    }
}

mod hex_str {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(b: &[u8], s: S) -> Result<S::Ok, S::Error> {
        let h: String = b.iter().map(|x| format!("{x:02X}")).collect();
        s.serialize_str(&h)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        let t: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        if !t.len().is_multiple_of(2) {
            return Err(D::Error::custom(format!("odd-length hex {s:?}")));
        }
        (0..t.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&t[i..i + 2], 16))
            .collect::<Result<_, _>>()
            .map_err(|_| D::Error::custom(format!("invalid hex {s:?}")))
    }
}

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error("line {line}: {source}")]
    Json {
        line: usize,
        source: serde_json::Error,
    },
    #[error("read: {0}")]
    Io(#[from] std::io::Error),
}

/// Parse a JSONL log. Blank lines are skipped.
pub fn read_jsonl(r: impl BufRead) -> Result<Vec<LogEntry>, ReplayError> {
    let mut out = Vec::new();
    for (i, line) in r.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(&line).map_err(|e| ReplayError::Json {
            line: i + 1,
            source: e,
        })?);
    }
    Ok(out)
}

pub fn write_jsonl(mut w: impl Write, entries: &[LogEntry]) -> std::io::Result<()> {
    for e in entries {
        serde_json::to_writer(&mut w, e)?;
        w.write_all(b"\n")?;
    }
    Ok(())
}

/// An ECU that replays a log.
pub struct ReplayEcu {
    entries: Vec<LogEntry>,
    /// Index of the next entry to consume.
    cursor: usize,
    rx_buf: Vec<u8>,
    first_byte_t: u64,
    journal: Journal,
    silent: bool,
}

impl ReplayEcu {
    pub fn new(entries: Vec<LogEntry>) -> ReplayEcu {
        ReplayEcu {
            entries,
            cursor: 0,
            rx_buf: Vec::new(),
            first_byte_t: 0,
            journal: Journal::new(),
            silent: false,
        }
    }

    pub fn journal(&self) -> Journal {
        self.journal.clone()
    }

    /// True when every entry was replayed.
    pub fn finished(&self) -> bool {
        self.cursor >= self.entries.len()
    }

    fn expected_tx(&mut self) -> Option<LogEntry> {
        // rx entries left over (e.g. a reply the tester never read) are skipped.
        while let Some(e) = self.entries.get(self.cursor) {
            if e.dir == Dir::Tx {
                return Some(e.clone());
            }
            self.cursor += 1;
        }
        None
    }

    fn mismatch(&mut self, t: u64, detail: String) {
        self.journal
            .violation(t, ViolationKind::ReplayMismatch, detail);
        self.silent = true;
    }

    /// Send the rx entries after the tx at `cursor`. `base`: when that tx began on the bus;
    /// `echo`: its bytes, to strip from the recorded rx.
    fn play_replies(&mut self, base: u64, echo: &[u8], out: &mut Out) {
        let tx_t = self.entries[self.cursor].t_ms;
        self.cursor += 1;
        let mut echo = echo.to_vec();
        let mut earliest = base + echo.len() as u64 * BYTE_MS;
        while let Some(e) = self.entries.get(self.cursor) {
            if e.dir == Dir::Tx {
                break;
            }
            let mut bytes = e.hex.as_slice();
            while let (Some(&b), Some(&x)) = (bytes.first(), echo.first()) {
                if b != x {
                    break;
                }
                bytes = &bytes[1..];
                echo.remove(0);
            }
            if !bytes.is_empty() {
                let delay = e.t_ms.saturating_sub(tx_t);
                let start = (base + delay)
                    .saturating_sub((bytes.len() as u64).saturating_sub(1) * BYTE_MS + BYTE_MS)
                    .max(earliest);
                earliest = out.send(start, bytes, 0);
            }
            self.cursor += 1;
        }
    }
}

impl Responder for ReplayEcu {
    fn on_wake(&mut self, wake: Wake, out: &mut Out) {
        if self.silent {
            return;
        }
        self.rx_buf.clear();
        let Some(exp) = self.expected_tx() else {
            return self.journal.note(0, "replay finished, wake-up ignored");
        };
        let (ok, base, what) = match wake {
            Wake::FiveBaud { byte, t_start, .. } => (
                exp.kind == Some(EntryKind::FiveBaud) && exp.hex == [byte],
                t_start,
                format!("5-baud {byte:#04x}"),
            ),
            Wake::Fast { t_low, .. } => (
                exp.kind == Some(EntryKind::FastInit),
                t_low,
                "fast init".into(),
            ),
        };
        if !ok {
            return self.mismatch(base, format!("{what}, log expects {exp:?}"));
        }
        self.play_replies(base, &[], out);
    }

    fn on_byte(&mut self, t: u64, b: u8, out: &mut Out) {
        if self.silent {
            return;
        }
        let Some(exp) = self.expected_tx() else {
            return self
                .journal
                .note(t, format!("replay finished, byte {b:#04x} ignored"));
        };
        if exp.kind.is_some() {
            return self.mismatch(t, format!("byte {b:#04x}, log expects a wake-up"));
        }
        if self.rx_buf.is_empty() {
            self.first_byte_t = t.saturating_sub(BYTE_MS);
        }
        self.rx_buf.push(b);
        if !exp.hex.starts_with(&self.rx_buf) {
            let got = std::mem::take(&mut self.rx_buf);
            return self.mismatch(
                t,
                format!("tester sent {got:02X?}, log expects {:02X?}", exp.hex),
            );
        }
        if self.rx_buf.len() == exp.hex.len() {
            let echo = std::mem::take(&mut self.rx_buf);
            self.play_replies(self.first_byte_t, &echo, out);
        }
    }

    fn next_timer(&self) -> Option<u64> {
        None
    }

    fn on_timer(&mut self, _t: u64, _out: &mut Out) {}
}
