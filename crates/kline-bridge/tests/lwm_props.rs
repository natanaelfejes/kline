//! Lock-when-moving automaton properties (review B-3): whatever the BMT answers, nothing
//! stays stuck. After any history, once the engine stops (or the Sirius goes silent) the
//! unlock duty ends within the attempts, and a read showing the doors unlocked ends it at once;
//! a lock always starts with the full unlock attempts, and no lock follows a give-up until the
//! owner arms again.

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
    VerifiedUnlocked,
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
        1 => Just(Ev::VerifiedUnlocked),
    ]
}

fn outcome() -> impl Strategy<Value = Outcome> {
    prop_oneof![
        1 => Just(Outcome::Done),
        3 => Just(Outcome::NotExecuted),
        1 => Just(Outcome::MaybeExecuted)
    ]
}

/// Feeds an intent back at once with the next outcome; checks the per-intent invariants.
struct Driver {
    l: LockWhenMoving,
    outcomes: Vec<Outcome>,
    next: usize,
    t: u64,
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
        self.l.intent_done(self.t + 300, i, o);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn nothing_stays_stuck(
        evs in prop::collection::vec(ev(), 0..200),
        outcomes in prop::collection::vec(outcome(), 1..20),
        end_silent in any::<bool>(),
        end_by_read in any::<bool>(),
    ) {
        let mut d = Driver { l: LockWhenMoving::new(LwmConfig::default()), outcomes, next: 0, t: 0 };
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
                Ev::VerifiedUnlocked => d.l.on_verified_unlocked(),
            }
        }
        // The owner disarms; then either a lock-state read shows the doors unlocked, or the
        // engine stops (rpm 0) / the Sirius goes silent, and the BMT answers whatever it answers.
        d.l.disarm();
        if end_by_read && d.l.locked_by_us() {
            d.l.on_verified_unlocked();
            prop_assert!(!d.l.locked_by_us());
            prop_assert!(!d.l.gave_up());
        }
        let cfg = LwmConfig::default();
        let bound = d.t + cfg.silence_ms + (u64::from(cfg.unlock_attempts) + 1) * (cfg.spacing_ms + 1_000);
        while d.t < bound {
            d.t += 100;
            let i = if end_silent { d.l.on_sirius_failure(d.t) } else { d.l.on_sample(d.t, 0, 0) };
            d.handle(i);
            let i = d.l.on_tick(d.t);
            d.handle(i);
        }
        prop_assert!(!d.l.locked_by_us(), "unlock duty stuck after the owner disarmed");
    }
}
