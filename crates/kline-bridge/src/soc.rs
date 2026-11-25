//! Battery state of charge, estimated by the bridge itself (owner decision 2026-11-25).
//!
//! Method: the open-circuit voltage of a **rested** flooded 12 V lead-acid battery maps to its
//! state of charge (rested OCV table below, the usual 20 C values for a flooded battery). The car
//! has no battery sensor, so this is the only honest "battery %":
//!
//! - **Rest time** comes from the bridge's own clock: milliseconds since it last saw the engine
//!   run (rpm above 0 in a Sirius frame) or a charging voltage. After a reset that time is
//!   unknown, and so is the estimate, until the bridge sees the engine run again.
//! - **Voltage history**: while the engine is off and nothing charges, the bridge keeps one
//!   sample of its own divider every [`SAMPLE_EVERY_MS`] (also across its light-sleep wake-ups),
//!   the last [`SAMPLES`] of them. Readings below [`IGNORE_BELOW_MV`] (a load, a start, a bad
//!   reading) and readings within [`DIP_QUIET_MS`] after a dip below the write limit are not
//!   sampled. The estimate uses the median of the last three samples.
//! - A charging voltage also restarts the rest time (it is a "run" for this purpose), also after
//!   a reset.
//! - **Confidence**: `Rested` after 4 h of rest with the last three samples within 20 mV (the
//!   surface charge has gone), `Approximate` after 1 h (reads high, about +-20 %), otherwise
//!   none. Never while charging (13.2 V or more) or with the engine running.
//! - **Caveats**: no temperature is known to the bridge; a cold battery reads low (about 5 mV
//!   per degree below 20 C), so a winter morning reads a few percent low. A load while parked
//!   (ignition on, lights) lowers the voltage; the bridge cannot tell. The divider's accuracy on
//!   the board is NOT VERIFIED. AGM or EFB batteries have a different table.

/// Rested OCV of a flooded 12 V lead-acid battery at 20 C, millivolts to percent.
pub const OCV_TABLE: [(u16, u8); 5] = [
    (11_890, 0),
    (12_060, 25),
    (12_240, 50),
    (12_450, 75),
    (12_650, 100),
];

pub const SAMPLE_EVERY_MS: u64 = 300_000;
pub const SAMPLES: usize = 24;
pub const MINIMUM_REST_MS: u64 = 3_600_000;
pub const RESTED_MS: u64 = 4 * 3_600_000;
/// The last three samples within this spread count as settled.
pub const SETTLED_MV: u16 = 20;
/// Readings below this are never sampled (a 9.5 V dip must not read as 0 %).
pub const IGNORE_BELOW_MV: u16 = 11_500;
/// No sample within this long after a reading below the write limit (12.0 V).
pub const DIP_QUIET_MS: u64 = 60_000;

/// Confidence of the estimate, Status byte 15.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Confidence {
    None = 0,
    Approximate = 1,
    Rested = 2,
}

impl Confidence {
    pub fn from_u8(b: u8) -> Option<Confidence> {
        Some(match b {
            0 => Confidence::None,
            1 => Confidence::Approximate,
            2 => Confidence::Rested,
            _ => return None,
        })
    }
}

/// Percent from a rested voltage, linear between the table points, rounded to whole percent.
pub fn percent_from_ocv(mv: u16) -> u8 {
    let (lo, hi) = (OCV_TABLE[0], OCV_TABLE[OCV_TABLE.len() - 1]);
    if mv <= lo.0 {
        return lo.1;
    }
    if mv >= hi.0 {
        return hi.1;
    }
    for w in OCV_TABLE.windows(2) {
        let ((v0, p0), (v1, p1)) = (w[0], w[1]);
        if mv <= v1 {
            let num = u32::from(mv - v0) * u32::from(p1 - p0);
            let den = u32::from(v1 - v0);
            return p0 + ((num + den / 2) / den) as u8;
        }
    }
    hi.1
}

/// The estimate: percent (rounded to 5) and confidence, or `(None, Confidence::None)`.
/// `rest_ms`: time since the engine last ran or the voltage last read charging (None: not
/// known since the bridge started). `samples`: (time, mV), oldest first.
pub fn estimate(
    rest_ms: Option<u64>,
    samples: &[(u64, u16)],
    charging: bool,
) -> (Option<u8>, Confidence) {
    let none = (None, Confidence::None);
    if charging {
        return none;
    }
    let (Some(rest), Some(&(_, mv))) = (rest_ms, samples.last()) else {
        return none;
    };
    if rest < MINIMUM_REST_MS {
        return none;
    }
    let mv = median3(samples).unwrap_or(mv);
    let p = percent_from_ocv(mv);
    let p = ((u16::from(p) + 2) / 5 * 5) as u8;
    let settled = samples.len() >= 3 && {
        let last = &samples[samples.len() - 3..];
        let max = last.iter().map(|s| s.1).max().unwrap_or(0);
        let min = last.iter().map(|s| s.1).min().unwrap_or(0);
        max - min <= SETTLED_MV
    };
    let c = if rest >= RESTED_MS && settled {
        Confidence::Rested
    } else {
        Confidence::Approximate
    };
    (Some(p), c)
}

/// The median of the last three samples (the newest alone with fewer).
fn median3(samples: &[(u64, u16)]) -> Option<u16> {
    if samples.len() < 3 {
        return samples.last().map(|s| s.1);
    }
    let mut v = [0u16; 3];
    for (i, s) in samples[samples.len() - 3..].iter().enumerate() {
        v[i] = s.1;
    }
    v.sort_unstable();
    Some(v[1])
}

/// The bridge's history: call [`History::on_battery`] with every reading.
#[derive(Debug, Clone, Default)]
pub struct History {
    last_run_ms: Option<u64>,
    samples: alloc::collections::VecDeque<(u64, u16)>,
}

impl History {
    /// `running`: the engine runs or the voltage reads charging now. `last_dip_ms`: the latest
    /// reading below the write limit.
    pub fn on_battery(&mut self, now_ms: u64, mv: u16, running: bool, last_dip_ms: Option<u64>) {
        if running {
            self.last_run_ms = Some(now_ms);
            self.samples.clear();
            return;
        }
        if mv < IGNORE_BELOW_MV
            || last_dip_ms.is_some_and(|d| now_ms.saturating_sub(d) < DIP_QUIET_MS)
        {
            return;
        }
        if self
            .samples
            .back()
            .is_none_or(|&(t, _)| now_ms.saturating_sub(t) >= SAMPLE_EVERY_MS)
        {
            self.samples.push_back((now_ms, mv));
            if self.samples.len() > SAMPLES {
                self.samples.pop_front();
            }
        }
    }

    /// The engine was seen running (rpm above 0).
    pub fn on_engine_running(&mut self, now_ms: u64) {
        self.last_run_ms = Some(now_ms);
        self.samples.clear();
    }

    /// When the engine last ran or the voltage last read charging (None: not since boot).
    pub fn last_run_ms(&self) -> Option<u64> {
        self.last_run_ms
    }

    pub fn estimate(&self, now_ms: u64, charging: bool) -> (Option<u8>, Confidence) {
        let rest = self.last_run_ms.map(|t| now_ms.saturating_sub(t));
        let s: alloc::vec::Vec<(u64, u16)> = self.samples.iter().copied().collect();
        estimate(rest, &s, charging)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same rows are in the app's ClioBridgeProtocolTests (`BridgeSocTests`).
    #[test]
    fn table_and_estimate() {
        assert_eq!(percent_from_ocv(11_500), 0);
        assert_eq!(percent_from_ocv(12_240), 50);
        assert_eq!(percent_from_ocv(12_345), 63);
        assert_eq!(percent_from_ocv(13_000), 100);
        let flat = [(0, 12_450), (300_000, 12_452), (600_000, 12_448)];
        assert_eq!(
            estimate(Some(5 * 3_600_000), &flat, false),
            (Some(75), Confidence::Rested)
        );
        assert_eq!(
            estimate(Some(2 * 3_600_000), &flat, false),
            (Some(75), Confidence::Approximate)
        );
        assert_eq!(
            estimate(Some(1_000_000), &flat, false),
            (None, Confidence::None)
        );
        assert_eq!(estimate(None, &flat, false), (None, Confidence::None));
        assert_eq!(
            estimate(Some(5 * 3_600_000), &flat, true),
            (None, Confidence::None)
        );
        // Not settled: approximate; the median of the last three (12.55 V) is used.
        let falling = [(0, 12_600), (300_000, 12_550), (600_000, 12_500)];
        assert_eq!(
            estimate(Some(5 * 3_600_000), &falling, false),
            (Some(90), Confidence::Approximate)
        );
        // One odd reading among three does not move the result.
        let odd = [(0, 12_450), (300_000, 11_600), (600_000, 12_452)];
        assert_eq!(estimate(Some(5 * 3_600_000), &odd, false).0, Some(75));
    }

    /// Review r2: a 9.5 V dip must not read 0 %, nor a reading in the minute after a dip.
    #[test]
    fn dips_are_never_sampled() {
        let mut h = History::default();
        h.on_battery(0, 14_100, true, None);
        h.on_battery(5 * 3_600_000, 9_500, false, Some(5 * 3_600_000));
        assert_eq!(h.estimate(5 * 3_600_000, false), (None, Confidence::None));
        h.on_battery(5 * 3_600_000 + 30_000, 12_450, false, Some(5 * 3_600_000));
        assert_eq!(
            h.estimate(5 * 3_600_000 + 30_000, false),
            (None, Confidence::None),
            "within 60 s of the dip"
        );
        h.on_battery(5 * 3_600_000 + 61_000, 12_450, false, Some(5 * 3_600_000));
        assert_eq!(
            h.estimate(5 * 3_600_000 + 61_000, false),
            (Some(75), Confidence::Approximate)
        );
    }

    /// A reading below 11.5 V is never sampled, even without a dip (a load, a bad reading).
    #[test]
    fn low_readings_are_never_sampled() {
        let mut h = History::default();
        h.on_battery(0, 14_100, true, None);
        h.on_battery(2 * 3_600_000, 11_400, false, None);
        assert_eq!(h.samples.len(), 0);
        assert_eq!(h.estimate(2 * 3_600_000, false), (None, Confidence::None));
    }

    /// The samples from before a run are dropped by a running reading: a full battery before a
    /// drive does not lift the estimate after it.
    #[test]
    fn a_running_reading_drops_the_samples() {
        let mut h = History::default();
        h.on_battery(0, 14_100, true, None);
        for k in 1..=3u64 {
            h.on_battery(3_600_000 + k * 300_000, 12_650, false, None);
        }
        let t = 2 * 3_600_000;
        h.on_battery(t, 14_100, true, None);
        h.on_battery(t + 2 * 3_600_000, 12_240, false, None);
        assert_eq!(
            h.estimate(t + 2 * 3_600_000, false),
            (Some(50), Confidence::Approximate)
        );
    }

    /// `running` (a charging voltage) starts the rest time on its own, and an engine run
    /// restarts it and drops the samples.
    #[test]
    fn running_and_engine_runs_restart_the_rest() {
        let mut h = History::default();
        h.on_battery(0, 13_800, true, None);
        assert_eq!(h.last_run_ms(), Some(0));
        for k in 1..=60u64 {
            h.on_battery(k * 300_000, 12_450, false, None);
        }
        assert_eq!(
            h.estimate(60 * 300_000, false),
            (Some(75), Confidence::Rested)
        );
        h.on_engine_running(60 * 300_000 + 1);
        assert_eq!(h.last_run_ms(), Some(60 * 300_000 + 1));
        assert_eq!(
            h.estimate(60 * 300_000 + 2, false),
            (None, Confidence::None)
        );
        assert_eq!(h.samples.len(), 0);
    }

    #[test]
    fn history_samples_every_five_minutes_and_restarts_on_a_run() {
        let mut h = History::default();
        h.on_battery(0, 12_400, false, None);
        assert_eq!(
            h.estimate(10, false),
            (None, Confidence::None),
            "rest unknown since boot"
        );
        h.on_engine_running(1_000);
        for k in 0..60u64 {
            h.on_battery(1_000 + k * 100_000, 12_450, false, None);
        }
        assert_eq!(h.samples.len(), 20);
        assert_eq!(
            h.estimate(5 * 3_600_000, false),
            (Some(75), Confidence::Rested)
        );
        h.on_battery(5 * 3_600_000, 14_100, true, None);
        assert_eq!(
            h.estimate(5 * 3_600_000 + 1, false),
            (None, Confidence::None)
        );
    }
}
