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
#[cfg(feature = "approvals")]
use kline_core::safety::Tier;
use kline_core::session::{Session, SessionError, SessionOutcome};
use kline_core::{Action, Event, LinkError, Step};

use crate::fob::{FobError, FobTiming};
use crate::gate::{self, Role};
use crate::gatt::{
    live_flags, status_flags, BridgeState, Channel, Command, ExtraFrame, LiveSample, LockState, Op,
    RawDir, RawFrame, Response, ResultCode, Status,
};
use crate::live::{self, BmtFrame};
use crate::lwm::{Intent, LockWhenMoving, LwmConfig, Outcome};
use crate::profile::{self, EcuId, Profile, EXTRA_SCHEDULE};
use crate::ratelimit::{ClientId, Counters, RateLimiter};

/// BMT actuator "lock all doors", 1 command: `31 A0 00 01` (WRITE-COMMANDS 1.5, decision D37).
pub const BMT_LOCK: [u8; 4] = [0x31, 0xA0, 0x00, 0x01];
/// BMT actuator "unlock all doors", 1 command: `31 A1 00 01`.
pub const BMT_UNLOCK: [u8; 4] = [0x31, 0xA1, 0x00, 0x01];
/// MASTER-PLAN-v2 P6: below 11.8 V switch off.
const CRITICAL_BATTERY_MV: u16 = 11_800;
/// A charging voltage that begins within this long after a dip is a start, not a charger.
/// ours: a start takes about 1 to 3 s from the cranking dip to the alternator's voltage.
const START_WINDOW_MS: u64 = 30_000;
/// The automatic body read waits this long at a standstill (owner decision 2026-11-25).
const AUTO_BODY_READ_AFTER_MS: u64 = 3_000;
/// The longest a body read keeps "engine running" (review r5: a hard bound). ours: the read
/// takes about 5.3 s in the sim, 11 s at worst on the car.
const SIRIUS_OWED_MS: u64 = 15_000;
/// BMT lock-state read (`21 A3`, DCODCOND in data byte 0 bit 7).
const BMT_LOCK_READ: [u8; 2] = [0x21, 0xA3];
/// Sirius speed and rpm (`21 A9`, the drive profile's frame), read before a PREPARE.
#[cfg(feature = "approvals")]
const SIRIUS_SAMPLE: [u8; 2] = [0x21, 0xA9];

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
    /// Deep sleep (low battery, critical battery, parked for long): a timer wake this often
    /// for one ADC reading, no BLE ([`crate::power`]). ours: 30 s (owner decision 2026-11-25).
    pub deep_wake_ms: u64,
    /// Parked light sleep (BLE advertising) at most this long after the engine last ran, then
    /// deep sleep. ours: 48 h (owner decision 2026-11-25).
    pub parked_light_max_ms: u64,
    /// Watchdog against a stuck-awake bridge: no client and not in the drive profile for this
    /// long, sleep. ours: 10 min (owner decision 2026-11-25).
    pub stuck_awake_ms: u64,
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
    /// Battery below the write limit continuously for this long before the bridge stops and
    /// sleeps; cranking dips and ADC noise are shorter. ours: 10 s.
    pub low_battery_debounce_ms: u64,
    /// Below 11.8 V (MASTER-PLAN-v2 P6 "okamžite vypnúť") continuously for this long. ours: 5 s,
    /// longer than a start (cranking dips last about 1-2 s).
    pub critical_battery_debounce_ms: u64,
    /// A Sirius speed sample older than this is not "the car's speed now". ours: 2 s.
    pub speed_fresh_ms: u64,
    /// A battery rise by more than this over the voltage of the engine-off sample voids it: an
    /// alternator raises the voltage over a resting or charger-held battery (review M4-1). ours:
    /// 300 mV, above the divider's reading noise (NOT VERIFIED on the bridge).
    pub start_rise_mv: u16,
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
            deep_wake_ms: crate::power::DEEP_WAKE_MS,
            parked_light_max_ms: crate::power::PARKED_LIGHT_MAX_MS,
            stuck_awake_ms: crate::power::STUCK_AWAKE_MS,
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
            low_battery_debounce_ms: 10_000,
            critical_battery_debounce_ms: 5_000,
            speed_fresh_ms: 2_000,
            start_rise_mv: 300,
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
    Sleep {
        wake_after_ms: Option<u64>,
        reason: SleepReason,
    },
    /// Nothing to do until then (or the next input).
    Idle { until_ms: u64 },
}

/// Why the bridge sleeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepReason {
    /// Below 12.0 V for the debounce time, engine off (MASTER-PLAN-v2 P6): hard off, deep sleep.
    LowBattery,
    /// Below 11.8 V for the debounce time (P6: switch off at once): deep sleep.
    CriticalBattery,
    /// No client, no profile, nothing to do: light sleep with BLE advertising.
    Idle,
    /// No client and not in the drive profile for `stuck_awake_ms` (the stuck-awake watchdog):
    /// light sleep with BLE advertising.
    NoClient,
    /// Parked for more than `parked_light_max_ms` since the engine last ran: deep sleep.
    ParkedLong,
}

impl SleepReason {
    /// How the firmware sleeps for this reason ([`crate::power`]).
    pub fn mode(self) -> crate::power::SleepMode {
        use crate::power::SleepMode;
        match self {
            SleepReason::Idle | SleepReason::NoClient => SleepMode::LightAdvertising,
            SleepReason::LowBattery | SleepReason::CriticalBattery | SleepReason::ParkedLong => {
                SleepMode::DeepTimer
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FobPhase {
    /// Read `21 A3`. Only a positive reply of this job decides (review M1).
    ReadState,
    /// The state differs from the target: pulse next.
    NeedPulse,
    /// The pulse was handed out; waiting for `on_fob_done`.
    Pulsing,
    /// Re-reading `21 A3` until a positive reply of this job shows the target state.
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
        /// Tier 2: the `21 80` reply the app chose its definition from.
        identification: Vec<u8>,
        /// Read `21 A9` from the Sirius first: nothing recent shows that the car stands with
        /// the engine off (review r3 minor 1).
        sample: bool,
    },
    #[cfg(feature = "approvals")]
    Confirm {
        challenge: u32,
        code: String,
    },
    Fob {
        lock: bool,
        phase: FobPhase,
        /// The lock state of the last positive `21 A3` reply received by this job.
        seen: Option<LockState>,
    },
    #[cfg(feature = "standing-lock")]
    Standing(Intent),
    /// Read the BMT lock state once (after LWM_DISARM with an unlock owed): a positive read
    /// showing the doors unlocked ends the duty (review B-3).
    LockCheck,
    /// The body read at a standstill (READ_BODY_ONCE or the automatic one): the
    /// [`profile::BODY_READ_SET`] on the BMT in turn, `next` the index of the next request,
    /// each reply published as an Extra frame.
    BodyRead {
        next: usize,
    },
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
    /// The session the challenge was made on ([`Bridge::session_gen`]).
    session_gen: u32,
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

/// Live extras (`LIVE_EXTRAS`, the "advanced" drive profile). The setting is in RAM: a reset
/// turns it off. The counters restart with every Sirius session.
#[derive(Debug, Default, Clone, Copy)]
struct Extras {
    /// 0 off, `n`: one extra read after every `n` drive cycles.
    every: u8,
    /// Drive cycles completed since the last extra read.
    cycles: u8,
    /// An extra read is due before the next drive poll.
    due: bool,
    /// Next position in [`EXTRA_SCHEDULE`].
    next: usize,
    /// [`EXTRA_SCHEDULE`] positions the ECU answered negatively in this session (bit i).
    unsupported: u8,
    /// Extra notifications sent (the [`ExtraFrame`] seq).
    seq: u16,
}

/// What the poll in progress is, for the extras.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum PollKind {
    /// A drive-profile poll whose reply is not forwarded.
    #[default]
    Plain,
    /// A drive-profile poll whose raw reply is forwarded as an Extra frame (extras on).
    Forward,
    /// Extra read number `i` of [`EXTRA_SCHEDULE`].
    Extra(usize),
    /// Parked: the BMT transponder status read ([`profile::TRANSPONDER_READ`]).
    Transponder,
    /// The BMT door read right before the switch to the drive profile.
    PreDrive,
}

/// The parked-profile reads that are not in the schedule.
#[derive(Debug, Default, Clone, Copy)]
struct BodyReads {
    /// Parked cycles since the last transponder read.
    cycles: usize,
    /// The ignition bit of the last transponder read in this BMT session.
    ignition: Option<bool>,
    /// The pre-drive door read was made in this BMT session.
    predrive_done: bool,
    /// The transponder status was published in this BMT session.
    transponder_sent: bool,
    /// `poll_index` of the last cycle start counted (a transponder read does not advance it).
    cycle_mark: Option<usize>,
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
    /// The lock risk confirmed in the first step of an arming, until the second step.
    #[cfg(feature = "standing-lock")]
    arming_lock: Option<kline_core::safety::RiskInfo>,
    /// The challenge dropped because the car may have started moving while lock-when-moving is
    /// armed (review r3 minor 2): its CONFIRM is answered `LwmActive`.
    #[cfg(feature = "standing-lock")]
    lwm_cancelled: Option<u32>,
    /// Counts the sessions; a challenge belongs to the session it was made on.
    session_gen: u32,
    lwm: LockWhenMoving,
    last_lwm_tick: u64,
    live: LiveSample,
    parked: Parked,
    last_engine_sample: Option<(u64, u16)>,
    /// Latest Sirius speed and when it was read.
    last_speed: Option<(u64, u8)>,
    /// The latest Sirius `21 A9` showing 0 km/h and 0 rpm, whether the battery read as
    /// charging then, and the battery voltage then (review r3 minor 1: engine off, whatever a
    /// charger does to the voltage).
    engine_off_at: Option<(u64, bool, u16)>,
    /// The latest battery reading below the write limit (a start pulls the battery far below
    /// it): an engine-off proof older than this no longer counts. The firmware also reports the
    /// lowest reading taken during a blocking K-line action (review M4-1).
    last_dip_ms: Option<u64>,
    /// Since when the battery reads below the write limit / the critical limit.
    low_since: Option<u64>,
    critical_since: Option<u64>,
    /// A session ended without a clean Stop (link errors) at this time: the next init waits
    /// P3max so the old ECU has timed out (one master on the line).
    unclean_end: Option<u64>,
    /// The job exchange in progress wrote its request (a lost reply may mean it executed).
    job_written: bool,
    poll_index: usize,
    extras: Extras,
    poll_kind: PollKind,
    body_reads: BodyReads,
    /// A Sirius init failed while the voltage read as charging (a charger, ignition off).
    sirius_silent_at: Option<u64>,
    /// Rest time and parked voltage history for the battery estimate (`crate::soc`).
    soc: crate::soc::History,
    /// Since when (and from which voltage) the battery reads charging.
    charging_since: Option<(u64, u16)>,
    /// Sirius inits in a row that got no answer.
    sirius_init_failures: u32,
    /// The last time a client was connected (the stuck-awake watchdog).
    last_client_ms: u64,
    /// READ_BODY_ONCE / the automatic body read: the current standstill (fresh 21A9 speed 0).
    stopped_since: Option<u64>,
    /// A body read was made in this standstill (at most one per stop).
    body_read_this_stop: bool,
    /// When the last body read was queued (at most one per [`profile::BODY_READ_MIN_GAP_MS`]).
    last_body_read_ms: Option<u64>,
    /// The bridge left a running engine's Sirius for a body read and has not tried the Sirius
    /// since: until this time the engine still counts as running (a pause the bridge made
    /// itself never ends "engine running", review r4 MAJOR-1), so the bridge goes back to the
    /// Sirius. Bounded: [`SIRIUS_OWED_MS`] after the read started, and cleared by a yield. (Not
    /// cleared when a job for another ECU follows the read: that would end "engine running"
    /// again, the MAJOR-1 case; the bound covers it.)
    sirius_owed_until: Option<u64>,
    yielded: bool,
    /// Who yielded the line (the lease): only that client or an owner may resume it.
    yield_holder: Option<ClientId>,
    /// YIELD command waiting for the session to stop and the transceiver to go off.
    yield_reply: Option<(ClientId, u32)>,
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
            #[cfg(feature = "standing-lock")]
            arming_lock: None,
            #[cfg(feature = "standing-lock")]
            lwm_cancelled: None,
            session_gen: 0,
            last_lwm_tick: now_ms,
            live: LiveSample::default(),
            parked: Parked::default(),
            last_engine_sample: None,
            last_speed: None,
            engine_off_at: None,
            last_dip_ms: None,
            low_since: None,
            critical_since: None,
            unclean_end: None,
            job_written: false,
            poll_index: 0,
            extras: Extras::default(),
            poll_kind: PollKind::Plain,
            body_reads: BodyReads::default(),
            sirius_silent_at: None,
            soc: crate::soc::History::default(),
            charging_since: None,
            sirius_init_failures: 0,
            last_client_ms: now_ms,
            stopped_since: None,
            body_read_this_stop: false,
            last_body_read_ms: None,
            sirius_owed_until: None,
            yielded: false,
            yield_holder: None,
            yield_reply: None,
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

    /// The client holding the K-line yield, if yielded. The firmware saves it to NVS on change
    /// and gives it back with [`Bridge::restore_yield`] after a reset, so a reset never takes
    /// the line from another tester (review M6).
    pub fn yield_holder(&self) -> Option<ClientId> {
        self.yield_holder
    }

    pub fn is_yielded(&self) -> bool {
        self.yielded
    }

    /// At boot: the line was yielded by `holder` before the reset.
    pub fn restore_yield(&mut self, holder: ClientId) {
        self.yielded = true;
        self.yield_holder = Some(holder);
    }

    /// A peer (re)bonded: forget its old counter, so a bond whose counter reached `u32::MAX`
    /// is not locked out forever (review nit).
    pub fn on_bond(&mut self, client: ClientId) {
        self.counters.forget(client);
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
        if self.lwm.gave_up() {
            flags |= status_flags::LWM_GAVE_UP;
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
        // While the engine runs the rest time is 0, so the estimate is unknown by itself.
        let (soc_pct, soc_conf) = self.soc.estimate(self.now_ms, self.charging());
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
            soc_pct: soc_pct.unwrap_or(crate::gatt::SOC_UNKNOWN),
            soc_conf: soc_conf as u8,
        }
    }

    // -----------------------------------------------------------------------------------------
    // Inputs

    /// A bonded client connected with `role` (from the bond table, set at pairing).
    /// Unbonded peers are not clients: no notifications go to them, and their commands are
    /// refused.
    pub fn on_connect(&mut self, client: ClientId, role: Role, now_ms: u64) {
        self.now_ms = now_ms;
        if role != Role::Unbonded {
            self.clients.insert(client, role);
        }
        self.last_activity_ms = now_ms;
        self.last_client_ms = now_ms;
    }

    pub fn on_disconnect(&mut self, client: ClientId, now_ms: u64) {
        self.now_ms = now_ms;
        self.clients.remove(&client);
        self.last_activity_ms = now_ms;
        self.last_client_ms = now_ms;
    }

    /// Battery voltage from the bridge's divider (PLAN-RESEARCH B5).
    pub fn on_battery_mv(&mut self, mv: u16, now_ms: u64) {
        self.now_ms = now_ms;
        self.battery_mv = Some(mv);
        // A rise over the voltage of the engine-off sample is an alternator (review M4-1: a
        // start whose dip was not seen).
        if self
            .engine_off_at
            .is_some_and(|(_, _, m0)| mv > m0.saturating_add(self.cfg.start_rise_mv))
        {
            self.engine_off_at = None;
        }
        if mv >= self.cfg.charging_mv {
            self.charging_since.get_or_insert((now_ms, mv));
        } else {
            self.charging_since = None;
        }
        let running = self.charging() || self.engine_running(now_ms);
        self.soc.on_battery(now_ms, mv, running, self.last_dip_ms);
        if mv < self.cfg.low_battery_mv {
            self.low_since.get_or_insert(now_ms);
            self.last_dip_ms = Some(now_ms);
        } else {
            self.low_since = None;
        }
        if mv < CRITICAL_BATTERY_MV {
            self.critical_since.get_or_insert(now_ms);
        } else {
            self.critical_since = None;
        }
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
            Op::StartLive(p) if p != Profile::Drive && self.lwm_holds_sirius(now_ms) => {
                r(ResultCode::LwmActive)
            }
            Op::StartLive(p) => {
                self.requested = p;
                Ok(())
            }
            Op::StopLive => {
                self.requested = Profile::Off;
                Ok(())
            }
            Op::Request { ecu, .. } | Op::Prepare { ecu, .. } | Op::PrepareWrite { ecu, .. }
                if ecu != EcuId::Sirius && self.lwm_holds_sirius(now_ms) =>
            {
                r(ResultCode::LwmActive)
            }
            Op::Request { ecu, payload } => gate::check_request(role, ecu, &payload)
                .and_then(|()| self.enqueue(client, counter, JobKind::Request { ecu, payload })),
            Op::Prepare { ecu, payload } => self.prepare(client, counter, role, ecu, payload, None),
            Op::PrepareWrite {
                ecu,
                identification,
                payload,
            } => self.prepare(client, counter, role, ecu, payload, Some(identification)),
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
                            seen: None,
                        },
                    )
                }
            }
            // Not while lock-when-moving needs the Sirius (an unlock owed, or armed and driving).
            Op::YieldKline if self.lwm_holds_sirius(now_ms) => r(ResultCode::LwmActive),
            Op::YieldKline if self.yielded => r(ResultCode::Yielded),
            Op::YieldKline => {
                self.yielded = true;
                // The line goes to the client, not back to the Sirius (review r5 MAJOR-A).
                self.sirius_owed_until = None;
                self.yield_holder = Some(client);
                self.fail_jobs(|_| true, ResultCode::Yielded);
                // Answered Ok only once the session is stopped and the transceiver is off.
                self.yield_reply = Some((client, counter));
                self.respond(Some(client), counter, ResultCode::Accepted, Vec::new());
                return;
            }
            // The lease: only the client that yielded, or an owner, takes the line back.
            Op::ResumeKline
                if self.yielded && self.yield_holder != Some(client) && role != Role::Owner =>
            {
                r(ResultCode::NotAllowedForRole)
            }
            Op::ResumeKline => {
                self.yielded = false;
                self.yield_holder = None;
                self.yield_reply = None;
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
                    // An unlock still owed: read the lock state once (answered Ok at once). Only
                    // an observed locked -> unlocked change ends the duty (review B4-1). One
                    // such read queued at a time, within the queue limit.
                    let queued = self
                        .jobs
                        .iter()
                        .any(|j| matches!(j.kind, JobKind::LockCheck));
                    if self.lwm.locked_by_us()
                        && !self.yielded
                        && !queued
                        && self.jobs.len() < self.cfg.max_queue
                    {
                        self.jobs.push_back(Job {
                            client: None,
                            counter: 0,
                            kind: JobKind::LockCheck,
                            since_ms: now_ms,
                        });
                    }
                    Ok(())
                }
            }
            Op::LwmAck => {
                if role != Role::Owner {
                    r(ResultCode::NotAllowedForRole)
                } else {
                    self.lwm.acknowledge_give_up();
                    Ok(())
                }
            }
            Op::RawStream(on) => {
                self.raw = on;
                Ok(())
            }
            // Reads only: any bonded role.
            Op::ReadBodyOnce => self.read_body_once(client, counter, now_ms),
            // Reads only: any bonded role. The counters restart, the session stays.
            Op::LiveExtras(every) => {
                self.extras.every = every;
                self.extras.cycles = 0;
                self.extras.due = false;
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
        identification: Option<Vec<u8>>,
    ) -> Result<(), ResultCode> {
        let tier = gate::check_prepare(role, ecu, &payload, self.battery_ok())?;
        // Tier 2 needs the identification the app chose its definition from (review m-1); it
        // is not accepted for the other tiers.
        match (tier, &identification) {
            (Tier::ConfigWrite, None) => return Err(ResultCode::IdentificationRequired),
            (Tier::ConfigWrite, Some(_)) | (_, None) => {}
            (_, Some(_)) => return Err(ResultCode::BadFrame),
        }
        let sample = self.motion_check(ecu)?;
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
                identification: identification.unwrap_or_default(),
                sample,
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
        _identification: Option<Vec<u8>>,
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
        #[cfg(feature = "standing-lock")]
        if self.lwm_cancelled == Some(challenge) {
            self.lwm_cancelled = None;
            return Err(ResultCode::LwmActive);
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
        // Review B-1/B-2: never while armed or while an unlock is owed (re-arming would replace
        // the risks the unlock duty relies on, and the BMT challenge would take the bridge off
        // the Sirius); disarm first. Never while the car may move.
        if self.lwm.is_armed() || self.lwm.locked_by_us() || self.lwm_holds_sirius(self.now_ms) {
            return Err(ResultCode::LwmActive);
        }
        // Parked, engine off (SAFETY.md rule 2, review r3 minor 5): the same rule as any BMT
        // write, so an idling engine at a standstill is refused too.
        let sample = self.motion_check(EcuId::Bmt)?;
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
                identification: Vec::new(),
                sample,
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

    /// At PREPARE: refuse when fresh Sirius data already shows the request may not run;
    /// otherwise whether the job must first read a fresh Sirius sample.
    #[cfg(feature = "approvals")]
    fn motion_check(&self, ecu: EcuId) -> Result<bool, ResultCode> {
        let moving = self.moving_for(ecu, self.now_ms);
        match self.fresh_speed(self.now_ms) {
            Some(_) if moving => Err(ResultCode::VehicleMoving),
            Some(_) => Ok(false),
            None => Ok(moving),
        }
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
                    if ecu == EcuId::Sirius {
                        self.sirius_init_failures = 0;
                        // A new Sirius session starts a new standstill (no instant body read at
                        // the first 21A9 of a drive); a new drive (not the return from a body
                        // read) may read the body at its first stop.
                        self.stopped_since = None;
                        if !self.sirius_owed(now_ms) {
                            self.body_read_this_stop = false;
                        }
                    }
                    self.session_gen = self.session_gen.wrapping_add(1);
                    self.poll_index = 0;
                    self.extras = Extras {
                        every: self.extras.every,
                        seq: self.extras.seq,
                        ..Extras::default()
                    };
                    // Per session: the transponder period and the pre-drive read. The last
                    // ignition state is kept across sessions (review: the ignition-off publish
                    // after a drive).
                    self.body_reads = BodyReads {
                        ignition: self.body_reads.ignition,
                        ..BodyReads::default()
                    };
                    self.unclean_end = None;
                    self.link = Link::Ready { ecu, s, errors: 0 };
                }
                Step::Failed(e) => {
                    self.init_failed(ecu, failures + 1, &e);
                    if ecu == EcuId::Sirius {
                        self.sirius_init_failures = self.sirius_init_failures.saturating_add(1);
                        // A charger with the ignition off: two inits in a row unanswered while
                        // the charging voltage predates any dip or rise and no rpm was seen
                        // (review: one missed answer of a running engine parked the bridge).
                        if self.sirius_init_failures >= 2 && self.charger_like(now_ms) {
                            self.sirius_silent_at = Some(now_ms);
                        }
                        self.sirius_failed();
                    }
                }
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
                    // A live-extras read that got no answer is not a Sirius failure (it never
                    // counts toward ignition off or re-init, and never restarts the
                    // lock-when-moving hold): the frame is skipped for the rest of the session.
                    let extra = match (op, self.poll_kind) {
                        (Exch::Poll(_), PollKind::Extra(i)) => Some(i),
                        _ => None,
                    };
                    if let Some(i) = extra {
                        self.extras.unsupported |= 1 << i;
                    }
                    let errors = if extra.is_some() { errors } else { errors + 1 };
                    if ecu == EcuId::Sirius
                        && extra.is_none()
                        && matches!(op, Exch::Poll(_) | Exch::TesterPresent)
                    {
                        self.sirius_failed();
                    }
                    if op == Exch::Stop || s.is_closed() {
                        self.unclean_end = Some(now_ms);
                        self.link = Link::Off;
                    } else if errors >= self.cfg.exchange_failures_before_reinit {
                        self.unclean_end = Some(now_ms);
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
        if matches!(self.link, Link::Ready { .. } | Link::Busy { .. }) {
            self.unclean_end = Some(now_ms);
        }
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
        if let JobKind::Fob { phase, seen, .. } = &mut job.kind {
            if *phase == FobPhase::Pulsing {
                // Only reads after the pulse count for the verification.
                *seen = None;
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
        if matches!(a, Action::Write { .. })
            && matches!(self.link, Link::Busy { op: Exch::Job, .. })
        {
            self.job_written = true;
        }
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
        // Armed, and a challenge for another ECU keeps the bridge off the Sirius while the car
        // may have started (charging voltage, a start's voltage dip): drop it so lock-when-moving
        // gets the Sirius back (review r3 minor 2). Its CONFIRM would be refused anyway.
        #[cfg(feature = "standing-lock")]
        if self.lwm.is_armed() {
            if let Some(p) = &self.pending {
                if p.ecu != EcuId::Sirius && self.moving_for(p.ecu, now) {
                    self.lwm_cancelled = Some(p.id);
                    self.pending = None;
                }
            }
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
        // YIELD answered once the line is released.
        if let Some((c, counter)) = self.yield_reply {
            if matches!(self.link, Link::Off) && !self.power && self.yielded && !self.awaiting {
                self.yield_reply = None;
                self.respond(Some(c), counter, ResultCode::Ok, Vec::new());
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
                    // After a session that ended without a clean Stop, wait P3max before any
                    // init, so the old ECU has timed out (never two talking at once).
                    if let Some(t) = self.unclean_end {
                        let until = t + self.cfg.answered_wait_ms;
                        if now < until {
                            return Some(Output::Idle { until_ms: until });
                        }
                    }
                    Some(self.start_init(ecu, 0))
                }
                None => {
                    if self.power {
                        self.power = false;
                        return Some(Output::Power(false));
                    }
                    if let Some((wake, reason)) = self.should_sleep(now) {
                        self.fail_jobs(|_| true, ResultCode::LowBattery);
                        self.asleep = true;
                        if reason.mode() == crate::power::SleepMode::DeepTimer {
                            // The last Status before the chip powers down (review r4 minor 5):
                            // a deep-sleep wake resets, so an armed lock-when-moving (flag 0x01
                            // here) is disarmed after it.
                            self.last_error = ResultCode::DeepSleep as u8;
                        }
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
                            reason,
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
                // After an unclean end, never before P3max (one master on the line).
                let until = match self.unclean_end {
                    Some(t) => until.max(t + self.cfg.answered_wait_ms),
                    None => until,
                };
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
                // Leaving the parked profile for the drive profile (the engine started): one
                // more door read first, for the app's drive-off check.
                if desired != Some(ecu)
                    && ecu == EcuId::Bmt
                    && desired == Some(EcuId::Sirius)
                    && !self.body_reads.predrive_done
                    && self.effective_profile(now) == Profile::Drive
                {
                    self.body_reads.predrive_done = true;
                    if let Ok(a) = s.request(&[0x21, profile::PREDRIVE_DOORS_READ], now) {
                        self.poll_kind = PollKind::PreDrive;
                        self.link = Link::Busy {
                            ecu,
                            s,
                            errors,
                            op: Exch::Poll(profile::PREDRIVE_DOORS_READ),
                        };
                        return Some(self.hand_out(a));
                    }
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
            self.job_written = false;
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
            let (lid, kind) = self.next_poll(profile, schedule);
            self.poll_kind = kind;
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

    /// The next profile poll: the schedule in turn, and with live extras on (drive profile
    /// only) one [`EXTRA_SCHEDULE`] read after every `every` completed cycles. The drive
    /// schedule itself is never skipped, so lock-when-moving keeps its `21 A9` samples.
    fn next_poll(&mut self, profile: Profile, schedule: &[u8]) -> (u8, PollKind) {
        if profile == Profile::Parked
            && self.poll_index.is_multiple_of(schedule.len())
            && self.body_reads.cycle_mark != Some(self.poll_index)
        {
            // Start of a parked cycle (counted once): the transponder status every
            // TRANSPONDER_EVERY cycles, the first cycle of a session too.
            self.body_reads.cycle_mark = Some(self.poll_index);
            let due = self.body_reads.cycles == 0;
            self.body_reads.cycles = (self.body_reads.cycles + 1) % profile::TRANSPONDER_EVERY;
            if due {
                return (profile::TRANSPONDER_READ, PollKind::Transponder);
            }
        }
        let extras_on = profile == Profile::Drive && self.extras.every > 0;
        if extras_on && self.extras.due {
            self.extras.due = false;
            let n = EXTRA_SCHEDULE.len();
            for k in 0..n {
                let i = (self.extras.next + k) % n;
                if self.extras.unsupported & (1 << i) == 0 {
                    self.extras.next = (i + 1) % n;
                    return (EXTRA_SCHEDULE[i], PollKind::Extra(i));
                }
            }
            // Every extra frame was refused in this session: plain drive polling.
        }
        let pos = self.poll_index % schedule.len();
        let lid = schedule[pos];
        self.poll_index = self.poll_index.wrapping_add(1);
        if !extras_on {
            return (lid, PollKind::Plain);
        }
        if self.poll_index.is_multiple_of(schedule.len()) {
            self.extras.cycles = self.extras.cycles.saturating_add(1);
            if self.extras.cycles >= self.extras.every {
                self.extras.cycles = 0;
                self.extras.due = true;
            }
        }
        // Forward every 21A9 and the first 21AA of a cycle: the fields Live v1 does not carry
        // (throttle, oxygen sensors, advance, ...), each about 5 times per second.
        let forward = lid == 0xA9 || pos == 0;
        (
            lid,
            if forward {
                PollKind::Forward
            } else {
                PollKind::Plain
            },
        )
    }

    #[cfg_attr(not(feature = "approvals"), allow(unused_variables))]
    fn start_job(&mut self, ecu: EcuId, s: &mut Session, now: u64) -> JobStart {
        #[cfg(feature = "approvals")]
        let resample = self.fresh_speed(now).is_none() && self.moving_for(EcuId::Sirius, now);
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
            // A Sirius PREPARE that waited (a body read away from the Sirius) with the engine
            // running: the standstill it saw is no longer fresh, so it samples again instead
            // of being refused.
            #[cfg(feature = "approvals")]
            JobKind::Prepare {
                ecu: EcuId::Sirius,
                sample,
                ..
            } if !*sample && resample && ecu == EcuId::Sirius => {
                *sample = true;
                match s.request(&SIRIUS_SAMPLE, now) {
                    Ok(a) => JobStart::Exchange(a),
                    Err(_) => {
                        self.finish_job(ResultCode::VehicleMoving, Vec::new());
                        JobStart::Nothing
                    }
                }
            }
            #[cfg(feature = "approvals")]
            JobKind::Prepare { sample: true, .. } => match s.request(&SIRIUS_SAMPLE, now) {
                Ok(a) => JobStart::Exchange(a),
                Err(_) => {
                    self.finish_job(ResultCode::VehicleMoving, Vec::new());
                    JobStart::Nothing
                }
            },
            #[cfg(feature = "approvals")]
            JobKind::Prepare {
                payload,
                purpose,
                identification,
                ..
            } => {
                let (payload, purpose, ident) = (payload.clone(), *purpose, identification.clone());
                self.make_challenge(ecu, s, payload, purpose, &ident);
                JobStart::Nothing
            }
            #[cfg(feature = "approvals")]
            JobKind::Confirm { challenge, code } => {
                let (challenge, code) = (*challenge, code.clone());
                self.confirm_now(s, challenge, &code, now)
            }
            JobKind::Fob { lock, phase, seen } => {
                let lock = *lock;
                let seen = *seen;
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
                        if seen == Some(target) {
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
            JobKind::LockCheck => match s.request(&BMT_LOCK_READ, now) {
                Ok(a) => JobStart::Exchange(a),
                Err(_) => {
                    self.finish_job(ResultCode::LinkError, Vec::new());
                    JobStart::Nothing
                }
            },
            JobKind::BodyRead { next } => {
                let lid = profile::BODY_READ_SET[*next];
                // The body read has the doors: no pre-drive read after it.
                self.body_reads.predrive_done = true;
                if *next == 0 && self.sirius_owed_until.is_some() {
                    self.sirius_owed_until = Some(now + SIRIUS_OWED_MS);
                }
                // The car started moving while the bridge switched over: no body read.
                if self.last_speed.is_some_and(|(_, v)| v > 0) {
                    self.finish_job(ResultCode::VehicleMoving, Vec::new());
                    return JobStart::Nothing;
                }
                match s.request(&[0x21, lid], now) {
                    Ok(a) => JobStart::Exchange(a),
                    Err(_) => {
                        self.finish_job(ResultCode::LinkError, Vec::new());
                        JobStart::Nothing
                    }
                }
            }
        }
    }

    /// Queue the body read ([`profile::BODY_READ_SET`]) for this standstill; the answer to
    /// `client` (if any) follows the last read.
    fn queue_body_read(
        &mut self,
        client: Option<ClientId>,
        counter: u32,
    ) -> Result<(), ResultCode> {
        if self.yielded {
            return Err(ResultCode::Yielded);
        }
        if self.jobs.len() >= self.cfg.max_queue {
            return Err(ResultCode::Busy);
        }
        let now = self.now_ms;
        self.jobs.push_back(Job {
            client,
            counter,
            kind: JobKind::BodyRead { next: 0 },
            since_ms: now,
        });
        self.body_read_this_stop = true;
        self.last_body_read_ms = Some(now);
        self.sirius_owed_until = self.engine_running(now).then_some(now + SIRIUS_OWED_MS);
        Ok(())
    }

    /// READ_BODY_ONCE (owner decision 2026-11-25): only at a fresh standstill, at most once per
    /// stop, never while lock-when-moving owes an unlock (it must watch the engine stop). With
    /// the engine off the parked profile reads the body anyway: `Ok` at once.
    fn read_body_once(
        &mut self,
        client: ClientId,
        counter: u32,
        now: u64,
    ) -> Result<(), ResultCode> {
        if self.effective_profile(now) != Profile::Drive {
            return Ok(());
        }
        let speed = self
            .last_speed
            .filter(|(t, _)| now.saturating_sub(*t) <= self.cfg.speed_fresh_ms)
            .map(|(_, v)| v);
        if speed != Some(0) {
            return Err(ResultCode::VehicleMoving);
        }
        if self.lwm.locked_by_us() {
            return Err(ResultCode::LwmActive);
        }
        if self.body_read_this_stop || !self.body_read_gap_ok(now) {
            return Err(ResultCode::RateLimited);
        }
        // A challenge keeps its session (the owner is typing the code).
        if self.challenge_pending() {
            return Err(ResultCode::Busy);
        }
        self.queue_body_read(Some(client), counter)
    }

    /// A live approval challenge holds its session.
    fn challenge_pending(&self) -> bool {
        #[cfg(feature = "approvals")]
        return self.pending_live();
        #[cfg(not(feature = "approvals"))]
        false
    }

    /// The last body read was queued at least [`profile::BODY_READ_MIN_GAP_MS`] ago.
    fn body_read_gap_ok(&self, now: u64) -> bool {
        self.last_body_read_ms
            .is_none_or(|t| now.saturating_sub(t) >= profile::BODY_READ_MIN_GAP_MS)
    }

    #[cfg(feature = "approvals")]
    fn make_challenge(
        &mut self,
        ecu: EcuId,
        s: &mut Session,
        payload: Vec<u8>,
        purpose: Purpose,
        identification: &[u8],
    ) {
        let Some(job) = self.jobs.front() else {
            return;
        };
        let (client, counter) = (job.client, job.counter);
        // Battery, motion and tier again, now: the command may have waited in the queue.
        if !self.battery_ok() {
            return self.finish_job(ResultCode::LowBattery, Vec::new());
        }
        if self.moving_for(ecu, self.now_ms) {
            return self.finish_job(ResultCode::VehicleMoving, Vec::new());
        }
        // The tier on the session's real target (key bytes of the init that ran).
        let made = match kline_core::safety::tier_of_in(s.definition_guard(), &s.target(), &payload)
        {
            Ok(Tier::Service) => s.service_challenge(&payload),
            // The identification the app chose its definition from; kline-core refuses unless
            // it equals this session's latest positive `21 80` reply (like the CLI).
            Ok(Tier::ConfigWrite) => s.config_write_challenge(&payload, identification),
            Ok(Tier::Expert) => s.expert_challenge(&payload),
            Ok(Tier::Read) => return self.finish_job(ResultCode::Unsupported, Vec::new()),
            Err(d) => Err(SessionError::Denied(d)),
        };
        match made {
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
                    session_gen: self.session_gen,
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
            #[cfg(feature = "standing-lock")]
            if self.lwm_cancelled.take() == Some(challenge) {
                self.finish_job(ResultCode::LwmActive, Vec::new());
                return JobStart::Nothing;
            }
            self.finish_job(ResultCode::ApprovalMismatch, Vec::new());
            return JobStart::Nothing;
        };
        if p.id != challenge || now >= p.expires_ms {
            self.finish_job(ResultCode::ApprovalMismatch, Vec::new());
            return JobStart::Nothing;
        }
        // The session the challenge was made on ended (keep-alive lost, line error): never
        // write into a fresh session; the owner prepares again (review m-2).
        if p.session_gen != self.session_gen {
            self.finish_job(ResultCode::SessionLost, Vec::new());
            return JobStart::Nothing;
        }
        let approval = match Approval::confirm(p.challenge, code) {
            Ok(a) => a,
            Err(_) => {
                // A wrong code in the middle of arming leaves nothing half-armed; the armed
                // risks (`standing`) are not touched.
                #[cfg(feature = "standing-lock")]
                if p.purpose != Purpose::Send {
                    self.arming_lock = None;
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
                if self.moving_for(p.ecu, now) {
                    self.finish_job(ResultCode::VehicleMoving, Vec::new());
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
                // Kept apart from `standing` until both codes are confirmed (review B-1).
                self.arming_lock = Some(risk);
                self.make_challenge(EcuId::Bmt, s, BMT_UNLOCK.to_vec(), Purpose::ArmUnlock, &[]);
                JobStart::Nothing
            }
            #[cfg(feature = "standing-lock")]
            Purpose::ArmUnlock => {
                let risk = *approval.risk();
                drop(approval);
                match self.arming_lock.take() {
                    // `arm` refuses while an unlock is owed, so nothing relies on the old risks.
                    Some(lock) if !self.lwm.locked_by_us() => {
                        self.standing = Some(Standing {
                            lock: Some(lock),
                            unlock: Some(risk),
                        });
                        self.lwm.arm();
                        self.finish_job(ResultCode::Ok, Vec::new());
                    }
                    _ => self.finish_job(ResultCode::ApprovalMismatch, Vec::new()),
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
                self.lwm.intent_done(now, intent, Outcome::NotExecuted);
                self.jobs.pop_front();
                return JobStart::Nothing;
            }
        };
        if intent == Intent::Lock && !self.battery_ok() {
            self.lwm.intent_done(now, intent, Outcome::NotExecuted);
            self.jobs.pop_front();
            return JobStart::Nothing;
        }
        let ok = match s.service_challenge(&payload) {
            Ok(ch)
                if standing_allows(&payload, ch.payload(), ch.target(), ch.risk(), &armed_risk) =>
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
                self.lwm.intent_done(now, intent, Outcome::NotExecuted);
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
            self.lwm
                .intent_done(self.now_ms, intent, Outcome::NotExecuted);
        }
    }

    // -----------------------------------------------------------------------------------------
    // Exchange results

    fn finished(&mut self, ecu: EcuId, op: Exch, r: Result<SessionOutcome, SessionError>) {
        match (op, r) {
            (Exch::Poll(lid), Ok(SessionOutcome::Reply(Reply::Positive(p)))) => {
                // The Extra frame first, so the app has the raw fields of a 21A9 before the
                // Live notification it belongs to.
                match self.poll_kind {
                    PollKind::Plain => {}
                    PollKind::Transponder => {
                        // Published at the first read of every BMT session, whatever the bit
                        // says, and at every change of the ignition bit (kept across sessions),
                        // so an ignition-off after a drive is seen (review r2).
                        let ign = p.len() > 3 && p[0] == 0x61 && p[1] == lid && p[3] & 0x01 != 0;
                        let prev = self.body_reads.ignition;
                        if !self.body_reads.transponder_sent || prev != Some(ign) {
                            self.extra_frame(ecu, lid, p.clone());
                            self.body_reads.transponder_sent = true;
                        }
                        self.body_reads.ignition = Some(ign);
                        if ign {
                            // The ignition is on: the Sirius should answer now (look again).
                            self.sirius_silent_at = None;
                        }
                    }
                    PollKind::PreDrive | PollKind::Forward | PollKind::Extra(_) => {
                        self.extra_frame(ecu, lid, p.clone())
                    }
                }
                self.positive(ecu, &p);
            }
            (Exch::Poll(_), Ok(SessionOutcome::Reply(Reply::Negative { .. }))) => {
                // The ECU does not know this extra frame: skip it for the rest of the session.
                if let PollKind::Extra(i) = self.poll_kind {
                    self.extras.unsupported |= 1 << i;
                }
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

    /// Forward one raw poll reply as an Extra frame (live extras on).
    fn extra_frame(&mut self, ecu: EcuId, lid: u8, reply: Vec<u8>) {
        if self.clients.is_empty() {
            return;
        }
        let f = ExtraFrame {
            seq: self.extras.seq,
            t_ms: self.now_ms as u32,
            ecu: ecu.address(),
            request: alloc::vec![0x21, lid],
            reply,
        };
        // A reply too long for one frame is not sent (never truncated).
        if let Ok(frame) = f.encode() {
            self.extras.seq = self.extras.seq.wrapping_add(1);
            self.out.push_back(Output::Notify {
                to: None,
                channel: Channel::Extra,
                frame,
            });
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
            let outcome = match code {
                ResultCode::Ok => Outcome::Done,
                // Written, reply lost: it may have executed (review M4).
                ResultCode::LinkError if self.job_written => Outcome::MaybeExecuted,
                _ => Outcome::NotExecuted,
            };
            self.lwm.intent_done(self.now_ms, i, outcome);
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
            // No Sirius sample: nothing shows the car stands with the engine off.
            #[cfg(feature = "approvals")]
            JobKind::Prepare { sample: true, .. } => {
                self.finish_job(ResultCode::VehicleMoving, Vec::new())
            }
            _ => self.job_finished_with(ResultCode::LinkError),
        }
    }

    fn job_reply(&mut self, ecu: EcuId, reply: Reply) {
        if let Reply::Positive(p) = &reply {
            self.positive(ecu, p);
        }
        #[cfg(feature = "approvals")]
        if let Some(Job {
            kind: JobKind::Prepare { ecu, sample, .. },
            ..
        }) = self.jobs.front_mut()
        {
            if *sample {
                // The sample (if positive) was recorded above; decide with it.
                *sample = false;
                let ecu = *ecu;
                if self.moving_for(ecu, self.now_ms) {
                    self.finish_job(ResultCode::VehicleMoving, Vec::new());
                }
                return;
            }
        }
        if let Some(Job {
            kind: JobKind::BodyRead { next },
            ..
        }) = self.jobs.front()
        {
            let next = *next;
            let lid = profile::BODY_READ_SET[next];
            let mut moving = false;
            if let Reply::Positive(p) = &reply {
                self.extra_frame(EcuId::Bmt, lid, p.clone());
                moving = live::bmt_speed_kmh(p).is_some_and(|v| v > 0);
            }
            if moving {
                // The car drives off: this request is done, the rest is dropped, back to the
                // Sirius (lock-when-moving and the engine data need it).
                self.stopped_since = None;
                self.finish_job(ResultCode::VehicleMoving, Vec::new());
            } else if next + 1 >= profile::BODY_READ_SET.len() {
                self.finish_job(ResultCode::Ok, Vec::new());
            } else if let Some(Job {
                kind: JobKind::BodyRead { next },
                ..
            }) = self.jobs.front_mut()
            {
                // A refused frame is skipped; the next request goes out on the next pass.
                *next += 1;
            }
            return;
        }
        let Some(job) = self.jobs.front_mut() else {
            return;
        };
        if let JobKind::Fob { lock, phase, seen } = &mut job.kind {
            let target = if *lock {
                LockState::Locked
            } else {
                LockState::Unlocked
            };
            // Only a positive `61 A3` of this job counts; a negative reply or anything else
            // leaves the state unknown (review M1: never decide on a stale value).
            let fresh = match &reply {
                Reply::Positive(p) => match live::bmt(p) {
                    Some(BmtFrame::Lock(l)) if l != LockState::Unknown => Some(l),
                    _ => None,
                },
                _ => None,
            };
            *seen = fresh;
            if *phase == FobPhase::ReadState {
                match fresh {
                    None => {
                        self.finish_job(ResultCode::LockNotConfirmed, Vec::new());
                    }
                    Some(s) if s == target => {
                        self.finish_job(ResultCode::Ok, Vec::new());
                    }
                    Some(_) => *phase = FobPhase::NeedPulse,
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
            self.lwm.intent_done(self.now_ms, i, Outcome::NotExecuted);
            return;
        }
        // A Sirius sample that never came: the car may move (yield and battery keep theirs).
        #[cfg(feature = "approvals")]
        let code = match (&job.kind, code) {
            (JobKind::Prepare { sample: true, .. }, ResultCode::LinkError) => {
                ResultCode::VehicleMoving
            }
            _ => code,
        };
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
                // The Sirius was tried again after a body read: its own samples decide now.
                self.sirius_tried();
                // (A Sirius answer needs no step here: an rpm above 0 is a running engine, an
                // rpm 0 21A9 at a charging voltage is the engine-off proof.)
                let mut changed = false;
                if let Some(v) = live::sirius_aa(p) {
                    self.live.rpm = v.rpm;
                    self.live.inj_us = v.inj_us;
                    self.live.fuel_mlh = v.fuel_mlh;
                    self.live.flags |= live_flags::AA_VALID;
                    self.last_engine_sample = Some((now, v.rpm));
                    if v.rpm > 0 {
                        self.engine_off_at = None;
                        self.soc.on_engine_running(now);
                    }
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
                    self.last_speed = Some((now, v.speed_kmh));
                    self.engine_off_at = if v.rpm == 0 && v.speed_kmh == 0 {
                        Some((now, self.charging(), self.battery_mv.unwrap_or(0)))
                    } else {
                        None
                    };
                    changed = true;
                    if let Some(intent) = self.lwm.on_sample(now, v.speed_kmh, v.rpm) {
                        self.lwm_intent(intent);
                    }
                    // Standstill tracking for the body read (at most one per stop). An engine
                    // stop ends the drive profile; the next drive's new Sirius session starts
                    // afresh (see the session start).
                    if v.speed_kmh == 0 {
                        let since = *self.stopped_since.get_or_insert(now);
                        // Automatic: at every stop over 3 s with the engine running, at most
                        // once per stop and per 30 s; never while lock-when-moving is armed (the
                        // read would delay a lock by up to about 11 s, review r5 MAJOR-B).
                        if v.rpm > 0
                            && now.saturating_sub(since) > AUTO_BODY_READ_AFTER_MS
                            && !self.body_read_this_stop
                            && self.body_read_gap_ok(now)
                            && !self.challenge_pending()
                            && !self.lwm.locked_by_us()
                            && !self.lwm.is_armed()
                            && self.jobs.is_empty()
                        {
                            let _ = self.queue_body_read(None, 0);
                        }
                    } else {
                        self.stopped_since = None;
                        self.body_read_this_stop = false;
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
                    if self.extras.every > 0 {
                        flags |= live_flags::EXTRAS_ON;
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
            EcuId::Abs | EcuId::Airbag => {}
            EcuId::Bmt => match live::bmt(p) {
                Some(BmtFrame::Lock(l)) => {
                    self.parked.lock = l;
                    self.parked.lock_at_ms = Some(now);
                    // Only an observed locked -> unlocked change ends an owed unlock (review
                    // B4-1; lock memory and polarity NOT VERIFIED).
                    match l {
                        LockState::Locked => self.lwm.on_lock_read(true),
                        LockState::Unlocked => self.lwm.on_lock_read(false),
                        LockState::Unknown => {}
                    }
                    self.drop_standing_if_done();
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
        // A yield always wins: also against a running engine (review M6).
        if self.yielded || self.asleep || self.critical_battery(now) || self.low_battery_sleep(now)
        {
            return None;
        }
        if self.stuck_awake(now) {
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

    /// Drive whenever the engine may run (so lock-when-moving and the dashboard work whatever
    /// the app requested), else the requested profile ([`profile::select`]).
    fn effective_profile(&self, now: u64) -> Profile {
        // Lock-when-moving needs the Sirius (to see the engine stop; a Sirius that no longer
        // answers is the ignition-off signal, review M9), and a running engine always gets the
        // drive profile, whatever was requested (owner decision 2026-11-25: the app asks for
        // parked and relies on this).
        profile::select(
            self.requested,
            self.engine_may_run_for_profile(now),
            self.lwm_holds_sirius(now),
        )
    }

    /// For the profile choice: rpm above 0 in a recent Sirius frame, or a charging voltage that
    /// nothing explains as a charger. A charger (13.2 V or more with the engine off) is told
    /// apart by the Sirius itself: a 21A9 with 0 rpm taken while the voltage was already that
    /// high (no dip or rise since, the engine-off proof), or a Sirius that does not answer at all
    /// (ignition off). Either holds for the challenge lifetime (120 s); then the bridge looks
    /// at the Sirius again, so an engine started on a charger is still found.
    fn engine_may_run_for_profile(&self, now: u64) -> bool {
        if self.engine_running(now) {
            return true;
        }
        if !self.charging() {
            return false;
        }
        let ttl = self.cfg.challenge_ttl_ms;
        let off_proof = self.engine_off_at.is_some_and(|(t, charging_then, _)| {
            charging_then && now.saturating_sub(t) <= ttl && self.last_dip_ms.is_none_or(|d| d < t)
        });
        // A dip since the proof also ends `charger_like` (the charging state restarts).
        let silent = self
            .sirius_silent_at
            .is_some_and(|t| now.saturating_sub(t) <= ttl && self.charger_like(now));
        !(off_proof || silent)
    }

    /// The stuck-awake watchdog (owner decision 2026-11-25): no client for `stuck_awake_ms`, not
    /// in the drive profile, nothing queued. (An owed unlock holds the drive profile, so it is
    /// covered by the profile check.)
    fn stuck_awake(&self, now: u64) -> bool {
        self.clients.is_empty()
            && self.jobs.is_empty()
            && self.effective_profile(now) != Profile::Drive
            && now.saturating_sub(self.last_client_ms) >= self.cfg.stuck_awake_ms
    }

    /// The charging voltage looks like a charger: no dip below the write limit since it began
    /// and none in the [`START_WINDOW_MS`] before (a start pulls the battery far down, then the
    /// alternator brings it up), no rise by `start_rise_mv` since it began (an alternator on top
    /// of a charger), and no rpm above 0 within the challenge lifetime.
    fn charger_like(&self, now: u64) -> bool {
        let ttl = self.cfg.challenge_ttl_ms;
        self.charging_since.is_some_and(|(t, mv0)| {
            self.last_dip_ms
                .is_none_or(|d| d.saturating_add(START_WINDOW_MS) < t)
                && self
                    .battery_mv
                    .is_some_and(|mv| mv <= mv0.saturating_add(self.cfg.start_rise_mv))
        }) && !self
            .last_engine_sample
            .is_some_and(|(t, rpm)| rpm > 0 && now.saturating_sub(t) <= ttl)
    }

    /// Lock-when-moving needs the Sirius session now: an unlock is owed, or it is armed and the
    /// engine runs. Then no profile switch, no other ECU and no yield (review B1).
    fn lwm_holds_sirius(&self, now: u64) -> bool {
        self.lwm.locked_by_us() || (self.lwm.is_armed() && self.engine_running(now))
    }

    /// The bridge tried the Sirius and it did not answer.
    /// The Sirius answered or failed: unless a body read is still queued (the reply of a poll
    /// in flight when it was queued), the pause is over.
    fn sirius_tried(&mut self) {
        if !self
            .jobs
            .iter()
            .any(|j| matches!(j.kind, JobKind::BodyRead { .. }))
        {
            self.sirius_owed_until = None;
        }
    }

    /// See [`Bridge::sirius_owed_until`].
    fn sirius_owed(&self, now: u64) -> bool {
        self.sirius_owed_until.is_some_and(|u| now < u)
    }

    fn sirius_failed(&mut self) {
        self.sirius_tried();
        if let Some(intent) = self.lwm.on_sirius_failure(self.now_ms) {
            self.lwm_intent(intent);
        }
    }

    /// A non-read to `ecu` is refused now (reviews M-1 and r3). A fresh (2 s) Sirius speed
    /// above 0 refuses. A fresh standstill is enough on the Sirius itself (its CONFIRM runs on
    /// the Sirius). Otherwise, and always for the other ECUs (their CONFIRM runs on their own
    /// session, without a speed), the engine must be known off: see [`Bridge::engine_off_known`].
    #[cfg_attr(not(feature = "approvals"), allow(dead_code))]
    fn moving_for(&self, ecu: EcuId, now: u64) -> bool {
        match self.fresh_speed(now) {
            Some(v) if v > 0 => true,
            Some(_) if ecu == EcuId::Sirius => false,
            _ => !self.engine_off_known(now),
        }
    }

    #[cfg_attr(not(feature = "approvals"), allow(dead_code))]
    fn fresh_speed(&self, now: u64) -> Option<u8> {
        self.last_speed
            .filter(|(t, _)| now.saturating_sub(*t) <= self.cfg.speed_fresh_ms)
            .map(|(_, v)| v)
    }

    /// The engine is off as far as the bridge can tell: a Sirius `21 A9` showed 0 km/h and
    /// 0 rpm within the challenge lifetime and nothing since says the engine started (no
    /// battery dip below the write limit, no rise to a charging voltage), whatever a charger
    /// holds the voltage at; or there is no sign at all that it runs.
    #[cfg_attr(not(feature = "approvals"), allow(dead_code))]
    fn engine_off_known(&self, now: u64) -> bool {
        let proof = self.engine_off_at.is_some_and(|(t, charging_then, _)| {
            now.saturating_sub(t) <= self.cfg.challenge_ttl_ms
                && self.last_dip_ms.is_none_or(|d| d < t)
                && (charging_then || !self.charging())
        });
        proof || !self.engine_may_run(now)
    }

    /// Charging voltage, or rpm above 0 seen within the challenge lifetime.
    #[cfg_attr(not(feature = "approvals"), allow(dead_code))]
    fn engine_may_run(&self, now: u64) -> bool {
        self.charging()
            || self.last_engine_sample.is_some_and(|(t, rpm)| {
                rpm > 0 && now.saturating_sub(t) <= self.cfg.challenge_ttl_ms
            })
    }

    fn charging(&self) -> bool {
        self.battery_mv.is_some_and(|mv| mv >= self.cfg.charging_mv)
    }

    /// rpm > 0 in a Sirius frame of the last 3 s (the LWM silence time).
    fn engine_running(&self, now: u64) -> bool {
        self.last_engine_sample.is_some_and(|(t, rpm)| {
            rpm > 0 && (self.sirius_owed(now) || now.saturating_sub(t) < self.cfg.lwm.silence_ms)
        })
    }

    /// Ignition on as far as the bridge can tell: engine running or charging voltage.
    fn ignition_on(&self, now: u64) -> bool {
        self.engine_running(now) || self.charging()
    }

    /// At or above the write limit now (an unknown voltage counts as low: no writes blind).
    /// Refusing a write on one low sample is the safe side, so this is not debounced.
    fn battery_ok(&self) -> bool {
        self.battery_mv
            .is_some_and(|mv| mv >= self.cfg.low_battery_mv)
    }

    /// MASTER-PLAN-v2 P6: below 11.8 V switch off, after the debounce (review M3), and never
    /// with the engine running or an unlock owed.
    fn critical_battery(&self, now: u64) -> bool {
        self.critical_since
            .is_some_and(|t| now.saturating_sub(t) >= self.cfg.critical_battery_debounce_ms)
            && !self.engine_running(now)
            && !self.lwm.locked_by_us()
    }

    /// Sessions (reads) are allowed down to the critical limit; writes need `battery_ok`.
    fn battery_allows_session(&self) -> bool {
        !self.critical_battery(self.now_ms)
    }

    /// Below the write limit for the debounce time with the engine off: stop and sleep
    /// (MASTER-PLAN-v2 P6). Not with the engine running (then the reading is a dip or a
    /// measurement problem) and not while lock-when-moving owes an unlock (unlocking is the safe
    /// direction and goes first, 3.1 rule 2).
    fn low_battery_sleep(&self, now: u64) -> bool {
        self.low_since
            .is_some_and(|t| now.saturating_sub(t) >= self.cfg.low_battery_debounce_ms)
            && !self.engine_running(now)
            && !self.lwm.locked_by_us()
    }

    /// When the bridge should sleep now (link already off), how long and why.
    fn should_sleep(&self, now: u64) -> Option<(u64, SleepReason)> {
        if self.critical_battery(now) {
            return Some((self.cfg.deep_wake_ms, SleepReason::CriticalBattery));
        }
        if self.low_battery_sleep(now) {
            return Some((self.cfg.deep_wake_ms, SleepReason::LowBattery));
        }
        if !self.jobs.is_empty() || self.lwm.locked_by_us() || self.engine_running(now) {
            return None;
        }
        let profile = self.effective_profile(now);
        let idle = self.clients.is_empty()
            && profile == Profile::Off
            && now.saturating_sub(self.last_activity_ms) >= self.cfg.idle_sleep_ms;
        // The stuck-awake watchdog: no client and not in the drive profile for 10 min (a parked
        // request outlives the phone that made it).
        let stuck = self.stuck_awake(now);
        if !idle && !stuck {
            return None;
        }
        // Light sleep with BLE advertising for at most 48 h after the engine last ran (since
        // boot when it has not run since), then deep sleep with a timer wake.
        let since_run = now.saturating_sub(self.soc.last_run_ms().unwrap_or(0));
        if since_run >= self.cfg.parked_light_max_ms {
            return Some((self.cfg.deep_wake_ms, SleepReason::ParkedLong));
        }
        Some((
            self.cfg.idle_wake_ms,
            if idle {
                SleepReason::Idle
            } else {
                SleepReason::NoClient
            },
        ))
    }

    // -----------------------------------------------------------------------------------------
    // Notifications

    fn respond(&mut self, to: Option<ClientId>, counter: u32, result: ResultCode, data: Vec<u8>) {
        let frame = match (Response {
            counter,
            result,
            data,
        })
        .encode()
        {
            Ok(f) => f,
            // Never truncated: the client gets the code that says the reply did not fit.
            Err(_) => Response {
                counter,
                result: ResultCode::ReplyTooLong,
                data: Vec::new(),
            }
            .encode()
            .expect("empty data fits"),
        };
        self.out.push_back(Output::Notify {
            to,
            channel: Channel::Response,
            frame,
        });
    }

    /// Raw notifications, split into frames of at most [`crate::gatt::MAX_RAW_BYTES`].
    fn raw_frame(&mut self, dir: RawDir, bytes: Vec<u8>) {
        let ecu = match &self.link {
            Link::Init { ecu, .. } | Link::Ready { ecu, .. } | Link::Busy { ecu, .. } => {
                ecu.address()
            }
            _ => 0,
        };
        for chunk in bytes.chunks(crate::gatt::MAX_RAW_BYTES) {
            let f = RawFrame {
                t_ms: self.now_ms as u32,
                dir,
                ecu,
                bytes: chunk.to_vec(),
            };
            if let Ok(frame) = f.encode() {
                self.out.push_back(Output::Notify {
                    to: None,
                    channel: Channel::Raw,
                    frame,
                });
            }
        }
    }
}

/// The standing self-approval of D37 answers kline-core's challenge only when it is for
/// exactly one of the two armed BMT payloads, on the BMT, with the risk the owner confirmed.
#[cfg(feature = "standing-lock")]
fn standing_allows(
    wanted: &[u8],
    challenge_payload: &[u8],
    challenge_target: kline_core::safety::Target,
    challenge_risk: &kline_core::safety::RiskInfo,
    armed_risk: &kline_core::safety::RiskInfo,
) -> bool {
    (wanted == BMT_LOCK || wanted == BMT_UNLOCK)
        && challenge_payload == wanted
        && challenge_target.ecu_address == EcuId::Bmt.address()
        && challenge_target.protocol == EcuId::Bmt.protocol()
        && challenge_risk == armed_risk
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
        JobKind::Prepare { sample: true, .. } => Some(EcuId::Sirius),
        #[cfg(feature = "approvals")]
        JobKind::Prepare { ecu, .. } => Some(*ecu),
        #[cfg(feature = "approvals")]
        JobKind::Confirm { .. } => None,
        JobKind::Fob { .. } | JobKind::LockCheck | JobKind::BodyRead { .. } => Some(EcuId::Bmt),
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

#[cfg(all(test, feature = "standing-lock"))]
mod tests {
    use super::*;
    use kline_core::safety::{assess, Protocol, Target};

    fn bmt() -> Target {
        Target {
            protocol: Protocol::Kwp,
            ecu_address: 0x26,
            keywords: Some((0xD0, 0x8F)),
        }
    }

    /// Review M6 (surviving mutation): the risk the owner confirmed must equal the challenge's.
    #[test]
    fn standing_self_approval_needs_the_exact_armed_risk_payload_and_target() {
        let risk = assess(&bmt(), &BMT_LOCK).unwrap();
        assert!(standing_allows(&BMT_LOCK, &BMT_LOCK, bmt(), &risk, &risk));
        let mut other = risk;
        other.what_can_go_wrong = "something else";
        assert!(!standing_allows(&BMT_LOCK, &BMT_LOCK, bmt(), &risk, &other));
        assert!(!standing_allows(
            &BMT_LOCK,
            &BMT_UNLOCK,
            bmt(),
            &risk,
            &risk
        ));
        let wrong = [0x31, 0xA2, 0x00, 0x01];
        assert!(!standing_allows(&wrong, &wrong, bmt(), &risk, &risk));
        let sirius = Target {
            ecu_address: 0x7A,
            ..bmt()
        };
        assert!(!standing_allows(&BMT_LOCK, &BMT_LOCK, sirius, &risk, &risk));
    }
}
