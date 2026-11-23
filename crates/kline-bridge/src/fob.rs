//! The fob output: a PhotoMOS relay across the button of remote B (MASTER-PLAN-v2 3.1 and
//! PLAN-RESEARCH B6), for locking and unlocking the parked car.
//!
//! Safety (MASTER-PLAN-v2 3.1 rule 3: a bridge failure must never lock anything):
//! - the output is driven low the moment the driver is made (boot), before anything else;
//! - a pulse only starts from [`FobDriver::pulse`], which the bridge calls only for an owner
//!   LOCK/UNLOCK command; boot, reconnects and resets never call it;
//! - the pulse is a fixed 300 ms and always ends low, also when the driver is dropped;
//! - at least 5 s between pulse starts;
//! - in hardware, an external pull-down keeps the PhotoMOS off while the GPIO floats during a
//!   reset or watchdog reset (docs/bridge-hardware.md).

use crate::hal::{Clock, OutputPin};

/// Pulse timing. MASTER-PLAN-v2 3.1: "Impulz 300 ms, potom 5 s pauza".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FobTiming {
    pub pulse_ms: u64,
    pub pause_ms: u64,
}

impl Default for FobTiming {
    fn default() -> Self {
        FobTiming {
            pulse_ms: 300,
            pause_ms: 5_000,
        }
    }
}

/// A pulse was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FobError {
    #[error("less than the pause since the last pulse")]
    TooSoon,
}

/// Drives the fob PhotoMOS. Owns the pin.
pub struct FobDriver<P: OutputPin> {
    pin: P,
    timing: FobTiming,
    last_start: Option<u64>,
}

impl<P: OutputPin> FobDriver<P> {
    /// Take the pin and drive it low at once.
    pub fn new(mut pin: P, timing: FobTiming) -> FobDriver<P> {
        pin.set_low();
        FobDriver {
            pin,
            timing,
            last_start: None,
        }
    }

    /// One press of the fob button: high for `pulse_ms`, then low. Blocks for the pulse.
    pub fn pulse(&mut self, clock: &mut impl Clock) -> Result<(), FobError> {
        let now = clock.now_ms();
        if self
            .last_start
            .is_some_and(|t| now.saturating_sub(t) < self.timing.pause_ms)
        {
            return Err(FobError::TooSoon);
        }
        self.last_start = Some(now);
        self.pin.set_high();
        clock.wait_until(now + self.timing.pulse_ms);
        self.pin.set_low();
        Ok(())
    }

    /// Whether a pulse would be accepted at `now_ms`.
    pub fn ready(&self, now_ms: u64) -> bool {
        self.last_start
            .is_none_or(|t| now_ms.saturating_sub(t) >= self.timing.pause_ms)
    }

    pub fn timing(&self) -> FobTiming {
        self.timing
    }
}

impl<P: OutputPin> Drop for FobDriver<P> {
    fn drop(&mut self) {
        self.pin.set_low();
    }
}
