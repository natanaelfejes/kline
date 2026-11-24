//! Lock-when-moving automaton properties (reviews B-3 and B4-1), with the doors and the BMT
//! lock memory modelled in two ways: a memory that follows the doors, and a stale one that
//! never changes (the 2026-11-22 notes leave open whether it follows the K-line actuators).
//!
//! - An owed unlock ends only by a K-line unlock the BMT accepted, by the give-up, or while the
//!   doors really are unlocked: a stale "unlocked" read never ends it.
//! - Nothing stays stuck: after any history, once the owner disarms and the engine stops (or the
//!   Sirius goes silent), the duty ends within the unlock attempts.
//! - A lock always starts with the full unlock attempts, and no lock follows a give-up until the
//!   owner arms again; reads never end the give-up report.

use kline_bridge::lwm::{Intent, LockWhenMoving, LwmConfig, Outcome};
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Ev {
    Sample {
        speed: u8,
        rpm: u16,
        dt: u64,
    },
    Failure {
        dt: u64,
    },
    Tick {
        dt: u64,
    },
    /// Samples every 100 ms for `secs` seconds at this speed and rpm.
    Run {
        speed: u8,
        rpm: u16,
        secs: u64,
    },
    Arm,
    Disarm,
    /// A positive `21 A3` read of the lock memory.
    Read,
    /// The owner unlocks by hand (or with the fob).
    HandUnlock,
    Ack,
}

fn ev() -> impl Strategy<Value = Ev> {
    prop_oneof![
        8 => (prop_oneof![Just(0u8), Just(10), Just(40), any::<u8>()],
              prop_oneof![Just(0u16), Just(800), Just(2500)],
              prop_oneof![Just(100u64), Just(500), Just(3_000), 0..70_000u64])
            .prop_map(|(speed, rpm, dt)| Ev::Sample { speed, rpm, dt }),
        2 => (100..4_000u64).prop_map(|dt| Ev::Failure { dt }),
        2 => (100..10_000u64).prop_map(|dt| Ev::Tick { dt }),
        6 => (prop_oneof![Just((40u8, 2200u16)), Just((0, 0)), Just((0, 800))], 1..90u64)
            .prop_map(|((speed, rpm), secs)| Ev::Run { speed, rpm, secs }),
        1 => Just(Ev::Arm),
        1 => Just(Ev::Disarm),
        4 => Just(Ev::Read),
        1 => Just(Ev::HandUnlock),
        1 => Just(Ev::Ack),
    ]
}

fn outcome() -> impl Strategy<Value = Outcome> {
    prop_oneof![
        1 => Just(Outcome::Done),
        3 => Just(Outcome::NotExecuted),
        1 => Just(Outcome::MaybeExecuted)
    ]
}

/// The lock memory: follows the doors, or stays at a fixed value.
#[derive(Debug, Clone, Copy)]
enum Memory {
    Follows,
    Stale(bool),
}

/// Feeds intents back at once with the next outcome; models the doors; checks the invariants.
struct Driver {
    l: LockWhenMoving,
    outcomes: Vec<Outcome>,
    next: usize,
    t: u64,
    doors_locked: bool,
    memory: Memory,
}

impl Driver {
    fn handle(&mut self, i: Option<Intent>) {
        let Some(i) = i else { return };
        if i == Intent::Lock {
            assert!(self.l.is_armed(), "lock only while armed");
            assert!(
                !self.l.gave_up(),
                "no lock after a give-up before the owner arms"
            );
            assert_eq!(
                self.l.unlock_failures(),
                0,
                "a lock gets all unlock attempts"
            );
        }
        let o = self.outcomes[self.next % self.outcomes.len()];
        self.next += 1;
        let before = self.l.locked_by_us();
        self.l.intent_done(self.t + 300, i, o);
        match (i, o) {
            (Intent::Lock, Outcome::Done | Outcome::MaybeExecuted) => self.doors_locked = true,
            (Intent::Unlock, Outcome::Done) => self.doors_locked = false,
            _ => {}
        }
        // An unlock that did not execute ends the duty only by the give-up (a lock that was not
        // executed leaves nothing owed).
        if before && !self.l.locked_by_us() && i == Intent::Unlock && o != Outcome::Done {
            assert!(
                self.l.gave_up(),
                "duty ended by a failed unlock without a give-up"
            );
        }
    }

    fn read(&mut self) {
        let shown = match self.memory {
            Memory::Follows => self.doors_locked,
            Memory::Stale(v) => v,
        };
        let before = self.l.locked_by_us();
        let gave_up = self.l.gave_up();
        self.l.on_lock_read(shown);
        if before && !self.l.locked_by_us() {
            assert!(
                !self.doors_locked,
                "a read ended the duty while the doors are locked"
            );
        }
        assert_eq!(
            self.l.gave_up(),
            gave_up,
            "reads never change the give-up report"
        );
    }
}

fn memory() -> impl Strategy<Value = Memory> {
    prop_oneof![
        Just(Memory::Follows),
        Just(Memory::Stale(false)),
        Just(Memory::Stale(true))
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn nothing_stays_stuck_and_stale_reads_never_end_the_duty(
        evs in prop::collection::vec(ev(), 0..200),
        outcomes in prop::collection::vec(outcome(), 1..20),
        memory in memory(),
        end_silent in any::<bool>(),
    ) {
        let mut d = Driver {
            l: LockWhenMoving::new(LwmConfig::default()),
            outcomes,
            next: 0,
            t: 0,
            doors_locked: false,
            memory,
        };
        for e in evs {
            match e {
                Ev::Sample { speed, rpm, dt } => {
                    d.t += dt;
                    let i = d.l.on_sample(d.t, speed, rpm);
                    d.handle(i);
                }
                Ev::Failure { dt } => {
                    d.t += dt;
                    let i = d.l.on_sirius_failure(d.t);
                    d.handle(i);
                }
                Ev::Tick { dt } => {
                    d.t += dt;
                    let i = d.l.on_tick(d.t);
                    d.handle(i);
                }
                Ev::Run { speed, rpm, secs } => {
                    for _ in 0..secs * 10 {
                        d.t += 100;
                        let i = d.l.on_sample(d.t, speed, rpm);
                        d.handle(i);
                    }
                }
                Ev::Arm => d.l.arm(),
                Ev::Disarm => d.l.disarm(),
                Ev::Read => d.read(),
                Ev::HandUnlock => d.doors_locked = false,
                Ev::Ack => d.l.acknowledge_give_up(),
            }
        }
        // The owner disarms; the engine stops (rpm 0) or the Sirius goes silent, and the BMT
        // answers whatever it answers.
        d.l.disarm();
        let cfg = LwmConfig::default();
        let bound = d.t
            + cfg.silence_ms
            + (u64::from(cfg.unlock_attempts) + 1) * (cfg.spacing_ms + 1_000);
        while d.t < bound {
            d.t += 100;
            let i = if end_silent { d.l.on_sirius_failure(d.t) } else { d.l.on_sample(d.t, 0, 0) };
            d.handle(i);
            let i = d.l.on_tick(d.t);
            d.handle(i);
            d.read();
        }
        prop_assert!(!d.l.locked_by_us(), "unlock duty stuck after the owner disarmed");
    }
}
