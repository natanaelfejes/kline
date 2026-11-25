//! Power states of the bridge (owner decision 2026-11-25). The bridge is powered from the OBD
//! socket pin 16 (permanent 12 V): no ignition wire, no soldering in the car, no 32 kHz crystal
//! on the board for now.
//!
//! | State | When | What runs | Current (estimate, NOT VERIFIED) |
//! |---|---|---|---|
//! | Driving | the engine may run (drive profile) | CPU, BLE, K-line transceiver | 30 to 80 mA |
//! | Parked, awake | a client is connected or something is queued | CPU, BLE, BMT polling | 25 to 40 mA |
//! | Parked light sleep | no client, nothing to do, at most [`PARKED_LIGHT_MAX_MS`] after the engine last ran | light sleep, BLE advertising, wake every 60 s for the battery | about 2.3 mA without the crystal |
//! | Deep sleep | parked longer than that; below 12.0 V rested (hard off); below 11.8 V | timer wake every [`DEEP_WAKE_MS`], one ADC reading, no BLE | about 0.1 to 0.2 mA (chip about 5 uA plus regulator and divider) |
//!
//! From deep sleep the bridge comes back only when the battery reads at least [`START_MV`] in
//! [`START_SAMPLES`] wakes in a row (the engine started, or a charger), see [`DeepWake`]. A
//! wake from deep sleep resets the chip: the bridge starts fresh (lock-when-moving disarmed, the
//! battery estimate's rest time unknown until the engine runs again). A hardware supervisor (a
//! comparator that powers the board up at 13 V) can replace the timer wakes later.
//!
//! Watchdog against a stuck-awake bridge: with no client for [`STUCK_AWAKE_MS`] and not in the
//! drive profile, the bridge sleeps (`SleepReason::NoClient`), whatever profile a client
//! requested before it left. Never while lock-when-moving owes an unlock.

/// How the firmware sleeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepMode {
    /// Light sleep, BLE advertising, a connection or the timer wakes the bridge.
    LightAdvertising,
    /// Deep sleep with a timer wake every [`DEEP_WAKE_MS`] for one ADC reading, no BLE.
    DeepTimer,
}

/// 48 h of parked light sleep after the engine last ran.
pub const PARKED_LIGHT_MAX_MS: u64 = 48 * 3_600_000;
/// Deep-sleep timer wake.
pub const DEEP_WAKE_MS: u64 = 30_000;
/// The battery reading that ends a deep sleep (charging voltage, PLAN-RESEARCH B5).
pub const START_MV: u16 = 13_200;
/// That many deep-sleep wakes in a row at or above [`START_MV`].
pub const START_SAMPLES: u8 = 2;
/// The stuck-awake watchdog.
pub const STUCK_AWAKE_MS: u64 = 600_000;

/// The decision at each deep-sleep wake. The firmware keeps it in RTC memory across the
/// sleeps (the wake resets the chip).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeepWake {
    /// Wakes in a row at or above [`START_MV`].
    pub high: u8,
}

impl DeepWake {
    /// One wake's battery reading (None: not measured). True: resume full operation.
    pub fn on_wake(&mut self, mv: Option<u16>) -> bool {
        match mv {
            Some(v) if v >= START_MV => self.high = self.high.saturating_add(1),
            _ => self.high = 0,
        }
        self.high >= START_SAMPLES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_charging_wakes_in_a_row_resume() {
        let mut d = DeepWake::default();
        assert!(!d.on_wake(Some(12_400)));
        assert!(!d.on_wake(Some(13_400)));
        assert!(!d.on_wake(Some(12_900)), "not in a row");
        assert!(!d.on_wake(Some(13_300)));
        assert!(!d.on_wake(None), "a missing reading restarts the count");
        assert!(!d.on_wake(Some(13_300)));
        assert!(d.on_wake(Some(13_200)));
    }
}
