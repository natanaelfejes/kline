//! The hardware the bridge needs, as traits. The ESP32-C3 firmware implements them with
//! esp-hal (`firmware/esp32c3/`); the host tests implement them over kline-sim.

use kline_core::{Action, Event};

use crate::gatt::Channel;
use crate::ratelimit::ClientId;

/// The port could not perform an action (UART fault, transceiver gone). The bridge drops the
/// session and backs off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("K-line port failure")]
pub struct PortError;

/// UART + K-line transceiver, performing kline-core actions.
///
/// On the ESP32-C3 with an L9637D (docs/bridge-hardware.md): `SetBreak(true)` detaches the UART
/// from the TX pin and drives it low (K low), `SetBreak(false)` drives it high and gives it back
/// to the UART at 10400 8N1; the 5-baud address byte is therefore bit-banged by the kline-core
/// init machine's 200 ms `Sleep`s. `CheckBreakLoopback` reports whether the RX pin read low
/// during the break (the L9637D RX follows K). `SetDtr` has no wire on the bridge and is
/// accepted as a no-op. `Read` is a total timeout, as on the host (docs/protocol.md "Sans-IO
/// contract").
pub trait KLinePort {
    fn perform(&mut self, action: &Action) -> Result<Event, PortError>;
    /// Power the transceiver and the switched K pull-up (`K_PWR`). Off = K released.
    fn set_power(&mut self, on: bool);
}

/// Monotonic milliseconds since boot.
pub trait Clock {
    fn now_ms(&self) -> u64;
    /// Block (or light-sleep) until `t_ms`. BLE keeps running on the radio task meanwhile.
    fn wait_until(&mut self, t_ms: u64);
}

/// One GPIO output. The fob output must read low after reset without software help (external
/// pull-down, docs/bridge-hardware.md): `set_low` is only the software half.
pub trait OutputPin {
    fn set_high(&mut self);
    fn set_low(&mut self);
}

/// BLE notifications. `to = None`: every subscribed client; `Some(c)`: client `c` only.
pub trait BleSink {
    fn notify(&mut self, to: Option<ClientId>, channel: Channel, frame: &[u8]);
}

/// Hardware watchdog. The runner feeds it once per step; a hung step resets the chip.
pub trait Watchdog {
    fn feed(&mut self);
}

/// Low-power entry.
pub trait Power {
    /// Sleep until a BLE connection, or `wake_after_ms` if given. Returns on wake-up. Must not
    /// busy-wait: light sleep (or deep sleep) in bounded slices that feed the watchdog.
    fn sleep(&mut self, wake_after_ms: Option<u64>, reason: crate::bridge::SleepReason);
}
