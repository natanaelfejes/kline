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
//!   after the pause, at most [`LwmConfig::unlock_attempts`] times; every lock starts with the
//!   full number of attempts (review B-3).
//! - ours: when the unlock attempts are used up the automaton gives up: it drops the duty,
//!   disarms itself and reports it ([`LockWhenMoving::gave_up`]) until the owner arms again or
//!   acknowledges it ([`LockWhenMoving::acknowledge_give_up`]). No lock follows a give-up
//!   without the owner.
//! - ours (review B4-1): the BMT lock memory may not follow the K-line `31 A0`/`31 A1` and its
//!   polarity is not verified, so a bare "unlocked" read proves nothing. Besides a positive
//!   reply to the unlock, the duty ends only on an observed change: after our lock completed, a
//!   read shows "locked" and a later read "unlocked" ([`LockWhenMoving::on_lock_read`]; a fob
//!   pulse that verifiably changed the bit is such a change).

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
    /// The unlock attempts were used up; cleared by [`LockWhenMoving::arm`] or
    /// [`LockWhenMoving::acknowledge_give_up`].
    gave_up: bool,
    /// A lock-state read showed "locked" since our lock completed.
    seen_locked: bool,
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
            gave_up: false,
            seen_locked: false,
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

    /// The owner armed it (two codes): this is also his acknowledgement of a give-up.
    pub fn arm(&mut self) {
        self.armed = true;
        self.gave_up = false;
        // A new arming may lock on the next drive-off, also after a give-up or a refused lock.
        self.relock_allowed = true;
    }

    /// The last unlock duty ended by giving up (the doors may still be locked); the automaton
    /// disarmed itself.
    pub fn gave_up(&self) -> bool {
        self.gave_up
    }

    /// No new locks. A pending unlock obligation stays (module rules).
    pub fn disarm(&mut self) {
        self.armed = false;
        self.moving_since = None;
    }

    /// The owner acknowledged a give-up (LWM_ACK): the report ends; nothing else changes.
    pub fn acknowledge_give_up(&mut self) {
        self.gave_up = false;
    }

    /// A positive BMT lock-state read (`21 A3`): `locked` as decoded. While an unlock is owed
    /// and no lock is being sent, a "locked" read followed by an "unlocked" read is an observed
    /// unlock (by hand or by the fob): the duty ends and a new drive-off may lock again. A bare
    /// "unlocked" read changes nothing (review B4-1).
    pub fn on_lock_read(&mut self, locked: bool) {
        if !self.locked_by_us || self.in_flight == Some(Intent::Lock) {
            return;
        }
        if locked {
            self.seen_locked = true;
        } else if self.seen_locked {
            self.locked_by_us = false;
            self.relock_allowed = true;
            self.unlock_failures = 0;
            self.seen_locked = false;
        }
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
            // This lock's unlock gets all its attempts (review B-3).
            self.unlock_failures = 0;
            // Only reads after this lock completed count.
            self.seen_locked = false;
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
                    // Give up; do not keep trying forever. No new lock until the owner arms
                    // again (review B-3); reported through `gave_up`.
                    self.locked_by_us = false;
                    self.armed = false;
                    self.moving_since = None;
                    self.gave_up = true;
                } else {
                    // Retry on the next tick after the pause, whatever the Sirius does.
                    self.sirius_gone = true;
                }
            }
        }
    }

    /// Unlock attempts that failed for the current (or last) lock.
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
        l.on_lock_read(true);
        l.on_lock_read(false);
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

    /// Lock, then an engine stop where every unlock fails: the automaton gives up.
    fn lock_then_give_up(l: &mut LockWhenMoving) -> u64 {
        assert_eq!(drive(l, 0, 5_000, 30, 1500).len(), 1);
        let mut t = 10_000;
        let mut n = 0;
        while t < 60_000 {
            if let Some(i) = l.on_sample(t, 0, 0) {
                assert_eq!(i, Intent::Unlock);
                l.intent_done(t + 300, i, Outcome::NotExecuted);
                n += 1;
            }
            t += 100;
        }
        assert_eq!(n, 3);
        t
    }

    /// Review B-3 (reviewer's break test): after one give-up, a later lock still gets its
    /// unlock.
    #[test]
    fn r3_unlock_after_give_up_then_new_drive() {
        let mut l = armed();
        let mut t = lock_then_give_up(&mut l);
        assert!(!l.locked_by_us());
        l.disarm();
        l.arm();
        while t < 200_000 {
            let _ = l.on_sample(t, 0, 0);
            t += 100;
        }
        assert_eq!(drive(&mut l, t, t + 20_000, 40, 2000).len(), 1, "locks");
        let t = t + 20_000;
        let v = drive(&mut l, t, t + 30_000, 0, 0);
        assert_eq!(
            v.iter().map(|(_, i)| *i).collect::<Vec<_>>(),
            vec![Intent::Unlock]
        );
        assert!(!l.locked_by_us());
    }

    /// Review B-3: a give-up disarms and is reported; no lock follows until the owner arms.
    #[test]
    fn a_give_up_disarms_until_the_owner_arms_again() {
        let mut l = armed();
        let t = lock_then_give_up(&mut l);
        assert!(l.gave_up());
        assert!(!l.is_armed());
        assert!(drive(&mut l, t, t + 30_000, 50, 2000).is_empty());
        l.arm();
        assert!(!l.gave_up());
        // The next drive-off locks, without a 60 s standstill first.
        let t = t + 30_000;
        assert!(drive(&mut l, t, t + 5_000, 0, 800).is_empty());
        assert_eq!(drive(&mut l, t + 5_000, t + 15_000, 50, 2000).len(), 1);
    }

    /// Review B4-1: only an observed locked -> unlocked change ends the duty; a bare
    /// "unlocked" read (stale lock memory) does not; reads while a lock is in flight do not
    /// count; a give-up report ends only by arming or the owner's acknowledgement.
    #[test]
    fn only_an_observed_unlock_ends_the_duty() {
        let mut l = armed();
        assert_eq!(drive(&mut l, 0, 5_000, 30, 1500).len(), 1);
        // Engine idling at a standstill: the duty stays...
        assert!(drive(&mut l, 5_000, 20_000, 0, 800).is_empty());
        // ...a bare "unlocked" read proves nothing...
        l.on_lock_read(false);
        l.on_lock_read(false);
        assert!(l.locked_by_us());
        // ...a "locked" read, then an "unlocked" one, is an observed unlock.
        l.on_lock_read(true);
        assert!(l.locked_by_us());
        l.on_lock_read(false);
        assert!(!l.locked_by_us());
        assert!(
            drive(&mut l, 20_000, 30_000, 0, 0).is_empty(),
            "nothing owed"
        );

        // Reads while the lock is in flight do not count.
        let mut l = armed();
        let mut t = 0;
        while l.on_sample(t, 40, 2000).is_none() {
            t += 100;
        }
        l.on_lock_read(true);
        l.on_lock_read(false);
        assert!(
            l.locked_by_us(),
            "the lock in flight may land after the reads"
        );
        l.intent_done(t + 300, Intent::Lock, Outcome::Done);
        l.on_lock_read(false);
        assert!(l.locked_by_us());

        // A "locked" read from an earlier lock does not carry over to a new one.
        let mut l = armed();
        assert_eq!(drive(&mut l, 0, 5_000, 30, 1500).len(), 1);
        l.on_lock_read(true);
        assert_eq!(
            drive(&mut l, 8_000, 9_000, 0, 0).len(),
            1,
            "unlocked by K-line"
        );
        assert!(!l.locked_by_us());
        assert!(drive(&mut l, 9_000, 75_000, 0, 800).is_empty());
        assert_eq!(
            drive(&mut l, 75_000, 85_000, 30, 1500).len(),
            1,
            "second lock"
        );
        l.on_lock_read(false);
        assert!(l.locked_by_us());

        // The give-up report.
        let mut l = armed();
        let _ = lock_then_give_up(&mut l);
        l.on_lock_read(true);
        l.on_lock_read(false);
        assert!(l.gave_up(), "reads never end the report");
        l.acknowledge_give_up();
        assert!(!l.gave_up());
        assert!(!l.is_armed(), "acknowledging does not arm");
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
