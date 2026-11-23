//! A `BleLink` to a kline-bridge `Bridge` that runs against kline-sim in the same process
//! (Sirius and BMT on one virtual K line). Shared by the kline-host and kline-cli BLE tests.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::io;

use kline_bridge::bridge::{Bridge, BridgeConfig};
use kline_bridge::fob::{FobDriver, FobTiming};
use kline_bridge::gate::Role;
use kline_bridge::gatt::Channel;
use kline_bridge::hal::{BleSink, Clock, KLinePort, OutputPin, PortError, Power, Watchdog};
use kline_bridge::ratelimit::ClientId;
use kline_bridge::runner::{self, Stepped};
use kline_core::{Action, Event};
use kline_host::ble::BleLink;
use kline_sim::bus::{BusConfig, Out, Responder, Wake};
use kline_sim::ecu::{EcuProfile, SpecEcu};
use kline_sim::journal::Journal;
use kline_sim::line::{Line, VirtualLine};
use kline_sim::scenario::Scenario;
use kline_spec::EcuDef;

pub const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../kline-bridge/tests/fixtures"
);

pub fn fixture(name: &str) -> EcuDef {
    EcuDef::from_xml_bytes(&std::fs::read(format!("{FIXTURES}/{name}")).unwrap()).unwrap()
}

struct TwoEcus {
    sirius: SpecEcu,
    bmt: SpecEcu,
}

impl Responder for TwoEcus {
    fn on_wake(&mut self, wake: Wake, out: &mut Out) {
        self.sirius.on_wake(wake, out);
        self.bmt.on_wake(wake, out);
    }
    fn on_byte(&mut self, t: u64, b: u8, out: &mut Out) {
        self.sirius.on_byte(t, b, out);
        self.bmt.on_byte(t, b, out);
    }
    fn next_timer(&self) -> Option<u64> {
        match (self.sirius.next_timer(), self.bmt.next_timer()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
    fn on_timer(&mut self, t: u64, out: &mut Out) {
        if self.sirius.next_timer().is_some_and(|n| n <= t) {
            self.sirius.on_timer(t, out);
        }
        if self.bmt.next_timer().is_some_and(|n| n <= t) {
            self.bmt.on_timer(t, out);
        }
    }
}

struct Io {
    line: VirtualLine,
    notes: Vec<(Option<ClientId>, Channel, Vec<u8>)>,
    writes: Vec<Vec<u8>>,
}

impl KLinePort for Io {
    fn perform(&mut self, action: &Action) -> Result<Event, PortError> {
        if let Action::Write { frame, .. } = action {
            self.writes.push(frame.bytes().to_vec());
        }
        self.line.perform(action).map_err(|_| PortError)
    }
    fn set_power(&mut self, _on: bool) {}
}

impl Clock for Io {
    fn now_ms(&self) -> u64 {
        self.line.now_ms()
    }
    fn wait_until(&mut self, t: u64) {
        let now = self.line.now_ms();
        if t > now {
            self.line.advance_ms(t - now);
        }
    }
}

impl BleSink for Io {
    fn notify(&mut self, to: Option<ClientId>, channel: Channel, frame: &[u8]) {
        self.notes.push((to, channel, frame.to_vec()));
    }
}

impl Watchdog for Io {
    fn feed(&mut self) {}
}

impl Power for Io {
    fn sleep(&mut self, wake_after_ms: Option<u64>, _reason: kline_bridge::bridge::SleepReason) {
        let t = self.now_ms() + wake_after_ms.unwrap_or(1_000);
        self.wait_until(t);
    }
}

struct NoPin;

impl OutputPin for NoPin {
    fn set_high(&mut self) {
        panic!("the fob output must not move in BLE transport tests");
    }
    fn set_low(&mut self) {}
}

/// The bridge, the car, and this link's identity (bond `client` with `role`).
pub struct SimBridge {
    bridge: Bridge,
    io: Io,
    fob: FobDriver<NoPin>,
    client: ClientId,
    inbox: VecDeque<(Channel, Vec<u8>)>,
    pub sirius_journal: Journal,
    pub bmt_journal: Journal,
}

impl SimBridge {
    pub fn new(role: Role) -> SimBridge {
        let sirius = SpecEcu::new(
            EcuProfile::sirius32(),
            fixture("bridge_sirius.xml"),
            Scenario::new()
                .number("Engine speed", 850.0)
                .number("Injection raw", 600.0)
                .number("Fuel raw", 96.0)
                .number("Battery raw", 197.0)
                .number("Pressure raw", 80.0)
                .number("Coolant raw", 208.0)
                .number("Air raw", 96.0)
                .number("Vehicle speed", 0.0),
        )
        .unwrap();
        let bmt = SpecEcu::new(
            EcuProfile::bmt(),
            fixture("bridge_bmt.xml"),
            Scenario::new()
                .text("Door lock memory", "unlocked")
                .number("Door bits", 0.0)
                .number("Supply raw", 164.0)
                .number("Status raw", 16.0)
                .number("Output state", 2.0),
        )
        .unwrap();
        let (sj, bj) = (sirius.journal(), bmt.journal());
        let tol = sirius.profile().tolerance;
        let line = VirtualLine::new(
            TwoEcus { sirius, bmt },
            BusConfig::default(),
            tol,
            sj.clone(),
        );
        let io = Io {
            line,
            notes: Vec::new(),
            writes: Vec::new(),
        };
        let mut bridge = Bridge::new(BridgeConfig::default(), 0);
        let client = 2;
        bridge.on_connect(client, role, 0);
        bridge.on_battery_mv(12_600, 0);
        SimBridge {
            bridge,
            io,
            fob: FobDriver::new(NoPin, FobTiming::default()),
            client,
            inbox: VecDeque::new(),
            sirius_journal: sj,
            bmt_journal: bj,
        }
    }

    /// Every K-line frame the bridge wrote.
    pub fn writes(&self) -> &[Vec<u8>] {
        &self.io.writes
    }
}

impl BleLink for SimBridge {
    fn write_command(&mut self, frame: &[u8]) -> io::Result<()> {
        let now = self.io.now_ms();
        self.bridge.on_command(self.client, frame, now);
        Ok(())
    }

    fn next_notification(&mut self, timeout_ms: u32) -> io::Result<Option<(Channel, Vec<u8>)>> {
        let end = self.io.now_ms() + u64::from(timeout_ms);
        loop {
            if let Some(n) = self.inbox.pop_front() {
                return Ok(Some(n));
            }
            if self.io.now_ms() >= end {
                return Ok(None);
            }
            if let Stepped::Idle { until_ms } =
                runner::step(&mut self.bridge, &mut self.io, &mut self.fob)
            {
                let to = until_ms.min(end).max(self.io.now_ms() + 1);
                self.io.wait_until(to);
            }
            let me = self.client;
            for (to, ch, f) in self.io.notes.drain(..) {
                if to.is_none() || to == Some(me) {
                    self.inbox.push_back((ch, f));
                }
            }
        }
    }
}
