//! Host harness: the bridge runs against kline-sim's in-process virtual K line with a Sirius
//! and a BMT on the same wire, a recording fob pin, and fake BLE clients.
#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;

use kline_bridge::bridge::{Bridge, BridgeConfig};
use kline_bridge::fob::{FobDriver, FobTiming};
use kline_bridge::gate::Role;
use kline_bridge::gatt::{Channel, Command, Op, Response, ResultCode};
use kline_bridge::hal::{BleSink, Clock, KLinePort, OutputPin, PortError, Power, Watchdog};
use kline_bridge::profile::EcuId;
use kline_bridge::ratelimit::ClientId;
use kline_bridge::runner::{self, Stepped};
use kline_core::safety::{self, Tier};
use kline_core::{Action, Event};
use kline_sim::bus::{BusConfig, Out, Responder, Wake};
use kline_sim::ecu::{EcuProfile, SpecEcu};
use kline_sim::journal::Journal;
use kline_sim::line::{Line, VirtualLine};
use kline_sim::scenario::Scenario;
use kline_spec::{EcuDef, Value};

pub const OWNER: ClientId = 1;
pub const AGENT: ClientId = 2;
pub const STRANGER: ClientId = 9;

pub fn fixture(name: &str) -> EcuDef {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    EcuDef::from_xml_bytes(&std::fs::read(&path).unwrap()).unwrap()
}

/// Sirius at idle: 850 rpm, standing, 14.3 V (raw 197), coolant 90 C (raw 208).
pub fn sirius_scenario() -> Scenario {
    Scenario::new()
        .number("Engine speed", 850.0)
        .number("Injection raw", 600.0)
        .number("Fuel raw", 96.0)
        .number("Battery raw", 197.0)
        .number("Pressure raw", 80.0)
        .number("Coolant raw", 208.0)
        .number("Air raw", 96.0)
        .number("Vehicle speed", 0.0)
}

/// BMT: doors closed, unlocked, 12.6 V (raw 164 x 0.077).
pub fn bmt_scenario(unlocked: bool) -> Scenario {
    Scenario::new()
        .text(
            "Door lock memory",
            if unlocked { "unlocked" } else { "locked" },
        )
        .number("Door bits", 0.0)
        .number("Supply raw", 164.0)
        .number("Status raw", 16.0)
        .number("Output state", 2.0)
}

pub fn sirius_ecu(s: Scenario) -> SpecEcu {
    SpecEcu::new(EcuProfile::sirius32(), fixture("bridge_sirius.xml"), s).unwrap()
}

pub fn bmt_ecu(s: Scenario) -> SpecEcu {
    SpecEcu::new(EcuProfile::bmt(), fixture("bridge_bmt.xml"), s).unwrap()
}

/// Both ECUs on one K line. Each ignores wake-ups for other addresses and bytes while asleep
/// (kline-sim `SpecEcu`), so the composite only routes timers.
pub struct TwoEcus {
    pub sirius: SpecEcu,
    pub bmt: SpecEcu,
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

/// One written K-line frame.
#[derive(Debug, Clone)]
pub struct Written {
    pub t_ms: u64,
    pub bytes: Vec<u8>,
}

/// The car side plus the BLE side of the bridge, as one `BridgeIo`.
pub struct Car {
    pub line: Rc<RefCell<VirtualLine>>,
    pub sirius_journal: Journal,
    pub bmt_journal: Journal,
    pub power: bool,
    pub power_log: Vec<(u64, bool)>,
    pub writes: Vec<Written>,
    pub notes: Vec<(u64, Option<ClientId>, Channel, Vec<u8>)>,
    pub sleeps: Vec<(u64, Option<u64>)>,
    pub watchdog_feeds: u64,
    /// While false, every K-line action fails as if the transceiver were not powered.
    pub cable_ok: bool,
}

impl Car {
    pub fn new(sirius: SpecEcu, bmt: SpecEcu) -> Car {
        let sj = sirius.journal();
        let bj = bmt.journal();
        let tol = sirius.profile().tolerance;
        let line = VirtualLine::new(
            TwoEcus { sirius, bmt },
            BusConfig::default(),
            tol,
            sj.clone(),
        );
        Car {
            line: Rc::new(RefCell::new(line)),
            sirius_journal: sj,
            bmt_journal: bj,
            power: false,
            power_log: Vec::new(),
            writes: Vec::new(),
            notes: Vec::new(),
            sleeps: Vec::new(),
            watchdog_feeds: 0,
            cable_ok: true,
        }
    }

    pub fn now(&self) -> u64 {
        self.line.borrow().now_ms()
    }

    /// Responses to `client`, decoded.
    pub fn responses(&self, client: ClientId) -> Vec<Response> {
        self.notes
            .iter()
            .filter(|(_, to, ch, _)| *to == Some(client) && *ch == Channel::Response)
            .map(|(_, _, _, f)| Response::decode(f).unwrap())
            .collect()
    }

    pub fn responses_for(&self, client: ClientId, counter: u32) -> Vec<Response> {
        self.responses(client)
            .into_iter()
            .filter(|r| r.counter == counter)
            .collect()
    }

    pub fn channel(&self, ch: Channel) -> Vec<Vec<u8>> {
        self.notes
            .iter()
            .filter(|(_, _, c, _)| *c == ch)
            .map(|(_, _, _, f)| f.clone())
            .collect()
    }

    /// Payloads of the written KWP frames (`[fmt][tgt][src][payload][cs]`), with the target.
    /// One-byte writes (the ~KW2 handshake) are skipped.
    pub fn written_payloads(&self) -> Vec<(u8, Vec<u8>)> {
        self.writes
            .iter()
            .filter(|w| w.bytes.len() > 1)
            .map(|w| {
                let b = &w.bytes;
                let n = (b[0] & 0x3F) as usize;
                assert!(n > 0, "length byte format not expected here: {b:02X?}");
                assert_eq!(b.len(), 3 + n + 1, "frame {b:02X?}");
                (b[1], b[3..3 + n].to_vec())
            })
            .collect()
    }
}

impl KLinePort for Car {
    fn perform(&mut self, action: &Action) -> Result<Event, PortError> {
        if !self.cable_ok {
            return Err(PortError);
        }
        assert!(
            self.power,
            "K-line action with the transceiver off: {action:?}"
        );
        if let Action::Write { frame, .. } = action {
            self.writes.push(Written {
                t_ms: self.now(),
                bytes: frame.bytes().to_vec(),
            });
        }
        self.line
            .borrow_mut()
            .perform(action)
            .map_err(|_| PortError)
    }
    fn set_power(&mut self, on: bool) {
        self.power = on;
        let t = self.now();
        self.power_log.push((t, on));
    }
}

impl Clock for Car {
    fn now_ms(&self) -> u64 {
        self.now()
    }
    fn wait_until(&mut self, t_ms: u64) {
        let now = self.now();
        if t_ms > now {
            self.line.borrow_mut().advance_ms(t_ms - now);
        }
    }
}

impl BleSink for Car {
    fn notify(&mut self, to: Option<ClientId>, channel: Channel, frame: &[u8]) {
        let t = self.now();
        self.notes.push((t, to, channel, frame.to_vec()));
    }
}

impl Watchdog for Car {
    fn feed(&mut self) {
        self.watchdog_feeds += 1;
    }
}

impl Power for Car {
    fn sleep(&mut self, wake_after_ms: Option<u64>) {
        let t = self.now();
        self.sleeps.push((t, wake_after_ms));
        self.wait_until(t + wake_after_ms.unwrap_or(1_000));
    }
}

/// Fob output that records every level change with the line's time.
#[derive(Clone)]
pub struct RecPin {
    pub line: Rc<RefCell<VirtualLine>>,
    pub log: Rc<RefCell<Vec<(u64, bool)>>>,
}

impl OutputPin for RecPin {
    fn set_high(&mut self) {
        let t = self.line.borrow().now_ms();
        self.log.borrow_mut().push((t, true));
    }
    fn set_low(&mut self) {
        let t = self.line.borrow().now_ms();
        self.log.borrow_mut().push((t, false));
    }
}

/// Everything a scenario needs.
pub struct Rig {
    pub bridge: Bridge,
    pub car: Car,
    pub fob: FobDriver<RecPin>,
    pub pin_log: Rc<RefCell<Vec<(u64, bool)>>>,
    counters: std::collections::BTreeMap<ClientId, u32>,
}

impl Rig {
    pub fn new(sirius: SpecEcu, bmt: SpecEcu) -> Rig {
        Rig::with_config(sirius, bmt, BridgeConfig::default())
    }

    pub fn with_config(sirius: SpecEcu, bmt: SpecEcu, cfg: BridgeConfig) -> Rig {
        let car = Car::new(sirius, bmt);
        let pin_log = Rc::new(RefCell::new(Vec::new()));
        let pin = RecPin {
            line: car.line.clone(),
            log: pin_log.clone(),
        };
        let fob = FobDriver::new(pin, FobTiming::default());
        let bridge = Bridge::new(cfg, car.now());
        Rig {
            bridge,
            car,
            fob,
            pin_log,
            counters: Default::default(),
        }
    }

    /// Default car: Sirius idling, BMT unlocked.
    pub fn default_car() -> Rig {
        Rig::new(sirius_ecu(sirius_scenario()), bmt_ecu(bmt_scenario(true)))
    }

    pub fn now(&self) -> u64 {
        self.car.now()
    }

    pub fn battery(&mut self, mv: u16) {
        let t = self.now();
        self.bridge.on_battery_mv(mv, t);
    }

    pub fn connect(&mut self, client: ClientId, role: Role) {
        let t = self.now();
        self.bridge.on_connect(client, role, t);
    }

    pub fn step(&mut self) -> Stepped {
        runner::step(&mut self.bridge, &mut self.car, &mut self.fob)
    }

    /// Run the loop until virtual time `t`.
    pub fn run_until(&mut self, t: u64) {
        let mut guard = 0u64;
        while self.now() < t {
            guard += 1;
            assert!(guard < 5_000_000, "runaway loop at {}", self.now());
            if let Stepped::Idle { until_ms } = self.step() {
                let to = until_ms.min(t).max(self.now() + 1);
                self.car.wait_until(to);
            }
        }
    }

    pub fn run_for(&mut self, ms: u64) {
        let t = self.now() + ms;
        self.run_until(t);
    }

    /// Run until `pred` holds or `max_ms` passed; returns whether it held.
    pub fn run_until_pred(&mut self, max_ms: u64, mut pred: impl FnMut(&Rig) -> bool) -> bool {
        let end = self.now() + max_ms;
        while self.now() < end {
            if pred(self) {
                return true;
            }
            if let Stepped::Idle { until_ms } = self.step() {
                let to = until_ms.min(end).max(self.now() + 1);
                self.car.wait_until(to);
            }
        }
        pred(self)
    }

    /// Next counter for `client`.
    pub fn next_counter(&mut self, client: ClientId) -> u32 {
        let c = self.counters.entry(client).or_insert(0);
        *c += 1;
        *c
    }

    /// Write a command frame; returns its counter.
    pub fn send(&mut self, client: ClientId, op: Op) -> u32 {
        let counter = self.next_counter(client);
        let frame = Command { counter, op }.encode().unwrap();
        let t = self.now();
        self.bridge.on_command(client, &frame, t);
        counter
    }

    pub fn send_raw(&mut self, client: ClientId, frame: &[u8]) {
        let t = self.now();
        self.bridge.on_command(client, frame, t);
    }

    /// Send and run until a final (non-Accepted) response for it arrives.
    pub fn call(&mut self, client: ClientId, op: Op, max_ms: u64) -> Response {
        let c = self.send(client, op);
        let ok = self.run_until_pred(max_ms, |r| {
            r.car
                .responses_for(client, c)
                .iter()
                .any(|x| x.result != ResultCode::Accepted)
        });
        assert!(ok, "no final response to counter {c} within {max_ms} ms");
        self.car
            .responses_for(client, c)
            .into_iter()
            .rfind(|x| x.result != ResultCode::Accepted)
            .unwrap()
    }

    /// Pin levels (true = high) since boot.
    pub fn pin_highs(&self) -> Vec<u64> {
        self.pin_log
            .borrow()
            .iter()
            .filter(|(_, h)| *h)
            .map(|(t, _)| *t)
            .collect()
    }

    /// Every written payload is Tier 0 for its target, or a protocol frame the session sends
    /// itself (StopCommunication `82`), unless it is in `approved`.
    pub fn assert_wire_tier0_except(&self, approved: &[(u8, Vec<u8>)]) {
        let mut approved: Vec<(u8, Vec<u8>)> = approved.to_vec();
        for (tgt, p) in self.car.written_payloads() {
            if let Some(i) = approved.iter().position(|a| *a == (tgt, p.clone())) {
                approved.remove(i);
                continue;
            }
            let ecu = EcuId::from_address(tgt).expect("frame for a known ECU");
            let t = kline_bridge::gate::target(ecu);
            let g = kline_bridge::gate::guard(ecu);
            assert!(
                p == [0x82] || safety::tier_of_in(&g, &t, &p) == Ok(Tier::Read),
                "non-read frame on the wire to {tgt:#04x}: {p:02X?}"
            );
        }
    }
}

pub fn text(s: &str) -> Value {
    Value::Text(s.into())
}
