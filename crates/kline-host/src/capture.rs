//! JSONL session capture in the kline-sim replay format (docs/sim.md "Replay log"):
//! `{"t_ms":300,"dir":"tx","kind":"five_baud","hex":"7A","unix_ms":1763800000000}`.
//!
//! `t_ms` is the transport clock (ms since the port was opened). `unix_ms` (wall clock) is an
//! extra field the simulator's reader ignores. Each line is flushed as it is written, so a
//! capture survives Ctrl-C or a crash.

use std::fs::{self, File};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{exit, CliError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dir {
    Tx,
    Rx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    FiveBaud,
    FastInit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub t_ms: u64,
    pub dir: Dir,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    pub hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unix_ms: Option<u64>,
}

impl Entry {
    pub fn bytes(&self) -> Result<Vec<u8>, String> {
        crate::parse_hex(&self.hex).ok_or_else(|| format!("invalid hex {:?}", self.hex))
    }
}

pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `YYYYMMDDTHHMMSSZ` for a Unix time in seconds (UTC, proleptic Gregorian).
pub fn utc_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        sod / 3600,
        (sod / 60) % 60,
        sod % 60
    )
}

/// The capture file of one session.
pub struct Capture {
    path: PathBuf,
    file: File,
    entries: usize,
}

impl Capture {
    /// Create `<dir>/<UTC stamp>-<label>-<command>-<pid>.jsonl` (the directory is created).
    pub fn create(dir: &Path, label: &str, command: &str) -> Result<Capture, CliError> {
        fs::create_dir_all(dir).map_err(|e| {
            CliError::new(
                exit::CAPTURE,
                "capture",
                format!("cannot create capture directory {}: {e}", dir.display()),
            )
        })?;
        let safe: String = label
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let name = format!(
            "{}-{safe}-{command}-{}.jsonl",
            utc_stamp(unix_ms() / 1000),
            std::process::id()
        );
        let path = dir.join(name);
        let file = File::create(&path).map_err(|e| {
            CliError::new(
                exit::CAPTURE,
                "capture",
                format!("cannot create capture {}: {e}", path.display()),
            )
        })?;
        Ok(Capture {
            path,
            file,
            entries: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> usize {
        self.entries
    }

    pub fn push(&mut self, e: &Entry) {
        if let Ok(s) = serde_json::to_string(e) {
            // A full disk must not abort the session on the car; the trace still shows it.
            let _ = writeln!(self.file, "{s}");
            let _ = self.file.flush();
            self.entries += 1;
        }
    }
}

/// Read a capture (blank lines skipped).
pub fn read(path: &Path) -> Result<Vec<Entry>, CliError> {
    let err = |m: String| CliError::new(exit::CAPTURE, "capture", m);
    let f = File::open(path).map_err(|e| err(format!("cannot open {}: {e}", path.display())))?;
    let mut out = Vec::new();
    for (i, line) in std::io::BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|e| err(format!("{}: {e}", path.display())))?;
        if line.trim().is_empty() {
            continue;
        }
        let e: Entry = serde_json::from_str(&line)
            .map_err(|e| err(format!("{} line {}: {e}", path.display(), i + 1)))?;
        e.bytes()
            .map_err(|m| err(format!("{} line {}: {m}", path.display(), i + 1)))?;
        out.push(e);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_stamp_known_dates() {
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        // 2026-11-22 10:11:12 UTC
        assert_eq!(utc_stamp(1_795_342_272), "20261122T101112Z");
        // 2000-02-29 23:59:59 UTC (leap day)
        assert_eq!(utc_stamp(951_868_799), "20000229T235959Z");
    }
}
