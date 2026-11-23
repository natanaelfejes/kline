//! Lock-when-moving as a pure automaton (MASTER-PLAN-v2 3.1 "Logika", decision D37).
//!
//! Inputs are Sirius samples (speed from `21 A9`, rpm), Sirius failures (the bridge tried the
//! Sirius and it did not answer), a clock tick and owner events. Output is at most one
//! [`Intent`] at a time; the bridge carries it out and reports back with
//! [`LockWhenMoving::intent_done`].
//!
//! Rules (all from MASTER-PLAN-v2 3.1 unless marked ours):
//! - Lock once when speed > 15 km/h for at least 3 s, from fresh samples only (ours: a gap of
//!   more than [`LwmConfig::sample_gap_ms`] between samples restarts the 3 s).
//! - Lock again only after a standstill (speed 0) longer than 60 s, or after the owner unlocked
//!   and the car drove off again; no repeated pulses at traffic lights.
//! - Unlock when rpm = 0 or the Sirius stopped answering (ignition off, engine stalled).
//!   ours: "stopped answering" means the bridge *tried* the Sirius and failed for
//!   [`LwmConfig::silence_ms`] with no sample in between ([`LockWhenMoving::on_sirius_failure`]).
//!   Mere absence of samples (the bridge on the BMT, a profile switch) is not silence.
//! - 5 s pause after every actuation.
//! - Off by default; arming is an owner action; it lasts until the owner disarms (owner
//!   decision 2026-11-23).
//! - ours: once the bridge has (possibly) locked, the unlock on engine stop happens even if the
//!   owner disarms in the meantime (the unlock is the accident mitigation of 3.1 rule 2).
//! - ours: a lock that was refused before or by the ECU is not retried; a lock that may have
//!   been executed (reply lost after the write) keeps the unlock duty. A failed unlock is retried
//!   after the pause, at most [`LwmConfig::unlock_attempts`] times.

/// Parameters, defaults from MASTER-PLAN-v2 3.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LwmConfig {
    /// Lock when the speed is above this (km/h). 3.1: 15.
    pub speed_kmh: u8,
    /// ... for at least this long. 3.1: 3 s.
    pub hold_ms: u64,
    /// Standstill (speed 0) after which a new lock is allowed. 3.1: 60 s.
    pub rearm_standstill_ms: u64,
    /// The Sirius counts as gone (ignition off) when the bridge's attempts to reach it have
    /// failed for this long. ours: 3 s.
    pub silence_ms: u64,
    /// Two samples further apart than this do not prove continuous movement. ours: 1.5 s, about
    /// seven `21 A9` periods of the drive profile.
    pub sample_gap_ms: u64,
    /// Minimum time between two actuations. 3.1: "Impulz 300 ms, potom 5 s pauza".
    pub spacing_ms: u64,
    /// Unlock attempts before giving up. ours.
    pub unlock_attempts: u8,
}

impl Default for LwmConfig {
    fn default() -> Self {
        LwmConfig {
            speed_kmh: 15,
            hold_ms: 3_000,
            rearm_standstill_ms: 60_000,
            silence_ms: 3_000,
            sample_gap_ms: 1_500,
            spacing_ms: 5_000,
            unlock_attempts: 3,
        }
    }
}

/// What the bridge should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Lock,
    Unlock,
}

/// How an intent ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The ECU accepted it (positive reply).
    Done,
    /// Nothing was written, or the ECU refused it (negative reply).
    NotExecuted,
    /// Written, but the reply is missing: it may have been executed.
    MaybeExecuted,
}

/// The automaton. Starts disarmed.
#[derive(Debug, Clone)]
pub struct LockWhenMoving {
    cfg: LwmConfig,
    armed: bool,
    locked_by_us: bool,
    relock_allowed: bool,
    moving_since: Option<u64>,
    stopped_since: Option<u64>,
    last_sample_ms: Option<u64>,
    /// First Sirius failure since the last sample.
    fail_since: Option<u64>,
    /// The failures have lasted `silence_ms`: the Sirius is gone.
    sirius_gone: bool,
    last_intent_ms: Option<u64>,
    in_flight: Option<Intent>,
    unlock_failures: u8,
}

impl LockWhenMoving {
    pub fn new(cfg: LwmConfig) -> LockWhenMoving {
        LockWhenMoving {
            cfg,
            armed: false,
            locked_by_us: false,
            relock_allowed: true,
            moving_since: None,
            stopped_since: None,
            last_sample_ms: None,
            fail_since: None,
            sirius_gone: false,
            last_intent_ms: None,
            in_flight: None,
            unlock_failures: 0,
        }
    }

    pub fn config(&self) -> &LwmConfig {
        &self.cfg
    }

    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// True while the doors are (possibly) locked by this automaton and not yet unlocked by it.
    pub fn locked_by_us(&self) -> bool {
        self.locked_by_us
    }

    pub fn arm(&mut self) {
        self.armed = true;
    }

    /// No new locks. A pending unlock obligation stays (module rules).
    pub fn disarm(&mut self) {
        self.armed = false;
        self.moving_since = None;
    }

    /// The owner unlocked by hand (app UNLOCK): a new drive-off may lock again.
    pub fn on_manual_unlock(&mut self) {
        self.relock_allowed = true;
    }

    /// A decoded Sirius sample at `now_ms`. Locks are decided here only, on fresh evidence.
    pub fn on_sample(&mut self, now_ms: u64, speed_kmh: u8, rpm: u16) -> Option<Intent> {
        let fresh = self
            .last_sample_ms
            .is_some_and(|t| now_ms.saturating_sub(t) <= self.cfg.sample_gap_ms);
        if !fresh {
            self.moving_since = None;
            self.stopped_since = None;
        }
        self.last_sample_ms = Some(now_ms);
        self.fail_since = None;
        self.sirius_gone = false;
        if speed_kmh > self.cfg.speed_kmh {
            self.moving_since.get_or_insert(now_ms);
            self.stopped_since = None;
        } else {
            self.moving_since = None;
            if speed_kmh == 0 {
                let since = *self.stopped_since.get_or_insert(now_ms);
                if now_ms.saturating_sub(since) > self.cfg.rearm_standstill_ms {
                    self.relock_allowed = true;
                }
            } else {
                self.stopped_since = None;
            }
        }
        if rpm == 0 && self.locked_by_us {
            return self.issue(now_ms, Intent::Unlock);
        }
        if self.armed && self.relock_allowed {
            if let Some(since) = self.moving_since {
                if now_ms.saturating_sub(since) >= self.cfg.hold_ms {
                    return self.issue(now_ms, Intent::Lock);
                }
            }
        }
        None
    }

    /// The bridge tried the Sirius (init or request) and got no answer.
    pub fn on_sirius_failure(&mut self, now_ms: u64) -> Option<Intent> {
        let since = *self.fail_since.get_or_insert(now_ms);
        self.moving_since = None;
        if now_ms.saturating_sub(since) >= self.cfg.silence_ms {
            self.sirius_gone = true;
        }
        self.on_tick(now_ms)
    }

    /// Clock tick (call it at least once per second): retries an owed unlock after the pause.
    pub fn on_tick(&mut self, now_ms: u64) -> Option<Intent> {
        if self.locked_by_us && self.sirius_gone {
            return self.issue(now_ms, Intent::Unlock);
        }
        None
    }

    fn issue(&mut self, now_ms: u64, intent: Intent) -> Option<Intent> {
        if self.in_flight.is_some() {
            return None;
        }
        if intent == Intent::Unlock && self.unlock_failures >= self.cfg.unlock_attempts {
            return None;
        }
        if self
            .last_intent_ms
            .is_some_and(|t| now_ms.saturating_sub(t) < self.cfg.spacing_ms)
        {
            return None;
        }
        self.last_intent_ms = Some(now_ms);
        self.in_flight = Some(intent);
        if intent == Intent::Lock {
            self.locked_by_us = true;
            self.relock_allowed = false;
        }
        Some(intent)
    }

    /// The bridge finished `intent` at `now_ms`.
    pub fn intent_done(&mut self, now_ms: u64, intent: Intent, outcome: Outcome) {
        if self.in_flight != Some(intent) {
            return;
        }
        self.in_flight = None;
        self.last_intent_ms = Some(now_ms);
        match (intent, outcome) {
            // Executed, or maybe executed: the doors may be locked, the unlock duty stays.
            (Intent::Lock, Outcome::Done | Outcome::MaybeExecuted) => {}
            // Not executed: not retried; nothing owed.
            (Intent::Lock, Outcome::NotExecuted) => self.locked_by_us = false,
            (Intent::Unlock, Outcome::Done) => {
                self.locked_by_us = false;
                self.relock_allowed = true;
                self.unlock_failures = 0;
            }
            (Intent::Unlock, Outcome::NotExecuted | Outcome::MaybeExecuted) => {
                self.unlock_failures = self.unlock_failures.saturating_add(1);
                if self.unlock_failures >= self.cfg.unlock_attempts {
                    // Give up; report it, do not keep trying forever.
                    self.locked_by_us = false;
                } else {
                    // Retry on the next tick after the pause, whatever the Sirius does.
                    self.sirius_gone = true;
                }
            }
        }
    }

    /// Unlock attempts that failed since the last good one.
    pub fn unlock_failures(&self) -> u8 {
        self.unlock_failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn armed() -> LockWhenMoving {
        let mut l = LockWhenMoving::new(LwmConfig::default());
        l.arm();
        l
    }

    /// Drive at `speed` from `from` to `to` in 100 ms samples; collect intents (each done OK
    /// 400 ms later).
    fn drive(
        l: &mut LockWhenMoving,
        from: u64,
        to: u64,
        speed: u8,
        rpm: u16,
    ) -> Vec<(u64, Intent)> {
        let mut v = Vec::new();
        let mut t = from;
        while t < to {
            if let Some(i) = l.on_sample(t, speed, rpm) {
                v.push((t, i));
                l.intent_done(t + 400, i, Outcome::Done);
            }
            t += 100;
        }
        v
    }

    /// The bridge fails to reach the Sirius every `every` ms from `from` to `to`.
    fn fail(l: &mut LockWhenMoving, from: u64, to: u64, every: u64) -> Vec<(u64, Intent)> {
        let mut v = Vec::new();
        let mut t = from;
        while t < to {
            if let Some(i) = l.on_sirius_failure(t) {
                v.push((t, i));
                l.intent_done(t + 400, i, Outcome::Done);
            }
            t += every;
        }
        v
    }

    #[test]
    fn disarmed_never_locks() {
        let mut l = LockWhenMoving::new(LwmConfig::default());
        assert!(drive(&mut l, 0, 60_000, 50, 2000).is_empty());
    }

    #[test]
    fn locks_after_three_seconds_above_fifteen() {
        let mut l = armed();
        assert!(
            drive(&mut l, 0, 10_000, 15, 1500).is_empty(),
            "15 is not above 15"
        );
        let v = drive(&mut l, 10_000, 20_000, 16, 1500);
        assert_eq!(v, vec![(13_000, Intent::Lock)]);
    }

    #[test]
    fn a_dip_restarts_the_hold() {
        let mut l = armed();
        assert!(drive(&mut l, 0, 2_900, 30, 1500).is_empty());
        assert!(drive(&mut l, 2_900, 3_000, 10, 1500).is_empty());
        let v = drive(&mut l, 3_000, 7_000, 30, 1500);
        assert_eq!(v, vec![(6_000, Intent::Lock)]);
    }

    /// Review M5: one sample above 15 km/h and then nothing (link trouble) must not lock; a
    /// gap in the samples restarts the hold.
    #[test]
    fn stale_speed_never_locks() {
        let mut l = armed();
        assert_eq!(l.on_sample(0, 40, 1500), None);
        for t in (100..10_000).step_by(100) {
            assert_eq!(l.on_tick(t), None);
        }
        // A sample 10 s later at speed: the 3 s count starts again here.
        assert_eq!(l.on_sample(10_000, 40, 1500), None);
        assert_eq!(
            l.on_sample(12_900, 40, 1500),
            None,
            "gap of 2.9 s: not continuous"
        );
        assert!(drive(&mut l, 13_000, 15_900, 40, 1500).is_empty());
        assert_eq!(
            drive(&mut l, 15_900, 16_000, 40, 1500),
            vec![(15_900, Intent::Lock)]
        );
    }

    #[test]
    fn no_relock_at_traffic_lights_only_after_long_standstill() {
        let mut l = armed();
        assert_eq!(drive(&mut l, 0, 5_000, 30, 1500).len(), 1);
        // 30 s at a light, drive on: no second lock.
        assert!(drive(&mut l, 5_000, 35_000, 0, 800).is_empty());
        assert!(drive(&mut l, 35_000, 45_000, 30, 1500).is_empty());
        // 61 s standstill (engine running), then drive: lock again.
        assert!(drive(&mut l, 45_000, 106_100, 0, 800).is_empty());
        let v = drive(&mut l, 106_100, 115_000, 30, 1500);
        assert_eq!(v, vec![(109_100, Intent::Lock)]);
    }

    #[test]
    fn manual_unlock_allows_relock_on_next_drive_off() {
        let mut l = armed();
        assert_eq!(drive(&mut l, 0, 5_000, 30, 1500).len(), 1);
        l.on_manual_unlock();
        assert!(drive(&mut l, 5_000, 8_000, 0, 800).is_empty());
        let v = drive(&mut l, 8_000, 15_000, 30, 1500);
        assert_eq!(v, vec![(11_000, Intent::Lock)]);
    }

    #[test]
    fn unlocks_on_engine_stop() {
        let mut l = armed();
        assert_eq!(drive(&mut l, 0, 5_000, 30, 1500).len(), 1);
        let v = drive(&mut l, 5_000, 12_000, 0, 0);
        assert_eq!(
            v,
            vec![(8_400, Intent::Unlock)],
            "5 s spacing after the lock done at 3400"
        );
        assert!(!l.locked_by_us());
    }

    /// Review B1: samples simply stopping (the bridge on the BMT, a profile switch) is not
    /// ignition off; only failed attempts for 3 s are.
    #[test]
    fn only_failed_attempts_count_as_silence() {
        let mut l = armed();
        assert_eq!(drive(&mut l, 0, 5_000, 30, 1500).len(), 1);
        for t in (5_000..60_000).step_by(100) {
            assert_eq!(l.on_tick(t), None, "no attempts, no silence, at {t}");
        }
        // Attempts failing for 2.9 s, then a sample: still nothing.
        assert!(fail(&mut l, 60_000, 63_000, 500).is_empty());
        assert!(drive(&mut l, 63_000, 63_100, 30, 1500).is_empty());
        // Failing for 3 s: unlock.
        assert_eq!(
            fail(&mut l, 64_000, 68_000, 500),
            vec![(67_000, Intent::Unlock)]
        );
    }

    /// Review M4: a lock written but not acknowledged keeps the unlock duty.
    #[test]
    fn a_maybe_executed_lock_keeps_the_unlock_duty() {
        let mut l = armed();
        let mut t = 0;
        let i = loop {
            if let Some(i) = l.on_sample(t, 40, 2000) {
                break i;
            }
            t += 100;
        };
        assert_eq!(i, Intent::Lock);
        l.intent_done(t + 500, Intent::Lock, Outcome::MaybeExecuted);
        assert!(l.locked_by_us());
        assert_eq!(
            drive(&mut l, 9_000, 9_100, 0, 0),
            vec![(9_000, Intent::Unlock)]
        );

        let mut l = armed();
        let mut t = 0;
        while l.on_sample(t, 40, 2000).is_none() {
            t += 100;
        }
        l.intent_done(t + 500, Intent::Lock, Outcome::NotExecuted);
        assert!(!l.locked_by_us());
    }

    #[test]
    fn unlock_obligation_survives_disarm() {
        let mut l = armed();
        assert_eq!(drive(&mut l, 0, 5_000, 30, 1500).len(), 1);
        l.disarm();
        assert_eq!(
            drive(&mut l, 9_000, 9_100, 0, 0),
            vec![(9_000, Intent::Unlock)]
        );
        assert!(
            drive(&mut l, 10_000, 30_000, 50, 2000).is_empty(),
            "disarmed: no relock"
        );
    }

    #[test]
    fn failed_unlock_is_retried_then_given_up() {
        let mut l = armed();
        assert_eq!(drive(&mut l, 0, 5_000, 30, 1500).len(), 1);
        let mut n = 0;
        let mut t = 10_000;
        while t < 60_000 {
            let i = l.on_sirius_failure(t);
            if let Some(i) = i {
                assert_eq!(i, Intent::Unlock);
                n += 1;
                l.intent_done(t, i, Outcome::NotExecuted);
            }
            t += 500;
        }
        assert_eq!(n, 3);
        assert!(!l.locked_by_us());
    }

    #[test]
    fn failed_lock_is_not_retried() {
        let mut l = armed();
        let mut t = 0;
        let mut locks = 0;
        while t < 30_000 {
            if let Some(i) = l.on_sample(t, 40, 2000) {
                assert_eq!(i, Intent::Lock);
                locks += 1;
                l.intent_done(t, i, Outcome::NotExecuted);
            }
            t += 100;
        }
        assert_eq!(locks, 1);
    }
}
