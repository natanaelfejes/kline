//! The fob output never pulses by itself: not at boot, not on reconnects, not on a reset, and
//! a pulse always ends low (MASTER-PLAN-v2 3.1 rule 3).

mod common;

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;

use common::*;
use kline_bridge::fob::{FobDriver, FobError, FobTiming};
use kline_bridge::gate::Role;
use kline_bridge::gatt::Op;
use kline_bridge::hal::{Clock, OutputPin};
use kline_bridge::profile::Profile;

#[derive(Clone, Default)]
struct Pin(Rc<RefCell<Vec<bool>>>);

impl OutputPin for Pin {
    fn set_high(&mut self) {
        self.0.borrow_mut().push(true);
    }
    fn set_low(&mut self) {
        self.0.borrow_mut().push(false);
    }
}

struct FakeClock {
    t: u64,
    /// Panic inside `wait_until` (a watchdog reset in the middle of a pulse).
    die: bool,
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.t
    }
    fn wait_until(&mut self, t: u64) {
        if self.die {
            panic!("watchdog reset");
        }
        self.t = t;
    }
}

#[test]
fn boot_drives_the_output_low_before_anything_else() {
    let pin = Pin::default();
    let _fob = FobDriver::new(pin.clone(), FobTiming::default());
    assert_eq!(*pin.0.borrow(), vec![false]);
}

/// On the host a panic unwinds and `FobDriver`'s Drop drives the pin low; that is what this test
/// shows. On the target a panic does NOT unwind and no Drop runs: there the panic handler drives
/// the fob GPIO low first (`firmware/esp32c3/src/main.rs`) and the hardware pull-down holds it
/// during the reset (docs/bridge-hardware.md "Panic and reset").
#[test]
fn a_reset_in_the_middle_of_a_pulse_leaves_the_output_low() {
    let pin = Pin::default();
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut fob = FobDriver::new(pin.clone(), FobTiming::default());
        let mut clock = FakeClock { t: 0, die: true };
        let _ = fob.pulse(&mut clock);
    }));
    assert!(r.is_err());
    assert_eq!(pin.0.borrow().last(), Some(&false), "{:?}", pin.0.borrow());
    // After the reset: a new driver, low at once, nothing pending.
    let pin2 = Pin::default();
    let _fob = FobDriver::new(pin2.clone(), FobTiming::default());
    assert_eq!(*pin2.0.borrow(), vec![false]);
}

#[test]
fn pulses_are_300_ms_and_5_s_apart() {
    let pin = Pin::default();
    let mut fob = FobDriver::new(pin.clone(), FobTiming::default());
    let mut clock = FakeClock {
        t: 1_000,
        die: false,
    };
    fob.pulse(&mut clock).unwrap();
    assert_eq!(clock.t, 1_300);
    assert_eq!(*pin.0.borrow(), vec![false, true, false]);
    clock.t = 5_999;
    assert_eq!(fob.pulse(&mut clock), Err(FobError::TooSoon));
    clock.t = 6_000;
    fob.pulse(&mut clock).unwrap();
    assert_eq!(pin.0.borrow().len(), 5);
}

#[test]
fn a_long_bridge_run_with_resets_never_pulses_without_a_command() {
    // Boot, drive, park, lose the cable, reboot the bridge (new Bridge and FobDriver on the
    // same pin, like a watchdog reset), clients come and go: the pin never goes high.
    let mut rig = Rig::default_car();
    rig.connect(OWNER, Role::Owner);
    rig.connect(AGENT, Role::Agent);
    rig.battery(14_100);
    rig.run_until(20_000);
    rig.car.cable_ok = false;
    rig.run_until(25_000);
    rig.car.cable_ok = true;
    rig.battery(12_600);
    rig.send(OWNER, Op::StartLive(Profile::Parked));
    rig.send(AGENT, Op::YieldKline);
    rig.run_until(30_000);
    rig.send(AGENT, Op::ResumeKline);
    rig.run_until(40_000);

    // Reset: drop the driver (as the chip loses its state) and boot again.
    let log = rig.pin_log.clone();
    let line = rig.car.line.clone();
    let now = rig.now();
    rig.fob = FobDriver::new(
        RecPin {
            line,
            log: log.clone(),
        },
        FobTiming::default(),
    );
    rig.bridge = kline_bridge::Bridge::new(Default::default(), now);
    rig.connect(OWNER, Role::Owner);
    rig.battery(12_600);
    rig.run_until(80_000);
    rig.bridge.on_disconnect(OWNER, rig.now());
    rig.run_until(300_000);

    assert!(rig.pin_highs().is_empty(), "{:?}", log.borrow());
    assert!(log.borrow().iter().all(|(_, high)| !high));
}
