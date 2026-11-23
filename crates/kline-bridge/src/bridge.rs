//! The bridge state machine: one K-line master, profiles, client jobs, keep-alive, reconnect,
//! sleep, lock-when-moving. Sans-IO: the caller (firmware loop or test harness, see
//! [`crate::runner`]) asks [`Bridge::poll`] what to do, performs it, and reports back.
//!
//! Contract:
//! - [`Output::KLine`]: perform the kline-core action, then call [`Bridge::on_kline_event`]
//!   with its event (or [`Bridge::on_port_error`]). No other `poll` happens in between.
//! - [`Output::FobPulse`]: pulse the fob output once ([`crate::fob::FobDriver::pulse`]), then
//!   call [`Bridge::on_fob_done`].
//! - [`Output::Power`], [`Output::Notify`]: perform, no answer.
//! - [`Output::Sleep`]: enter low power; call [`Bridge::on_wake`] on wake-up.
//! - [`Output::Idle`]: nothing to do until `until_ms` or the next input.
//!
//! Inputs at any time between actions: [`Bridge::on_connect`], [`Bridge::on_disconnect`],
//! [`Bridge::on_command`], [`Bridge::on_battery_mv`].
//!
//! There is only one K-line master: every request goes through the one [`Session`] the bridge
//! holds, a session for another ECU starts only after the previous one was stopped, and
//! `YIELD_KLINE` stops the session and switches the transceiver off (MASTER-PLAN-v2 P7, "one
//! tool on the cable").

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use core::mem;

use kline_core::frame::Reply;
use kline_core::init::FiveBaudInit;
use kline_core::session::{Session, SessionError, SessionOutcome};
use kline_core::{Action, Event, LinkError, Step};

use crate::fob::{FobError, FobTiming};
use crate::gate::{self, Role};
use crate::gatt::{
    live_flags, status_flags, BridgeState, Channel, Command, LiveSample, LockState, Op, RawDir,
    RawFrame, Response, ResultCode, Status,
};
use crate::live::{self, BmtFrame};
use crate::lwm::{Intent, LockWhenMoving, LwmConfig};
use crate::profile::{EcuId, Profile};
use crate::ratelimit::{ClientId, Counters, RateLimiter};

/// BMT actuator "lock all doors", 1 command: `31 A0 00 01` (WRITE-COMMANDS 1.5, decision D37).
pub const BMT_LOCK: [u8; 4] = [0x31, 0xA0, 0x00, 0x01];
/// BMT actuator "unlock all doors", 1 command: `31 A1 00 01`.
pub const BMT_UNLOCK: [u8; 4] = [0x31, 0xA1, 0x00, 0x01];
/// BMT lock-state read (`21 A3`, DCODCOND in data byte 0 bit 7).
const BMT_LOCK_READ: [u8; 2] = [0x21, 0xA3];

/// Tunables. Defaults cite their source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeConfig {
    pub fw_major: u8,
    pub fw_minor: u8,
    /// No writes and no LOCK/UNLOCK below this; sleep below it when the engine is off.
    /// MASTER-PLAN-v2 P6: 12.0 V.
    pub low_battery_mv: u16,
    /// Engine running / charging above this. PLAN-RESEARCH B5: 13.2 V.
    pub charging_mv: u16,
    /// Sleep after this long with no client, no profile and nothing to do. ours: 2 min.
    pub idle_sleep_ms: u64,
    /// Wake-up interval while asleep, to look at the battery. ours: 60 s idle, 1 h on low
    /// battery (PLAN-RESEARCH B8 "deep sleep with hourly wake").
    pub idle_wake_ms: u64,
    pub low_battery_wake_ms: u64,
    /// First wait after a failed init; doubles up to `backoff_max_ms`. ours (DDT2000 waits
    /// 500 ms between init rounds, docs/protocol.md "DDT2000 retry behaviour").
    pub backoff_ms: u64,
    pub backoff_max_ms: u64,
    /// Wait after an init attempt the ECU answered: P3max (docs/protocol.md "Wait after an
    /// answered attempt", kline-core `timing::consts::P3_MAX_MS`).
    pub answered_wait_ms: u64,
    /// Init failures in a row after which queued jobs for that ECU fail. ours (DDT2000: 3
    /// rounds).
    pub init_failures_before_job_error: u32,
    /// Exchange failures in a row after which the session is dropped and re-initialised. ours.
    pub exchange_failures_before_reinit: u32,
    /// A queued job older than this fails with `LinkError`. ours.
    pub job_timeout_ms: u64,
    /// A challenge not confirmed within this is dropped. ours: the CLI's `--confirm-timeout`
    /// default, 120 s.
    pub challenge_ttl_ms: u64,
    /// After a fob pulse, how long to look for the BMT lock bit to change. ours: the bit follows
    /// within ~0.5 s (BMT-ANALYSIS section 6); 4 s leaves margin.
    pub lock_verify_ms: u64,
    pub status_interval_ms: u64,
    /// Granularity of `Idle` when nothing is scheduled. ours.
    pub idle_poll_ms: u64,
    pub max_queue: usize,
    /// Commands per client-independent window. ours: 20 per second.
    pub command_rate_max: usize,
    pub command_rate_window_ms: u64,
    /// LOCK/UNLOCK: at most this many per window, at least the fob pause apart. ours: 6 per
    /// 10 min (MASTER-PLAN-v2 3.1 rule 4 "limit frekvencie").
    pub lock_rate_max: usize,
    pub lock_rate_window_ms: u64,
    pub lwm: LwmConfig,
    pub fob: FobTiming,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        BridgeConfig {
            fw_major: 0,
            fw_minor: 1,
            low_battery_mv: 12_000,
            charging_mv: 13_200,
            idle_sleep_ms: 120_000,
            idle_wake_ms: 60_000,
            low_battery_wake_ms: 3_600_000,
            backoff_ms: 500,
            backoff_max_ms: 10_000,
            answered_wait_ms: 5_000,
            init_failures_before_job_error: 3,
            exchange_failures_before_reinit: 3,
            job_timeout_ms: 20_000,
            challenge_ttl_ms: 120_000,
            lock_verify_ms: 4_000,
            status_interval_ms: 1_000,
            idle_poll_ms: 50,
            max_queue: 8,
            command_rate_max: 20,
            command_rate_window_ms: 1_000,
            lock_rate_max: 6,
            lock_rate_window_ms: 600_000,
            lwm: LwmConfig::default(),
            fob: FobTiming::default(),
        }
    }
}

/// What the caller must do next.
#[derive(Debug)]
pub enum Output {
    /// Perform on the K line, then [`Bridge::on_kline_event`].
    KLine(Action),
    /// Transceiver and switched K pull-up on (true) or off (false).
    Power(bool),
    /// Send a notification.
    Notify {
        to: Option<ClientId>,
        channel: Channel,
        frame: Vec<u8>,
    },
    /// Pulse the fob output once, then [`Bridge::on_fob_done`].
    FobPulse,
    /// Enter low power, then [`Bridge::on_wake`].
    Sleep { wake_after_ms: Option<u64> },
    /// Nothing to do until then (or the next input).
    Idle { until_ms: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FobPhase {
    /// Read `21 A3` (a value read after the command was accepted).
    ReadState,
    /// The state differs from the target: pulse next.
    NeedPulse,
    /// The pulse was handed out; waiting for `on_fob_done`.
    Pulsing,
    /// Re-reading `21 A3` until it shows the target state.
    Verify { deadline: u64 },
}

/// Why a pending challenge exists.
#[cfg(feature = "approvals")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    /// Send the request after the code is confirmed.
    Send,
    /// Arming lock-when-moving: confirm `31 A0 00 01`, then `31 A1 00 01`. Never sent here.
    #[cfg(feature = "standing-lock")]
    ArmLock,
    #[cfg(feature = "standing-lock")]
    ArmUnlock,
}

#[derive(Debug)]
enum JobKind {
    Request {
        ecu: EcuId,
        payload: Vec<u8>,
    },
    #[cfg(feature = "approvals")]
    Prepare {
        ecu: EcuId,
        payload: Vec<u8>,
        purpose: Purpose,
    },
    #[cfg(feature = "approvals")]
    Confirm {
        challenge: u32,
        code: String,
    },
    Fob {
        lock: bool,
        phase: FobPhase,
    },
    #[cfg(feature = "standing-lock")]
    Standing(Intent),
}

#[derive(Debug)]
struct Job {
    client: Option<ClientId>,
    counter: u32,
    kind: JobKind,
    since_ms: u64,
}

#[cfg(feature = "approvals")]
struct Pending {
    id: u32,
    client: ClientId,
    ecu: EcuId,
    payload: Vec<u8>,
    purpose: Purpose,
    challenge: kline_core::safety::ApprovalChallenge,
    expires_ms: u64,
}

/// The owner armed lock-when-moving: the risks he confirmed for the two requests.
#[cfg(feature = "standing-lock")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Standing {
    lock: Option<kline_core::safety::RiskInfo>,
    unlock: Option<kline_core::safety::RiskInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exch {
    /// A profile poll `21 lid`.
    Poll(u8),
    TesterPresent,
    Stop,
    /// The exchange of the job at the front of the queue.
    Job,
}

enum Link {
    Off,
    Backoff {
        ecu: EcuId,
        until: u64,
        failures: u32,
    },
    Init {
        ecu: EcuId,
        m: FiveBaudInit,
        failures: u32,
    },
    Ready {
        ecu: EcuId,
        s: Session,
        errors: u32,
    },
    Busy {
        ecu: EcuId,
        s: Session,
        errors: u32,
        op: Exch,
    },
}

#[derive(Debug, Default, Clone, Copy)]
struct Parked {
    lock: LockState,
    lock_at_ms: Option<u64>,
    front_open: bool,
    rear_open: bool,
    batt_cv: Option<u16>,
}

/// The bridge.
pub struct Bridge {
    cfg: BridgeConfig,
    link: Link,
    power: bool,
    /// An action or fob pulse was handed out and not answered yet.
    awaiting: bool,
    next_action: Option<Action>,
    out: VecDeque<Output>,
    requested: Profile,
    battery_mv: Option<u16>,
    clients: BTreeMap<ClientId, Role>,
    counters: Counters,
    command_rate: RateLimiter,
    lock_rate: RateLimiter,
    jobs: VecDeque<Job>,
    #[cfg(feature = "approvals")]
    pending: Option<Pending>,
    #[cfg(feature = "approvals")]
    next_challenge_id: u32,
    #[cfg(feature = "standing-lock")]
    standing: Option<Standing>,
    lwm: LockWhenMoving,
    last_lwm_tick: u64,
    live: LiveSample,
    parked: Parked,
    last_engine_sample: Option<(u64, u16)>,
    poll_index: usize,
    yielded: bool,
    asleep: bool,
    raw: bool,
    last_activity_ms: u64,
    last_status_ms: Option<u64>,
    last_status: Option<Status>,
    last_error: u8,
    unix_time: Option<(u64, u32)>,
    now_ms: u64,
}

impl Bridge {
    pub fn new(cfg: BridgeConfig, now_ms: u64) -> Bridge {
        Bridge {
            command_rate: RateLimiter::new(cfg.command_rate_max, cfg.command_rate_window_ms, 0),
            lock_rate: RateLimiter::new(
                cfg.lock_rate_max,
                cfg.lock_rate_window_ms,
                cfg.fob.pause_ms,
            ),
            lwm: LockWhenMoving::new(cfg.lwm),
            cfg,
            link: Link::Off,
            power: false,
            awaiting: false,
            next_action: None,
            out: VecDeque::new(),
            requested: Profile::Off,
            battery_mv: None,
            clients: BTreeMap::new(),
            counters: Counters::new(),
            jobs: VecDeque::new(),
            #[cfg(feature = "approvals")]
            pending: None,
            #[cfg(feature = "approvals")]
            next_challenge_id: 1,
            #[cfg(feature = "standing-lock")]
            standing: None,
            last_lwm_tick: now_ms,
            live: LiveSample::default(),
            parked: Parked::default(),
            last_engine_sample: None,
            poll_index: 0,
            yielded: false,
            asleep: false,
            raw: false,
            last_activity_ms: now_ms,
            last_status_ms: None,
            last_status: None,
            last_error: 0,
            unix_time: None,
            now_ms,
        }
    }

    /// The counters to save to NVS after a command (see [`Counters`]).
    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// Restore saved counters at boot.
    pub fn restore_counters(&mut self, c: Counters) {
        self.counters = c;
    }

    pub fn lwm(&self) -> &LockWhenMoving {
        &self.lwm
    }

    pub fn is_asleep(&self) -> bool {
        self.asleep
    }

    /// The ECU of the open session, if any.
    pub fn session_ecu(&self) -> Option<EcuId> {
        match &self.link {
            Link::Ready { ecu, .. } | Link::Busy { ecu, .. } => Some(*ecu),
            _ => None,
        }
    }

    /// The current status record.
    pub fn status(&self) -> Status {
        let state = if self.asleep {
            BridgeState::Sleep
        } else if self.yielded && matches!(self.link, Link::Off) {
            BridgeState::Yielded
        } else {
            match &self.link {
                Link::Off => BridgeState::Idle,
                Link::Backoff { .. } => BridgeState::Backoff,
                Link::Init { .. } => BridgeState::Init,
                Link::Ready { .. } | Link::Busy { .. } => BridgeState::Session,
            }
        };
        let ecu = match &self.link {
            Link::Init { ecu, .. } | Link::Ready { ecu, .. } | Link::Busy { ecu, .. } => {
                ecu.address()
            }
            _ => 0,
        };
        let mut flags = 0;
        if self.lwm.is_armed() {
            flags |= status_flags::LWM_ARMED;
        }
        if self.lwm.locked_by_us() {
            flags |= status_flags::LWM_LOCKED;
        }
        if self.engine_running(self.now_ms) {
            flags |= status_flags::ENGINE_RUNNING;
        }
        if cfg!(feature = "approvals") {
            flags |= status_flags::APPROVALS;
        }
        if self.parked.front_open {
            flags |= status_flags::DOOR_FRONT_OPEN;
        }
        if self.parked.rear_open {
            flags |= status_flags::DOOR_REAR_OPEN;
        }
        if !self.battery_ok() {
            flags |= status_flags::LOW_BATTERY;
        }
        Status {
            fw_major: self.cfg.fw_major,
            fw_minor: self.cfg.fw_minor,
            state,
            ecu,
            profile: self.effective_profile(self.now_ms),
            batt_cv: self.battery_mv.map_or(0, |mv| mv / 10),
            lock: self.parked.lock,
            flags,
            last_error: self.last_error,
            uptime_s: (self.now_ms / 1000) as u32,
        }
    }

    // -----------------------------------------------------------------------------------------
    // Inputs

    /// A bonded client connected with `role` (from the bond table, set at pairing).
    pub fn on_connect(&mut self, client: ClientId, role: Role, now_ms: u64) {
        self.now_ms = now_ms;
        self.clients.insert(client, role);
        self.last_activity_ms = now_ms;
    }

    pub fn on_disconnect(&mut self, client: ClientId, now_ms: u64) {
        self.now_ms = now_ms;
        self.clients.remove(&client);
        self.last_activity_ms = now_ms;
    }

    /// Battery voltage from the bridge's divider (PLAN-RESEARCH B5).
    pub fn on_battery_mv(&mut self, mv: u16, now_ms: u64) {
        self.now_ms = now_ms;
        self.battery_mv = Some(mv);
    }

    /// Woke up from [`Output::Sleep`].
    pub fn on_wake(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
        self.asleep = false;
        self.last_activity_ms = now_ms;
    }

    /// A frame written to the Command characteristic by `client`.
    pub fn on_command(&mut self, client: ClientId, frame: &[u8], now_ms: u64) {
        self.now_ms = now_ms;
        let role = self.clients.get(&client).copied().unwrap_or(Role::Unbonded);
        let cmd = match Command::decode(frame) {
            Ok(c) => c,
            Err(_) => return self.respond(Some(client), 0, ResultCode::BadFrame, Vec::new()),
        };
        let counter = cmd.counter;
        if role == Role::Unbonded {
            return self.respond(
                Some(client),
                counter,
                ResultCode::NotAllowedForRole,
                Vec::new(),
            );
        }
        if !self.counters.accept(client, counter) {
            return self.respond(Some(client), counter, ResultCode::StaleCounter, Vec::new());
        }
        if !self.command_rate.try_acquire(now_ms) {
            return self.respond(Some(client), counter, ResultCode::RateLimited, Vec::new());
        }
        self.last_activity_ms = now_ms;
        let r = |c| Err::<(), ResultCode>(c);
        let result: Result<(), ResultCode> = match cmd.op {
            Op::StartLive(p) => {
                self.requested = p;
                Ok(())
            }
            Op::StopLive => {
                self.requested = Profile::Off;
                Ok(())
            }
            Op::Request { ecu, payload } => gate::check_request(role, ecu, &payload)
                .and_then(|()| self.enqueue(client, counter, JobKind::Request { ecu, payload })),
            Op::Prepare { ecu, payload } => self.prepare(client, counter, role, ecu, payload),
            Op::Confirm { challenge, code } => self.confirm(client, counter, role, challenge, code),
            Op::Lock | Op::Unlock => {
                let lock = cmd.op == Op::Lock;
                if role != Role::Owner {
                    r(ResultCode::NotAllowedForRole)
                } else if !self.battery_ok() {
                    r(ResultCode::LowBattery)
                } else if self.ignition_on(now_ms) {
                    r(ResultCode::IgnitionOn)
                } else if self.yielded {
                    r(ResultCode::Yielded)
                } else if !self.lock_rate.try_acquire(now_ms) {
                    r(ResultCode::RateLimited)
                } else {
                    self.enqueue(
                        client,
                        counter,
                        JobKind::Fob {
                            lock,
                            phase: FobPhase::ReadState,
                        },
                    )
                }
            }
            // Not while lock-when-moving owes an unlock: it needs the K line at engine stop.
            Op::YieldKline if self.lwm.locked_by_us() => r(ResultCode::Busy),
            Op::YieldKline => {
                self.yielded = true;
                self.fail_jobs(|_| true, ResultCode::Yielded);
                Ok(())
            }
            Op::ResumeKline => {
                self.yielded = false;
                Ok(())
            }
            Op::SetTime(t) => {
                self.unix_time = Some((now_ms, t));
                Ok(())
            }
            Op::LwmArm => self.arm(client, counter, role),
            Op::LwmDisarm => {
                if role != Role::Owner {
                    r(ResultCode::NotAllowedForRole)
                } else {
                    self.lwm.disarm();
                    self.drop_standing_if_done();
                    Ok(())
                }
            }
            Op::RawStream(on) => {
                self.raw = on;
                Ok(())
            }
            Op::GetStatus => {
                let s = self.status().encode();
                self.respond(Some(client), counter, ResultCode::Ok, s);
                return;
            }
        };
        match result {
            Ok(()) => {
                let queued = self
                    .jobs
                    .back()
                    .is_some_and(|j| j.client == Some(client) && j.counter == counter);
                let code = if queued {
                    ResultCode::Accepted
                } else {
                    ResultCode::Ok
                };
                self.respond(Some(client), counter, code, Vec::new());
            }
            Err(c) => self.respond(Some(client), counter, c, Vec::new()),
        }
    }

    fn enqueue(&mut self, client: ClientId, counter: u32, kind: JobKind) -> Result<(), ResultCode> {
        if self.yielded {
            return Err(ResultCode::Yielded);
        }
        if self.jobs.len() >= self.cfg.max_queue {
            return Err(ResultCode::Busy);
        }
        self.jobs.push_back(Job {
            client: Some(client),
            counter,
            kind,
            since_ms: self.now_ms,
        });
        Ok(())
    }

    #[cfg(feature = "approvals")]
    fn prepare(
        &mut self,
        client: ClientId,
        counter: u32,
        role: Role,
        ecu: EcuId,
        payload: Vec<u8>,
    ) -> Result<(), ResultCode> {
        gate::check_prepare(role, ecu, &payload, self.battery_ok())?;
        if self.pending_live() {
            return Err(ResultCode::Busy);
        }
        self.enqueue(
            client,
            counter,
            JobKind::Prepare {
                ecu,
                payload,
                purpose: Purpose::Send,
            },
        )
    }

    #[cfg(not(feature = "approvals"))]
    fn prepare(
        &mut self,
        _client: ClientId,
        _counter: u32,
        role: Role,
        ecu: EcuId,
        payload: Vec<u8>,
    ) -> Result<(), ResultCode> {
        gate::check_prepare(role, ecu, &payload, self.battery_ok()).map(|_| ())
    }

    #[cfg(feature = "approvals")]
    fn confirm(
        &mut self,
        client: ClientId,
        counter: u32,
        role: Role,
        challenge: u32,
        code: String,
    ) -> Result<(), ResultCode> {
        if role != Role::Owner {
            return Err(ResultCode::NotAllowedForRole);
        }
        match &self.pending {
            Some(p) if p.client == client && p.id == challenge && self.now_ms < p.expires_ms => {}
            _ => {
                // A wrong or late answer closes any pending challenge (one try per code).
                self.pending = None;
                return Err(ResultCode::ApprovalMismatch);
            }
        }
        self.enqueue(client, counter, JobKind::Confirm { challenge, code })
    }

    #[cfg(not(feature = "approvals"))]
    fn confirm(
        &mut self,
        _client: ClientId,
        _counter: u32,
        role: Role,
        _challenge: u32,
        _code: String,
    ) -> Result<(), ResultCode> {
        if role != Role::Owner {
            return Err(ResultCode::NotAllowedForRole);
        }
        Err(ResultCode::Unsupported)
    }

    #[cfg(feature = "standing-lock")]
    fn arm(&mut self, client: ClientId, counter: u32, role: Role) -> Result<(), ResultCode> {
        if role != Role::Owner {
            return Err(ResultCode::NotAllowedForRole);
        }
        if !self.battery_ok() {
            return Err(ResultCode::LowBattery);
        }
        if self.pending_live() {
            return Err(ResultCode::Busy);
        }
        self.enqueue(
            client,
            counter,
            JobKind::Prepare {
                ecu: EcuId::Bmt,
                payload: BMT_LOCK.to_vec(),
                purpose: Purpose::ArmLock,
            },
        )
    }

    #[cfg(not(feature = "standing-lock"))]
    fn arm(&mut self, _client: ClientId, _counter: u32, role: Role) -> Result<(), ResultCode> {
        if role != Role::Owner {
            return Err(ResultCode::NotAllowedForRole);
        }
        Err(ResultCode::Unsupported)
    }

    #[cfg(feature = "approvals")]
    fn pending_live(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| self.now_ms < p.expires_ms)
    }

    fn drop_standing_if_done(&mut self) {
        #[cfg(feature = "standing-lock")]
        if !self.lwm.is_armed() && !self.lwm.locked_by_us() {
            self.standing = None;
        }
    }

    /// The K-line event answering the last [`Output::KLine`].
    pub fn on_kline_event(&mut self, ev: Event, now_ms: u64) {
        self.now_ms = now_ms;
        self.awaiting = false;
        if self.raw {
            if let Event::ReadOk(b) | Event::ReadTimeout(b) = &ev {
                if !b.is_empty() {
                    self.raw_frame(RawDir::Rx, b.clone());
                }
            }
        }
        match mem::replace(&mut self.link, Link::Off) {
            Link::Init {
                ecu,
                mut m,
                failures,
            } => match m.on_event(ev) {
                Step::Act(a) => {
                    self.link = Link::Init { ecu, m, failures };
                    self.next_action = Some(a);
                }
                Step::Done(outcome) => {
                    let mut s = Session::new(outcome, now_ms);
                    s.set_definition_guard(gate::guard(ecu));
                    self.poll_index = 0;
                    self.link = Link::Ready { ecu, s, errors: 0 };
                }
                Step::Failed(e) => self.init_failed(ecu, failures + 1, &e),
            },
            Link::Busy {
                ecu,
                mut s,
                errors,
                op,
            } => match s.on_event(ev, now_ms) {
                Step::Act(a) => {
                    self.link = Link::Busy { ecu, s, errors, op };
                    self.next_action = Some(a);
                }
                Step::Done(outcome) => {
                    if matches!(outcome, SessionOutcome::Stopped(_)) || s.is_closed() {
                        self.link = Link::Off;
                    } else {
                        self.link = Link::Ready { ecu, s, errors: 0 };
                    }
                    self.finished(ecu, op, Ok(outcome));
                }
                Step::Failed(e) => {
                    let errors = errors + 1;
                    if op == Exch::Stop || s.is_closed() {
                        self.link = Link::Off;
                    } else if errors >= self.cfg.exchange_failures_before_reinit {
                        // The line is not trustworthy: drop the session, re-init after a pause
                        // (the ECU sleeps after P3max of silence anyway).
                        self.link = Link::Backoff {
                            ecu,
                            until: now_ms + self.cfg.backoff_ms,
                            failures: 0,
                        };
                    } else {
                        self.link = Link::Ready { ecu, s, errors };
                    }
                    self.last_error = ResultCode::LinkError as u8;
                    self.finished(ecu, op, Err(e));
                }
            },
            other => {
                // An event without an outstanding action: ignore it, keep the state.
                self.link = other;
            }
        }
    }

    /// The port failed while performing the last action: drop the link and back off.
    pub fn on_port_error(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
        self.awaiting = false;
        self.next_action = None;
        let ecu = match &self.link {
            Link::Init { ecu, .. } | Link::Ready { ecu, .. } | Link::Busy { ecu, .. } => Some(*ecu),
            Link::Backoff { ecu, .. } => Some(*ecu),
            Link::Off => None,
        };
        let busy_job = matches!(self.link, Link::Busy { op: Exch::Job, .. });
        self.link = match ecu {
            Some(ecu) => Link::Backoff {
                ecu,
                until: now_ms + self.cfg.backoff_ms,
                failures: 1,
            },
            None => Link::Off,
        };
        self.last_error = ResultCode::LinkError as u8;
        if busy_job {
            if let Some(ecu) = ecu {
                self.job_exchange_failed(ecu);
            }
        }
    }

    /// The fob pulse was performed (`Ok`) or refused by the driver.
    pub fn on_fob_done(&mut self, result: Result<(), FobError>, now_ms: u64) {
        self.now_ms = now_ms;
        self.awaiting = false;
        let Some(job) = self.jobs.front_mut() else {
            return;
        };
        if let JobKind::Fob { phase, .. } = &mut job.kind {
            if *phase == FobPhase::Pulsing {
                match result {
                    Ok(()) => {
                        *phase = FobPhase::Verify {
                            deadline: now_ms + self.cfg.lock_verify_ms,
                        }
                    }
                    Err(FobError::TooSoon) => self.finish_job(ResultCode::RateLimited, Vec::new()),
                }
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // poll

    /// What to do next.
    pub fn poll(&mut self, now_ms: u64) -> Output {
        self.now_ms = now_ms;
        if let Some(o) = self.out.pop_front() {
            return o;
        }
        if self.awaiting {
            // Misuse: an action is outstanding. Do nothing until its event arrives.
            return Output::Idle {
                until_ms: now_ms + self.cfg.idle_poll_ms,
            };
        }
        if let Some(a) = self.next_action.take() {
            return self.hand_out(a);
        }
        self.housekeeping(now_ms);
        if let Some(o) = self.out.pop_front() {
            return o;
        }
        // Each pass either returns an output or changes the link state; bound the loop anyway.
        for _ in 0..8 {
            if let Some(o) = self.step_link(now_ms) {
                return o;
            }
            if let Some(o) = self.out.pop_front() {
                return o;
            }
        }
        Output::Idle {
            until_ms: now_ms + self.cfg.idle_poll_ms,
        }
    }

    fn hand_out(&mut self, a: Action) -> Output {
        self.awaiting = true;
        if self.raw {
            if let Action::Write { frame, .. } = &a {
                self.raw_frame(RawDir::Tx, frame.bytes().to_vec());
                // The raw notification goes out after this action; order is kept by t_ms.
            }
        }
        Output::KLine(a)
    }

    fn housekeeping(&mut self, now: u64) {
        // Expired challenge.
        #[cfg(feature = "approvals")]
        if self.pending.as_ref().is_some_and(|p| now >= p.expires_ms) {
            self.pending = None;
        }
        // Expired jobs (not the one whose exchange is running).
        let busy = matches!(self.link, Link::Busy { op: Exch::Job, .. }) || self.awaiting;
        let timeout = self.cfg.job_timeout_ms;
        let mut i = 0;
        while i < self.jobs.len() {
            let running = i == 0 && busy;
            if !running && now.saturating_sub(self.jobs[i].since_ms) >= timeout {
                let job = self.jobs.remove(i).expect("index in range");
                self.job_failed(job, ResultCode::LinkError);
            } else {
                i += 1;
            }
        }
        // Lock-when-moving tick.
        if now.saturating_sub(self.last_lwm_tick) >= 200 {
            self.last_lwm_tick = now;
            if let Some(intent) = self.lwm.on_tick(now) {
                self.lwm_intent(intent);
            }
        }
        // Status notification on change or every interval.
        let st = self.status();
        let due = self
            .last_status_ms
            .is_none_or(|t| now.saturating_sub(t) >= self.cfg.status_interval_ms);
        if due || self.last_status.is_some_and(|s| !same_status(&s, &st)) {
            self.last_status_ms = Some(now);
            self.last_status = Some(st);
            if !self.clients.is_empty() {
                self.out.push_back(Output::Notify {
                    to: None,
                    channel: Channel::Status,
                    frame: st.encode(),
                });
            }
        }
    }

    fn step_link(&mut self, now: u64) -> Option<Output> {
        let desired = self.desired_ecu(now);
        match mem::replace(&mut self.link, Link::Off) {
            Link::Off => match desired {
                Some(ecu) => {
                    if !self.power {
                        self.power = true;
                        return Some(Output::Power(true));
                    }
                    if !self.battery_allows_session() {
                        return Some(self.idle(now));
                    }
                    Some(self.start_init(ecu, 0))
                }
                None => {
                    if self.power {
                        self.power = false;
                        return Some(Output::Power(false));
                    }
                    if let Some(wake) = self.should_sleep(now) {
                        self.fail_jobs(|_| true, ResultCode::LowBattery);
                        self.asleep = true;
                        let st = self.status();
                        self.last_status = Some(st);
                        if !self.clients.is_empty() {
                            self.out.push_back(Output::Notify {
                                to: None,
                                channel: Channel::Status,
                                frame: st.encode(),
                            });
                        }
                        self.out.push_back(Output::Sleep {
                            wake_after_ms: Some(wake),
                        });
                        return None;
                    }
                    Some(self.idle(now))
                }
            },
            Link::Backoff {
                ecu,
                until,
                failures,
            } => {
                if desired != Some(ecu) {
                    self.link = Link::Off;
                    return None;
                }
                if now >= until {
                    return Some(self.start_init(ecu, failures));
                }
                self.link = Link::Backoff {
                    ecu,
                    until,
                    failures,
                };
                Some(Output::Idle { until_ms: until })
            }
            Link::Ready { ecu, mut s, errors } => {
                if s.is_closed() {
                    return None;
                }
                if desired != Some(ecu) {
                    return match s.stop(now) {
                        Ok(a) => {
                            self.link = Link::Busy {
                                ecu,
                                s,
                                errors,
                                op: Exch::Stop,
                            };
                            Some(self.hand_out(a))
                        }
                        Err(_) => None,
                    };
                }
                self.next_op(ecu, s, errors, now)
            }
            other @ (Link::Init { .. } | Link::Busy { .. }) => {
                self.link = other;
                Some(Output::Idle {
                    until_ms: now + self.cfg.idle_poll_ms,
                })
            }
        }
    }

    fn idle(&self, now: u64) -> Output {
        Output::Idle {
            until_ms: now + self.cfg.idle_poll_ms,
        }
    }

    fn start_init(&mut self, ecu: EcuId, failures: u32) -> Output {
        let cfg = ecu.link_config();
        match FiveBaudInit::new(&cfg) {
            Ok(mut m) => {
                let a = m.start();
                self.link = Link::Init { ecu, m, failures };
                self.hand_out(a)
            }
            // Only for an aliased address (bit 7), impossible for the two fixed addresses.
            Err(_) => {
                self.link = Link::Off;
                self.idle(self.now_ms)
            }
        }
    }

    fn init_failed(&mut self, ecu: EcuId, failures: u32, e: &LinkError) {
        self.last_error = ResultCode::LinkError as u8;
        let wait = if ecu_answered(e) {
            self.cfg.answered_wait_ms
        } else {
            let shift = failures.saturating_sub(1).min(16);
            (self.cfg.backoff_ms << shift).min(self.cfg.backoff_max_ms)
        };
        self.link = Link::Backoff {
            ecu,
            until: self.now_ms + wait,
            failures,
        };
        if failures >= self.cfg.init_failures_before_job_error {
            self.fail_jobs(|k| job_ecu_of(k) == Some(ecu), ResultCode::LinkError);
        }
    }

    /// The next exchange in an idle session on `ecu`.
    fn next_op(&mut self, ecu: EcuId, mut s: Session, errors: u32, now: u64) -> Option<Output> {
        // 1. The job at the front, if it is for this ECU.
        if self
            .jobs
            .front()
            .is_some_and(|j| self.job_ecu(&j.kind) == Some(ecu))
        {
            return match self.start_job(ecu, &mut s, now) {
                JobStart::Exchange(a) => {
                    self.link = Link::Busy {
                        ecu,
                        s,
                        errors,
                        op: Exch::Job,
                    };
                    Some(self.hand_out(a))
                }
                JobStart::Pulse => {
                    self.link = Link::Ready { ecu, s, errors };
                    self.awaiting = true;
                    Some(Output::FobPulse)
                }
                JobStart::Nothing => {
                    self.link = Link::Ready { ecu, s, errors };
                    None
                }
            };
        }
        // 2. The profile's next poll.
        let profile = self.effective_profile(now);
        let schedule = if profile.ecu() == Some(ecu) {
            profile.schedule()
        } else {
            &[]
        };
        if !schedule.is_empty() {
            let lid = schedule[self.poll_index % schedule.len()];
            self.poll_index = self.poll_index.wrapping_add(1);
            if let Ok(a) = s.request(&[0x21, lid], now) {
                self.link = Link::Busy {
                    ecu,
                    s,
                    errors,
                    op: Exch::Poll(lid),
                };
                return Some(self.hand_out(a));
            }
        }
        // 3. Keep-alive.
        if s.tester_present_due(now) {
            if let Ok(a) = s.tester_present(now) {
                self.link = Link::Busy {
                    ecu,
                    s,
                    errors,
                    op: Exch::TesterPresent,
                };
                return Some(self.hand_out(a));
            }
        }
        self.link = Link::Ready { ecu, s, errors };
        Some(self.idle(now))
    }

    #[cfg_attr(not(feature = "approvals"), allow(unused_variables))]
    fn start_job(&mut self, ecu: EcuId, s: &mut Session, now: u64) -> JobStart {
        let Some(job) = self.jobs.front_mut() else {
            return JobStart::Nothing;
        };
        match &mut job.kind {
            JobKind::Request { payload, .. } => {
                let payload = payload.clone();
                match s.request(&payload, now) {
                    Ok(a) => JobStart::Exchange(a),
                    Err(e) => {
                        let code = session_error_code(&e);
                        self.finish_job(code, Vec::new());
                        JobStart::Nothing
                    }
                }
            }
            #[cfg(feature = "approvals")]
            JobKind::Prepare {
                payload, purpose, ..
            } => {
                let (payload, purpose) = (payload.clone(), *purpose);
                self.make_challenge(ecu, s, payload, purpose);
                JobStart::Nothing
            }
            #[cfg(feature = "approvals")]
            JobKind::Confirm { challenge, code } => {
                let (challenge, code) = (*challenge, code.clone());
                self.confirm_now(s, challenge, &code, now)
            }
            JobKind::Fob { lock, phase } => {
                let lock = *lock;
                match *phase {
                    FobPhase::ReadState => match s.request(&BMT_LOCK_READ, now) {
                        Ok(a) => JobStart::Exchange(a),
                        Err(_) => {
                            self.finish_job(ResultCode::LockNotConfirmed, Vec::new());
                            JobStart::Nothing
                        }
                    },
                    FobPhase::NeedPulse => {
                        *phase = FobPhase::Pulsing;
                        JobStart::Pulse
                    }
                    FobPhase::Pulsing => JobStart::Nothing,
                    FobPhase::Verify { deadline } => {
                        let target = if lock {
                            LockState::Locked
                        } else {
                            LockState::Unlocked
                        };
                        if self.parked.lock == target {
                            if !lock {
                                self.lwm.on_manual_unlock();
                            }
                            self.finish_job(ResultCode::Ok, Vec::new());
                            return JobStart::Nothing;
                        }
                        if now >= deadline {
                            self.finish_job(ResultCode::LockNotConfirmed, Vec::new());
                            return JobStart::Nothing;
                        }
                        match s.request(&BMT_LOCK_READ, now) {
                            Ok(a) => JobStart::Exchange(a),
                            Err(_) => {
                                self.finish_job(ResultCode::LockNotConfirmed, Vec::new());
                                JobStart::Nothing
                            }
                        }
                    }
                }
            }
            #[cfg(feature = "standing-lock")]
            JobKind::Standing(intent) => {
                let intent = *intent;
                self.standing_send(s, intent, now)
            }
        }
    }

    #[cfg(feature = "approvals")]
    fn make_challenge(&mut self, ecu: EcuId, s: &mut Session, payload: Vec<u8>, purpose: Purpose) {
        let Some(job) = self.jobs.front() else {
            return;
        };
        let (client, counter) = (job.client, job.counter);
        // Battery and tier again, now: the command may have waited in the queue.
        if !self.battery_ok() {
            return self.finish_job(ResultCode::LowBattery, Vec::new());
        }
        match s.service_challenge(&payload) {
            Ok(ch) => {
                let id = self.next_challenge_id;
                self.next_challenge_id = self.next_challenge_id.wrapping_add(1).max(1);
                let mut data = Vec::new();
                data.extend_from_slice(&id.to_le_bytes());
                data.push(ch.tier() as u8);
                data.extend_from_slice(ch.code().as_bytes());
                self.pending = Some(Pending {
                    id,
                    client: client.unwrap_or(0),
                    ecu,
                    payload,
                    purpose,
                    challenge: ch,
                    expires_ms: self.now_ms + self.cfg.challenge_ttl_ms,
                });
                self.jobs.pop_front();
                if let Some(c) = client {
                    self.respond(Some(c), counter, ResultCode::Challenge, data);
                }
            }
            Err(e) => self.finish_job(session_error_code(&e), Vec::new()),
        }
    }

    #[cfg(feature = "approvals")]
    fn confirm_now(&mut self, s: &mut Session, challenge: u32, code: &str, now: u64) -> JobStart {
        use kline_core::safety::Approval;
        // The pending challenge is consumed whatever happens (one try per code).
        let Some(p) = self.pending.take() else {
            self.finish_job(ResultCode::ApprovalMismatch, Vec::new());
            return JobStart::Nothing;
        };
        if p.id != challenge || now >= p.expires_ms {
            self.finish_job(ResultCode::ApprovalMismatch, Vec::new());
            return JobStart::Nothing;
        }
        let approval = match Approval::confirm(p.challenge, code) {
            Ok(a) => a,
            Err(_) => {
                // A wrong code in the middle of arming leaves nothing half-armed.
                #[cfg(feature = "standing-lock")]
                if p.purpose != Purpose::Send && !self.lwm.is_armed() {
                    self.standing = None;
                }
                self.finish_job(ResultCode::ApprovalMismatch, Vec::new());
                return JobStart::Nothing;
            }
        };
        match p.purpose {
            Purpose::Send => {
                if !self.battery_ok() {
                    self.finish_job(ResultCode::LowBattery, Vec::new());
                    return JobStart::Nothing;
                }
                match s.request_approved(&p.payload, approval, now) {
                    Ok(a) => JobStart::Exchange(a),
                    Err(e) => {
                        self.finish_job(session_error_code(&e), Vec::new());
                        JobStart::Nothing
                    }
                }
            }
            #[cfg(feature = "standing-lock")]
            Purpose::ArmLock => {
                // The owner confirmed the lock request's risk. Nothing is sent; ask for the
                // unlock request's code next, on the same session.
                let risk = *approval.risk();
                drop(approval);
                self.standing = Some(Standing {
                    lock: Some(risk),
                    unlock: None,
                });
                self.make_challenge(EcuId::Bmt, s, BMT_UNLOCK.to_vec(), Purpose::ArmUnlock);
                JobStart::Nothing
            }
            #[cfg(feature = "standing-lock")]
            Purpose::ArmUnlock => {
                let risk = *approval.risk();
                drop(approval);
                match &mut self.standing {
                    Some(st) if st.lock.is_some() => {
                        st.unlock = Some(risk);
                        self.lwm.arm();
                        self.finish_job(ResultCode::Ok, Vec::new());
                    }
                    _ => {
                        self.standing = None;
                        self.finish_job(ResultCode::ApprovalMismatch, Vec::new());
                    }
                }
                JobStart::Nothing
            }
        }
    }

    /// Send one of the two armed BMT requests. The bridge answers kline-core's challenge
    /// itself, and only when the challenge is for exactly the armed bytes, target and risk.
    #[cfg(feature = "standing-lock")]
    fn standing_send(&mut self, s: &mut Session, intent: Intent, now: u64) -> JobStart {
        use kline_core::safety::Approval;
        let (payload, armed_risk) = match (intent, self.standing) {
            (Intent::Lock, Some(Standing { lock: Some(r), .. })) => (BMT_LOCK, r),
            (
                Intent::Unlock,
                Some(Standing {
                    unlock: Some(r), ..
                }),
            ) => (BMT_UNLOCK, r),
            _ => {
                self.lwm.intent_done(now, intent, false);
                self.jobs.pop_front();
                return JobStart::Nothing;
            }
        };
        if intent == Intent::Lock && !self.battery_ok() {
            self.lwm.intent_done(now, intent, false);
            self.jobs.pop_front();
            return JobStart::Nothing;
        }
        let ok = match s.service_challenge(&payload) {
            Ok(ch)
                if ch.payload() == payload
                    && ch.target() == gate::target(EcuId::Bmt)
                    && *ch.risk() == armed_risk =>
            {
                let code = String::from(ch.code());
                Approval::confirm(ch, &code)
                    .ok()
                    .and_then(|a| s.request_approved(&payload, a, now).ok())
            }
            _ => None,
        };
        match ok {
            Some(a) => JobStart::Exchange(a),
            None => {
                self.lwm.intent_done(now, intent, false);
                self.jobs.pop_front();
                JobStart::Nothing
            }
        }
    }

    fn lwm_intent(&mut self, intent: Intent) {
        #[cfg(feature = "standing-lock")]
        {
            self.jobs.push_back(Job {
                client: None,
                counter: 0,
                kind: JobKind::Standing(intent),
                since_ms: self.now_ms,
            });
        }
        #[cfg(not(feature = "standing-lock"))]
        {
            // Not reachable: the automaton is never armed without the feature.
            self.lwm.intent_done(self.now_ms, intent, false);
        }
    }

    // -----------------------------------------------------------------------------------------
    // Exchange results

    fn finished(&mut self, ecu: EcuId, op: Exch, r: Result<SessionOutcome, SessionError>) {
        match (op, r) {
            (Exch::Poll(_), Ok(SessionOutcome::Reply(Reply::Positive(p)))) => {
                self.positive(ecu, &p)
            }
            (Exch::Job, Ok(SessionOutcome::Reply(reply))) => self.job_reply(ecu, reply),
            (Exch::Job, Err(e)) => {
                if let SessionError::Denied(d) = &e {
                    let code = if d.reason.is_forbidden() {
                        ResultCode::Forbidden
                    } else {
                        ResultCode::ApprovalRequired
                    };
                    self.job_finished_with(code);
                } else {
                    self.job_exchange_failed(ecu);
                }
            }
            _ => {}
        }
    }

    fn job_finished_with(&mut self, code: ResultCode) {
        #[cfg(feature = "standing-lock")]
        if let Some(Job {
            kind: JobKind::Standing(i),
            ..
        }) = self.jobs.front()
        {
            let i = *i;
            self.lwm.intent_done(self.now_ms, i, code == ResultCode::Ok);
            self.jobs.pop_front();
            self.drop_standing_if_done();
            return;
        }
        self.finish_job(code, Vec::new());
    }

    fn job_exchange_failed(&mut self, _ecu: EcuId) {
        let Some(job) = self.jobs.front() else {
            return;
        };
        match &job.kind {
            // Keep reading until the verify deadline / job timeout.
            JobKind::Fob { .. } => {}
            _ => self.job_finished_with(ResultCode::LinkError),
        }
    }

    fn job_reply(&mut self, ecu: EcuId, reply: Reply) {
        if let Reply::Positive(p) = &reply {
            self.positive(ecu, p);
        }
        let Some(job) = self.jobs.front_mut() else {
            return;
        };
        if let JobKind::Fob { lock, phase } = &mut job.kind {
            let target = if *lock {
                LockState::Locked
            } else {
                LockState::Unlocked
            };
            if *phase == FobPhase::ReadState {
                match self.parked.lock {
                    LockState::Unknown => {
                        self.finish_job(ResultCode::LockNotConfirmed, Vec::new());
                    }
                    s if s == target => {
                        self.finish_job(ResultCode::Ok, Vec::new());
                    }
                    _ => *phase = FobPhase::NeedPulse,
                }
            }
            // Verify: decided in start_job on the next pass.
            return;
        }
        match reply {
            Reply::Positive(p) => self.job_finished_ok(p),
            Reply::Negative { sid, nrc } => {
                self.job_finished_negative(alloc::vec![0x7F, sid, nrc]);
            }
            Reply::Iso8Status(b) => self.job_finished_negative(alloc::vec![b]),
            Reply::ResponsePending { sid } => {
                self.job_finished_negative(alloc::vec![0x7F, sid, 0x78]);
            }
        }
    }

    fn job_finished_ok(&mut self, p: Vec<u8>) {
        #[cfg(feature = "standing-lock")]
        if matches!(
            self.jobs.front(),
            Some(Job {
                kind: JobKind::Standing(_),
                ..
            })
        ) {
            return self.job_finished_with(ResultCode::Ok);
        }
        self.finish_job(ResultCode::Ok, p);
    }

    fn job_finished_negative(&mut self, data: Vec<u8>) {
        #[cfg(feature = "standing-lock")]
        if matches!(
            self.jobs.front(),
            Some(Job {
                kind: JobKind::Standing(_),
                ..
            })
        ) {
            return self.job_finished_with(ResultCode::NegativeReply);
        }
        self.finish_job(ResultCode::NegativeReply, data);
    }

    /// Answer and remove the job at the front.
    fn finish_job(&mut self, code: ResultCode, data: Vec<u8>) {
        if let Some(job) = self.jobs.pop_front() {
            if code != ResultCode::Ok {
                self.last_error = code as u8;
            }
            if let Some(c) = job.client {
                self.respond(Some(c), job.counter, code, data);
            }
        }
    }

    fn job_failed(&mut self, job: Job, code: ResultCode) {
        #[cfg(feature = "standing-lock")]
        if let JobKind::Standing(i) = job.kind {
            self.lwm.intent_done(self.now_ms, i, false);
            return;
        }
        self.last_error = code as u8;
        if let Some(c) = job.client {
            self.respond(Some(c), job.counter, code, Vec::new());
        }
    }

    fn fail_jobs(&mut self, which: impl Fn(&JobKind) -> bool, code: ResultCode) {
        let busy = matches!(self.link, Link::Busy { op: Exch::Job, .. }) || self.awaiting;
        let mut keep = VecDeque::new();
        let mut first = true;
        while let Some(job) = self.jobs.pop_front() {
            let running = first && busy;
            first = false;
            if !running && which(&job.kind) {
                self.job_failed(job, code);
            } else {
                keep.push_back(job);
            }
        }
        self.jobs = keep;
    }

    /// A positive reply from `ecu`: decode the frames the bridge knows.
    fn positive(&mut self, ecu: EcuId, p: &[u8]) {
        let now = self.now_ms;
        match ecu {
            EcuId::Sirius => {
                let mut changed = false;
                if let Some(v) = live::sirius_aa(p) {
                    self.live.rpm = v.rpm;
                    self.live.inj_us = v.inj_us;
                    self.live.fuel_mlh = v.fuel_mlh;
                    self.live.flags |= live_flags::AA_VALID;
                    self.last_engine_sample = Some((now, v.rpm));
                    changed = true;
                }
                if let Some(v) = live::sirius_a9(p) {
                    self.live.rpm = v.rpm;
                    self.live.speed_kmh = v.speed_kmh;
                    self.live.batt_cv = v.batt_cv;
                    self.live.map_mbar = v.map_mbar;
                    self.live.coolant_c = v.coolant_c;
                    self.live.iat_c = v.iat_c;
                    self.live.flags |= live_flags::A9_VALID;
                    self.last_engine_sample = Some((now, v.rpm));
                    changed = true;
                    if let Some(intent) = self.lwm.on_sample(now, v.speed_kmh, v.rpm) {
                        self.lwm_intent(intent);
                    }
                }
                if changed {
                    self.live.seq = self.live.seq.wrapping_add(1);
                    self.live.t_ms = now as u32;
                    let mut flags = self.live.flags & (live_flags::AA_VALID | live_flags::A9_VALID);
                    if self.live.rpm > 0 {
                        flags |= live_flags::ENGINE_RUNNING;
                    }
                    if self.lwm.is_armed() {
                        flags |= live_flags::LWM_ARMED;
                    }
                    if self.lwm.locked_by_us() {
                        flags |= live_flags::LWM_LOCKED;
                    }
                    self.live.flags = flags;
                    if !self.clients.is_empty() {
                        self.out.push_back(Output::Notify {
                            to: None,
                            channel: Channel::Live,
                            frame: self.live.encode(),
                        });
                    }
                }
            }
            EcuId::Bmt => match live::bmt(p) {
                Some(BmtFrame::Lock(l)) => {
                    self.parked.lock = l;
                    self.parked.lock_at_ms = Some(now);
                }
                Some(BmtFrame::Doors {
                    front_open,
                    rear_open,
                }) => {
                    self.parked.front_open = front_open;
                    self.parked.rear_open = rear_open;
                }
                Some(BmtFrame::Battery { batt_cv }) => self.parked.batt_cv = Some(batt_cv),
                None => {}
            },
        }
    }

    // -----------------------------------------------------------------------------------------
    // Decisions

    fn job_ecu(&self, k: &JobKind) -> Option<EcuId> {
        match k {
            #[cfg(feature = "approvals")]
            JobKind::Confirm { .. } => self.pending.as_ref().map(|p| p.ecu),
            other => job_ecu_of(other),
        }
    }

    /// The ECU the bridge should have a session with now, if any.
    fn desired_ecu(&self, now: u64) -> Option<EcuId> {
        if self.yielded || self.asleep || self.critical_battery() || self.low_battery_sleep(now) {
            return None;
        }
        if let Some(j) = self.jobs.front() {
            if let Some(e) = self.job_ecu(&j.kind) {
                return Some(e);
            }
        }
        #[cfg(feature = "approvals")]
        if let Some(p) = &self.pending {
            if now < p.expires_ms {
                return Some(p.ecu);
            }
        }
        self.effective_profile(now).ecu()
    }

    /// The requested profile, or Drive on its own when the engine runs (so lock-when-moving
    /// and the dashboard work without a command).
    fn effective_profile(&self, now: u64) -> Profile {
        if self.requested != Profile::Off {
            return self.requested;
        }
        // Lock-when-moving owes an unlock: keep talking to the Sirius to see the engine stop
        // (a Sirius that no longer answers is the ignition-off signal).
        if self.charging() || self.engine_running(now) || self.lwm.locked_by_us() {
            return Profile::Drive;
        }
        Profile::Off
    }

    fn charging(&self) -> bool {
        self.battery_mv.is_some_and(|mv| mv >= self.cfg.charging_mv)
    }

    /// rpm > 0 in a Sirius frame of the last 3 s (the LWM silence time).
    fn engine_running(&self, now: u64) -> bool {
        self.last_engine_sample
            .is_some_and(|(t, rpm)| rpm > 0 && now.saturating_sub(t) < self.cfg.lwm.silence_ms)
    }

    /// Ignition on as far as the bridge can tell: engine running or charging voltage.
    fn ignition_on(&self, now: u64) -> bool {
        self.engine_running(now) || self.charging()
    }

    /// At or above the write limit (an unknown voltage counts as low: no writes blind).
    fn battery_ok(&self) -> bool {
        self.battery_mv
            .is_some_and(|mv| mv >= self.cfg.low_battery_mv)
    }

    /// MASTER-PLAN-v2 P6: below 11.8 V switch off at once.
    fn critical_battery(&self) -> bool {
        self.battery_mv.is_some_and(|mv| mv < 11_800)
    }

    /// Sessions (reads) are allowed down to the critical limit; writes need `battery_ok`.
    fn battery_allows_session(&self) -> bool {
        !self.critical_battery()
    }

    /// Below the write limit with the engine off: stop and sleep (MASTER-PLAN-v2 P6, task
    /// W4). The engine running wins: the voltage then is a measurement problem, not a flat
    /// battery, and lock-when-moving must keep its Sirius session.
    /// Not while lock-when-moving still owes the unlock: unlocking is the safe direction and
    /// goes first (MASTER-PLAN-v2 3.1 rule 2).
    fn low_battery_sleep(&self, now: u64) -> bool {
        self.battery_mv
            .is_some_and(|mv| mv < self.cfg.low_battery_mv)
            && !self.engine_running(now)
            && !self.lwm.locked_by_us()
    }

    /// `Some(wake_after_ms)` when the bridge should sleep now (link already off).
    fn should_sleep(&self, now: u64) -> Option<u64> {
        if self.low_battery_sleep(now) || self.critical_battery() {
            return Some(self.cfg.low_battery_wake_ms);
        }
        if !self.jobs.is_empty() || self.lwm.locked_by_us() || self.engine_running(now) {
            return None;
        }
        if self.clients.is_empty()
            && self.effective_profile(now) == Profile::Off
            && now.saturating_sub(self.last_activity_ms) >= self.cfg.idle_sleep_ms
        {
            return Some(self.cfg.idle_wake_ms);
        }
        None
    }

    // -----------------------------------------------------------------------------------------
    // Notifications

    fn respond(&mut self, to: Option<ClientId>, counter: u32, result: ResultCode, data: Vec<u8>) {
        let frame = Response {
            counter,
            result,
            data,
        }
        .encode();
        self.out.push_back(Output::Notify {
            to,
            channel: Channel::Response,
            frame,
        });
    }

    fn raw_frame(&mut self, dir: RawDir, bytes: Vec<u8>) {
        let ecu = match &self.link {
            Link::Init { ecu, .. } | Link::Ready { ecu, .. } | Link::Busy { ecu, .. } => {
                ecu.address()
            }
            _ => 0,
        };
        let f = RawFrame {
            t_ms: self.now_ms as u32,
            dir,
            ecu,
            bytes,
        };
        self.out.push_back(Output::Notify {
            to: None,
            channel: Channel::Raw,
            frame: f.encode(),
        });
    }
}

enum JobStart {
    Exchange(Action),
    Pulse,
    Nothing,
}

fn job_ecu_of(k: &JobKind) -> Option<EcuId> {
    match k {
        JobKind::Request { ecu, .. } => Some(*ecu),
        #[cfg(feature = "approvals")]
        JobKind::Prepare { ecu, .. } => Some(*ecu),
        #[cfg(feature = "approvals")]
        JobKind::Confirm { .. } => None,
        JobKind::Fob { .. } => Some(EcuId::Bmt),
        #[cfg(feature = "standing-lock")]
        JobKind::Standing(_) => Some(EcuId::Bmt),
    }
}

fn session_error_code(e: &SessionError) -> ResultCode {
    match e {
        SessionError::Denied(d) if d.reason.is_forbidden() => ResultCode::Forbidden,
        SessionError::Denied(d)
            if matches!(
                d.reason,
                kline_core::safety::DenyReason::ApprovalMismatch
                    | kline_core::safety::DenyReason::IdentificationMismatch
                    | kline_core::safety::DenyReason::PreReadRequired { .. }
            ) =>
        {
            ResultCode::ApprovalMismatch
        }
        SessionError::Denied(_) => ResultCode::ApprovalRequired,
        SessionError::Busy => ResultCode::Busy,
        _ => ResultCode::LinkError,
    }
}

/// Did the ECU answer the failed init attempt? Same rule as kline-host's `ecu_answered`
/// (docs/protocol.md "Wait after an answered attempt"): then wait P3max before the next one.
fn ecu_answered(e: &LinkError) -> bool {
    match e {
        LinkError::NoKeywords { received } => !received.is_empty(),
        LinkError::KeywordMismatch { .. }
        | LinkError::NoAddressComplement { .. }
        | LinkError::Collision { .. }
        | LinkError::IncompleteReply { .. }
        | LinkError::Frame(_)
        | LinkError::TooManyPending { .. }
        | LinkError::UnexpectedStartReply(_) => true,
        _ => false,
    }
}

/// Status changes worth a notification (uptime alone is not one).
fn same_status(a: &Status, b: &Status) -> bool {
    Status { uptime_s: 0, ..*a } == Status { uptime_s: 0, ..*b }
}
