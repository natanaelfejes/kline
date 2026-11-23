//! Drives a [`Bridge`] over the [`crate::hal`] traits: the firmware main loop, and the host
//! harness's loop.

use crate::bridge::{Bridge, Output};
use crate::fob::FobDriver;
use crate::hal::{BleSink, Clock, KLinePort, OutputPin, Power, Watchdog};

/// Everything but the fob pin, as one object (the firmware's board struct, or the test
/// harness).
pub trait BridgeIo: KLinePort + Clock + BleSink + Watchdog + Power {}

impl<T: KLinePort + Clock + BleSink + Watchdog + Power> BridgeIo for T {}

/// What one [`step`] did, for tests and logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stepped {
    KLine,
    Power(bool),
    Notify,
    Fob,
    Slept,
    Idle { until_ms: u64 },
}

/// Perform one bridge output. `Idle` is returned to the caller, who waits for input or for
/// `until_ms` (the firmware light-sleeps; the harness advances its clock).
pub fn step<IO: BridgeIo, P: OutputPin>(
    bridge: &mut Bridge,
    io: &mut IO,
    fob: &mut FobDriver<P>,
) -> Stepped {
    io.feed();
    let now = io.now_ms();
    match bridge.poll(now) {
        Output::KLine(action) => {
            match io.perform(&action) {
                Ok(ev) => bridge.on_kline_event(ev, io.now_ms()),
                Err(_) => bridge.on_port_error(io.now_ms()),
            }
            Stepped::KLine
        }
        Output::Power(on) => {
            io.set_power(on);
            Stepped::Power(on)
        }
        Output::Notify { to, channel, frame } => {
            io.notify(to, channel, &frame);
            Stepped::Notify
        }
        Output::FobPulse => {
            let r = fob.pulse(io);
            bridge.on_fob_done(r, io.now_ms());
            Stepped::Fob
        }
        Output::Sleep { wake_after_ms } => {
            io.set_power(false);
            io.sleep(wake_after_ms);
            bridge.on_wake(io.now_ms());
            Stepped::Slept
        }
        Output::Idle { until_ms } => Stepped::Idle { until_ms },
    }
}
