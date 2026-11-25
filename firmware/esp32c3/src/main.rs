//! Clio Link bridge firmware skeleton for the ESP32-C3 (MASTER-PLAN-v2 W4 deliverable 4;
//! finished in W7). Pinout and wiring: docs/bridge-hardware.md.
//!
//! What runs here today: the kline-bridge state machine over the real UART and GPIOs, the fob
//! output driver (low before anything else), the RTC watchdog, the K-line power switch, the
//! battery divider on the ADC, light sleep in watchdog-fed slices, deep sleep with a 30 s timer
//! wake for the long parked and low-battery sleeps (kline-bridge `power`), the TX pin held low
//! while the transceiver is off. What is not here yet (W7): BLE (the `BleSink` below drops notifications;
//! `--features bench` prints live samples on the USB serial console instead), BLE wake-up and
//! the 32 kHz crystal, NVS for the command counters and the yield holder
//! (`Bridge::restore_counters`, `Bridge::restore_yield` are ready for it).
//!
//! 5-baud init: the L9637D TX input is driven by UART1 TX through 1 kOhm, and `BREAK_N`
//! (open drain) pulls that node low. kline-core's `SetBreak(true)` / `SetBreak(false)` with its
//! 200 ms `Sleep`s therefore bit-bang the address byte while UART1 stays configured at
//! 10400 8N1 for everything after it.

#![no_std]
#![no_main]

extern crate alloc;

use esp_hal::analog::adc::{Adc, AdcConfig, AdcPin, Attenuation};
use esp_hal::gpio::{DriveMode, Level, Output, OutputConfig};
use esp_hal::peripherals::{ADC1, GPIO3, GPIO4, GPIO5, GPIO6, GPIO7, UART1};
use esp_hal::rtc_cntl::sleep::{LowPower, RtcSleepConfig};
use esp_hal::rtc_cntl::{reset_reason, Rtc, Rwdt, RwdtStage, SocResetReason};
use esp_hal::system::Cpu;
use esp_hal::time::{Duration, Instant};
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_hal::{main, Blocking};
use kline_bridge::bridge::{Bridge, BridgeConfig, SleepReason};
use kline_bridge::fob::{FobDriver, FobTiming};
use kline_bridge::gatt::Channel;
use kline_bridge::hal::{BleSink, Clock, KLinePort, OutputPin, PortError, Power, Watchdog};
use kline_bridge::power::{DeepWake, SleepMode, DEEP_WAKE_MS};
use kline_bridge::ratelimit::ClientId;
use kline_bridge::runner::{self, Stepped};
use kline_core::{Action, Event};

esp_bootloader_esp_idf::esp_app_desc!();

/// kline-core's approval codes and nonces come from getrandom; on the C3 they come from the
/// hardware RNG with the SAR ADC entropy source switched on for the call (esp-hal `TrngSource`
/// and `Trng`, review m-3): the esp-hal docs say the RNG is a true RNG while an ADC (or the radio)
/// feeds it noise. NOT VERIFIED on the chip (statistics of the output were not measured).
///
/// # Safety
///
/// getrandom calls this with a valid, writable `dest` of `len` bytes.
#[cfg(feature = "approvals")]
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    use esp_hal::rng::{Trng, TrngSource};
    // SAFETY: the caller guarantees `dest` is valid for `len` writable bytes.
    let buf = unsafe { core::slice::from_raw_parts_mut(dest, len) };
    // SAFETY: single-threaded loop; ADC1 is otherwise taken only inside `battery_millivolts`,
    // never during this call, and RNG has no other user.
    let source = TrngSource::new(unsafe { esp_hal::peripherals::RNG::steal() }, unsafe {
        ADC1::steal()
    });
    let result = match Trng::try_new() {
        Ok(trng) => {
            trng.read(buf);
            Ok(())
        }
        Err(_) => Err(getrandom::Error::UNSUPPORTED),
    };
    drop(source);
    result
}

/// K-line UART speed: 10400 8N1 (docs/protocol.md "Line settings").
const K_BAUD: u32 = 10_400;
/// Battery divider 1 MOhm / 180 kOhm (PLAN-RESEARCH B5): V_bat = V_adc * 1180 / 180.
const DIVIDER_NUM: u32 = 1_180;
const DIVIDER_DEN: u32 = 180;
/// Watchdog: a step that blocks longer than this resets the chip. ours: the longest blocking
/// action is a 5000 ms responsePending read (kline-core `RESPONSE_PENDING_TIMEOUT_MS`).
const WATCHDOG_MS: u64 = 8_000;
/// Battery reading period, also inside blocking K-line actions (a 5-baud init blocks for about
/// 2 s). ours: a start (cranking, about 1-2 s) must show up as a reading below 12.0 V, which
/// voids the bridge's engine-off sample (docs/bridge-protocol.md "Battery charger"); 100 ms
/// sees even a short one. NOT VERIFIED on the car.
const BATTERY_PERIOD_MS: u64 = 100;

/// Battery readings taken while a K-line action blocks the main loop (review M4-1): the lowest
/// one is handed to the bridge after the action.
#[derive(Default)]
struct BatteryWatch {
    next_ms: u64,
    lowest: Option<u16>,
}

impl BatteryWatch {
    /// Read the battery if the period is over; keep the lowest reading.
    fn tick(&mut self) {
        let now = Instant::now().duration_since_epoch().as_millis();
        if now < self.next_ms {
            return;
        }
        self.next_ms = now + BATTERY_PERIOD_MS;
        if let Some(mv) = battery_millivolts() {
            self.lowest = Some(self.lowest.map_or(mv, |l| l.min(mv)));
        }
    }

    fn take_lowest(&mut self) -> Option<u16> {
        self.lowest.take()
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // First the fob output low: on the target a panic does not unwind, so `FobDriver`'s Drop
    // never runs (MASTER-PLAN-v2 3.1 rule 3). The K line is released too.
    // SAFETY: the program is ending; nothing else uses these pins after this point, and
    // driving them low is the safe state of both outputs.
    let (_fob, _k_pwr) = unsafe {
        (
            Output::new(GPIO7::steal(), Level::Low, OutputConfig::default()),
            Output::new(GPIO6::steal(), Level::Low, OutputConfig::default()),
        )
    };
    esp_println::println!("panic: {info}");
    // Reset now, without relying on the watchdog (it is off around a deep sleep); the
    // pull-downs hold both outputs off during the reset.
    esp_hal::system::software_reset()
}

/// UART1 + BREAK_N + K_PWR around the L9637D.
struct KLine<'d> {
    /// UART1 on GPIO5 (TX) / GPIO4 (RX) while the transceiver is powered.
    uart: Option<Uart<'static, Blocking>>,
    /// GPIO5 driven low while the transceiver is off, so the TX pin never back-powers the
    /// unpowered L9637D through its TX input (review minor, docs/bridge-hardware.md).
    tx_low: Option<Output<'static>>,
    break_n: Output<'d>,
    k_pwr: Output<'d>,
    /// Bytes seen on RX since the last break started (break loopback, docs/protocol.md
    /// "5-baud init" step 3).
    rx_during_break: bool,
    in_break: bool,
    battery: BatteryWatch,
}

impl KLine<'_> {
    fn drain(&mut self) -> usize {
        let Some(uart) = self.uart.as_mut() else {
            return 0;
        };
        let mut buf = [0u8; 32];
        let mut n = 0;
        while uart.read_ready() {
            match uart.read_buffered(&mut buf) {
                Ok(0) => break,
                Ok(k) => n += k,
                // A break reads as a framing error: that is the loopback we look for.
                Err(_) => n += 1,
            }
        }
        n
    }
}

impl KLinePort for KLine<'_> {
    fn perform(&mut self, action: &Action) -> Result<Event, PortError> {
        Ok(match action {
            // No DTR on the bridge (docs/bridge-hardware.md).
            Action::SetDtr(_) => Event::Done,
            Action::SetBreak(on) => {
                if *on {
                    self.drain();
                    self.rx_during_break = false;
                    self.in_break = true;
                    self.break_n.set_low();
                } else {
                    self.break_n.set_high();
                    self.in_break = false;
                }
                Event::Done
            }
            Action::Sleep { ms } => {
                let end = Instant::now() + Duration::from_millis(u64::from(*ms));
                while Instant::now() < end {
                    if self.in_break && self.drain() > 0 {
                        self.rx_during_break = true;
                    }
                    self.battery.tick();
                }
                Event::Done
            }
            Action::CheckBreakLoopback => {
                if self.drain() > 0 {
                    self.rx_during_break = true;
                }
                Event::BreakLoopbackSeen(self.rx_during_break)
            }
            Action::FlushRx => {
                self.drain();
                Event::Done
            }
            Action::Write {
                frame,
                inter_byte_ms,
            } => {
                let bytes = frame.bytes();
                let uart = self.uart.as_mut().ok_or(PortError)?;
                if *inter_byte_ms == 0 {
                    write_all(uart, bytes)?;
                } else {
                    for (i, b) in bytes.iter().enumerate() {
                        if i > 0 {
                            busy_ms(u64::from(*inter_byte_ms));
                            self.battery.tick();
                        }
                        write_all(uart, core::slice::from_ref(b))?;
                    }
                }
                uart.flush().map_err(|_| PortError)?;
                Event::Done
            }
            Action::Read { count, timeout_ms } => {
                // Total timeout for the whole read (docs/protocol.md "Sans-IO contract").
                let end = Instant::now() + Duration::from_millis(u64::from(*timeout_ms));
                let mut out = alloc::vec::Vec::with_capacity(*count);
                let mut b = [0u8; 1];
                let uart = self.uart.as_mut().ok_or(PortError)?;
                while out.len() < *count && Instant::now() < end {
                    if uart.read_ready() {
                        if let Ok(1) = uart.read_buffered(&mut b) {
                            out.push(b[0]);
                        }
                    } else {
                        // The UART FIFO holds the bytes meanwhile; a reading is microseconds.
                        self.battery.tick();
                    }
                }
                if out.len() == *count {
                    Event::ReadOk(out)
                } else {
                    Event::ReadTimeout(out)
                }
            }
        })
    }

    /// On: release the TX pin, UART1 back (TX idles high), then K_PWR. Off: K_PWR first, then
    /// the UART away and TX low. The order keeps K recessive and the unpowered transceiver's TX
    /// input low.
    fn set_power(&mut self, on: bool) {
        if on {
            if self.uart.is_none() {
                self.tx_low = None;
                self.uart = Some(new_uart());
            }
            self.k_pwr.set_high();
        } else {
            self.k_pwr.set_low();
            self.uart = None;
            if self.tx_low.is_none() {
                self.tx_low = Some(tx_pin_low());
            }
        }
    }
}

/// UART1 on GPIO5/GPIO4 at 10400 8N1.
fn new_uart() -> Uart<'static, Blocking> {
    // SAFETY: `main` gives up UART1, GPIO4 and GPIO5 at start, and the only other user of
    // these pins (`tx_low`) has been dropped before this call (`KLine::set_power`).
    let (uart, tx, rx) = unsafe { (UART1::steal(), GPIO5::steal(), GPIO4::steal()) };
    Uart::new(uart, UartConfig::default().with_baudrate(K_BAUD))
        .expect("UART1 config")
        .with_tx(tx)
        .with_rx(rx)
}

/// GPIO5 as a plain output driven low.
fn tx_pin_low() -> Output<'static> {
    // SAFETY: the UART that owned GPIO5 has been dropped before this call.
    Output::new(
        unsafe { GPIO5::steal() },
        Level::Low,
        OutputConfig::default(),
    )
}

fn write_all(uart: &mut Uart<'_, Blocking>, mut bytes: &[u8]) -> Result<(), PortError> {
    while !bytes.is_empty() {
        let n = uart.write(bytes).map_err(|_| PortError)?;
        bytes = &bytes[n..];
    }
    Ok(())
}

fn busy_ms(ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {}
}

/// Everything the runner needs besides the fob pin.
struct Board<'d> {
    kline: KLine<'d>,
    rwdt: Rwdt,
    low_power: LowPower<'d>,
}

impl KLinePort for Board<'_> {
    fn perform(&mut self, action: &Action) -> Result<Event, PortError> {
        self.kline.perform(action)
    }
    fn set_power(&mut self, on: bool) {
        self.kline.set_power(on)
    }
}

impl Clock for Board<'_> {
    fn now_ms(&self) -> u64 {
        Instant::now().duration_since_epoch().as_millis()
    }
    fn wait_until(&mut self, t_ms: u64) {
        while self.now_ms() < t_ms {
            self.rwdt.feed();
        }
    }
}

impl BleSink for Board<'_> {
    fn notify(&mut self, _to: Option<ClientId>, channel: Channel, frame: &[u8]) {
        // W7: GATT notifications. Until then, bench builds print the live samples.
        if cfg!(feature = "bench") && channel == Channel::Live {
            if let Ok(s) = kline_bridge::gatt::LiveSample::decode(frame) {
                esp_println::println!(
                    "live seq={} rpm={} speed={} coolant={} batt_cv={}",
                    s.seq,
                    s.rpm,
                    s.speed_kmh,
                    s.coolant_c,
                    s.batt_cv
                );
            }
        }
    }
}

impl Watchdog for Board<'_> {
    fn feed(&mut self) {
        self.rwdt.feed();
    }
}

/// Light-sleep slice: the RTC watchdog keeps running in light sleep, so the bridge wakes to
/// feed it. ours: half the watchdog time.
const SLEEP_SLICE_MS: u64 = WATCHDOG_MS / 2;

/// Marks the RTC state below as written by [`deep_sleep`] (persistent RAM starts as random
/// bytes after some resets).
const DEEP_MAGIC: u32 = 0xC110_DEE9;

/// Deep-sleep state kept in RTC fast memory across the sleeps (each wake resets the chip).
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut DEEP_MARK: u32 = 0;
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut DEEP_HIGH: u8 = 0;
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut DEEP_WAKE: u32 = 0;

/// Deep sleep with a timer wake after `wake_ms` (kline-bridge `power`): no BLE, one ADC reading
/// per wake in [`resume_from_deep_sleep`]. Called with the K line already released (the bridge
/// sleeps only with no session). The fob and K_PWR pads float during deep sleep; the 10 kOhm
/// pull-downs keep both off (docs/bridge-hardware.md). Never returns: the wake resets the chip.
fn deep_sleep(low_power: &mut LowPower<'_>, rwdt: &mut Rwdt, high: u8, wake_ms: u64) -> ! {
    // SAFETY: single-threaded; nothing else touches these statics.
    unsafe {
        DEEP_HIGH = high;
        DEEP_WAKE = u32::try_from(wake_ms).unwrap_or(u32::MAX);
        DEEP_MARK = DEEP_MAGIC;
    }
    loop {
        low_power.set_wakeup_deadline(Instant::now() + Duration::from_millis(wake_ms));
        // The RTC watchdog would reset the chip in the middle of the sleep, so it goes off as
        // the very last step; a panic before this still resets (the panic handler resets too).
        rwdt.disable();
        low_power.sleep_deep_with_rejection(RtcSleepConfig::deep());
        // Rejected (a wake source was already asserted): the watchdog back on, and try again.
        // Some pads may be disconnected now; the pull-downs hold the fob and K_PWR off, and
        // the bridge never runs again before the reset of the wake.
        rwdt.enable();
        rwdt.feed();
    }
}

/// At boot after a deep-sleep wake: one battery reading, then back to deep sleep unless
/// [`DeepWake`] says resume (two readings in a row at 13.2 V or more). Returns on a cold boot,
/// on any other reset, and on resume.
fn resume_from_deep_sleep(low_power: &mut LowPower<'_>, rwdt: &mut Rwdt) {
    // SAFETY: single-threaded; read before anything writes them.
    let (mark, high, wake_ms) = unsafe { (DEEP_MARK, DEEP_HIGH, DEEP_WAKE) };
    if reset_reason(Cpu::ProCpu) != Some(SocResetReason::CoreDeepSleep) || mark != DEEP_MAGIC {
        // SAFETY: as above.
        unsafe { DEEP_MARK = 0 };
        return;
    }
    let mut wake = DeepWake { high };
    if wake.on_wake(battery_millivolts()) {
        // SAFETY: as above.
        unsafe { DEEP_MARK = 0 };
        return;
    }
    let wake_ms = if wake_ms == 0 {
        DEEP_WAKE_MS
    } else {
        u64::from(wake_ms)
    };
    deep_sleep(low_power, rwdt, wake.high, wake_ms)
}

impl Power for Board<'_> {
    /// Light sleep in slices of [`SLEEP_SLICE_MS`] (never a busy-wait), feeding the watchdog
    /// between slices; deep sleep (does not return, the wake resets the chip) when the reason
    /// asks for it. W7: wake on a BLE connection too.
    fn sleep(&mut self, wake_after_ms: Option<u64>, reason: SleepReason) {
        esp_println::println!("sleep {reason:?} {wake_after_ms:?}");
        if reason.mode() == SleepMode::DeepTimer {
            let wake_ms = wake_after_ms.unwrap_or(DEEP_WAKE_MS);
            deep_sleep(&mut self.low_power, &mut self.rwdt, 0, wake_ms);
        }
        let end = self.now_ms() + wake_after_ms.unwrap_or(1_000);
        while self.now_ms() < end {
            let slice = (end - self.now_ms()).min(SLEEP_SLICE_MS);
            self.low_power
                .set_wakeup_deadline(Instant::now() + Duration::from_millis(slice));
            self.low_power.sleep_light(RtcSleepConfig::default());
            self.rwdt.feed();
        }
    }
}

/// The fob PhotoMOS LED driver pin.
struct FobPin<'d>(Output<'d>);

impl OutputPin for FobPin<'_> {
    fn set_high(&mut self) {
        self.0.set_high();
    }
    fn set_low(&mut self) {
        self.0.set_low();
    }
}

/// Battery millivolts. ADC1 at 11 dB attenuation reads 0..~2500 mV over 12 bits; the raw to
/// mV scale is nominal (NOT VERIFIED: calibrate against the multimeter and Sirius 21A9,
/// PLAN-RESEARCH B5).
///
/// ADC1 is shared with the TRNG entropy source (`__getrandom_v03_custom`), so each reading
/// takes the ADC and gives it back; the main loop is the only thread.
fn battery_millivolts() -> Option<u16> {
    // SAFETY: single-threaded loop; the only other user of ADC1 (the TRNG source in
    // `__getrandom_v03_custom`) holds it only inside its own call, never across this one.
    let (adc1, gpio3) = unsafe { (ADC1::steal(), GPIO3::steal()) };
    let mut cfg = AdcConfig::new();
    let mut pin: AdcPin<GPIO3<'_>, ADC1<'_>> = cfg.enable_pin(gpio3, Attenuation::_11dB);
    let mut adc: Adc<'_, ADC1<'_>, Blocking> = Adc::new(adc1, cfg);
    let raw = nb_block(|| adc.read_oneshot(&mut pin))?;
    let adc_mv = u32::from(raw) * 2_500 / 4_095;
    u16::try_from(adc_mv * DIVIDER_NUM / DIVIDER_DEN).ok()
}

fn nb_block<T>(mut f: impl FnMut() -> nb::Result<T, ()>) -> Option<T> {
    for _ in 0..10_000 {
        match f() {
            Ok(v) => return Some(v),
            Err(nb::Error::WouldBlock) => continue,
            Err(nb::Error::Other(())) => return None,
        }
    }
    None
}

#[main]
fn main() -> ! {
    let p = esp_hal::init(esp_hal::Config::default());

    // First: the fob output low (MASTER-PLAN-v2 3.1 rule 3), K line released.
    let fob = FobDriver::new(
        FobPin(Output::new(p.GPIO7, Level::Low, OutputConfig::default())),
        FobTiming::default(),
    );
    let k_pwr = Output::new(p.GPIO6, Level::Low, OutputConfig::default());
    let break_n = Output::new(
        p.GPIO10,
        Level::High,
        OutputConfig::default().with_drive_mode(DriveMode::OpenDrain),
    );

    esp_alloc::heap_allocator!(size: 48 * 1024);

    let mut rtc = Rtc::new(p.RTC_TIMER);
    rtc.rwdt
        .set_timeout(RwdtStage::Stage0, Duration::from_millis(WATCHDOG_MS));
    rtc.rwdt.enable();

    // UART1, GPIO4 and GPIO5 are taken over by `KLine::set_power` (see `new_uart`). The
    // transceiver starts off, so the TX pin starts low.
    let _ = (p.UART1, p.GPIO4, p.GPIO5);
    let tx_low = tx_pin_low();

    // ADC1/GPIO3 (battery) and RNG/ADC1 (TRNG) are taken per use, see `battery_millivolts`.
    let _ = (p.ADC1, p.GPIO3, p.RNG);

    // After a deep-sleep wake: back to sleep until the battery reads charging (needs ADC1, still
    // unused here).
    let mut low_power = LowPower::new(p.LPWR);
    resume_from_deep_sleep(&mut low_power, &mut rtc.rwdt);

    let mut board = Board {
        kline: KLine {
            uart: None,
            tx_low: Some(tx_low),
            break_n,
            k_pwr,
            rx_during_break: false,
            in_break: false,
            battery: BatteryWatch::default(),
        },
        rwdt: rtc.rwdt,
        low_power,
    };
    let mut fob = fob;
    let mut bridge = Bridge::new(BridgeConfig::default(), board.now_ms());

    if cfg!(feature = "bench") {
        // A built-in agent-role client asks for the drive profile (Tier 0 only).
        use kline_bridge::gate::Role;
        use kline_bridge::gatt::{Command, Op};
        use kline_bridge::profile::Profile;
        let now = board.now_ms();
        bridge.on_connect(0, Role::Agent, now);
        if let Ok(f) = (Command {
            counter: 1,
            op: Op::StartLive(Profile::Drive),
        })
        .encode()
        {
            bridge.on_command(0, &f, now);
        }
    }

    let mut last_battery = 0u64;
    loop {
        let now = board.now_ms();
        if now.saturating_sub(last_battery) >= BATTERY_PERIOD_MS {
            last_battery = now;
            if let Some(mv) = battery_millivolts() {
                bridge.on_battery_mv(mv, now);
            }
        }
        let stepped = runner::step(&mut bridge, &mut board, &mut fob);
        // The lowest reading taken during a blocking action, stamped now (later than it was
        // taken, so it can only void an engine-off sample, never keep one).
        if let Some(mv) = board.kline.battery.take_lowest() {
            bridge.on_battery_mv(mv, board.now_ms());
            if let Some(mv) = battery_millivolts() {
                bridge.on_battery_mv(mv, board.now_ms());
            }
        }
        if let Stepped::Idle { until_ms } = stepped {
            // W7: light sleep until `until_ms` or a BLE event.
            board.wait_until(until_ms.min(now + 50));
        }
    }
}
