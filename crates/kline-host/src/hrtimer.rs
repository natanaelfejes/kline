//! High-resolution sleep for the bit-banged init timing (docs/platforms.md "Timing").
//!
//! The 5-baud init is 200 ms bits with +-10 ms (ISO 9141-2: 5 baud +-5 %), and the fast-init low
//! pulse TiniL is 25 +- 1 ms (ISO 14230-2). A plain OS sleep wakes up late by up to one timer
//! tick: on Windows the default tick is 15.625 ms (64 Hz), which alone breaks TiniL; on Linux and
//! macOS a wake-up is typically 50-500 us late but can be more under load.
//!
//! So [`sleep_until_us`] sleeps the OS way until [`SPIN_TAIL_US`] before the deadline and
//! busy-waits the rest on the monotonic clock. On Windows [`TimerResolution`] additionally asks
//! for a 1 ms system tick (`timeBeginPeriod(1)`) for as long as a serial transport is open, so the
//! OS part never overshoots into the spin tail by a whole 15.6 ms tick even where the Rust
//! standard library's high-resolution waitable timer is unavailable (Windows before 10 1803).
//! `kline selftest` measures what this achieves on the host (`sleep_jitter`).

use std::time::{Duration, Instant};

/// Busy-wait the last this many microseconds of every sleep (ours). Windows: 2 ms, above the
/// 1 ms tick `timeBeginPeriod(1)` gives plus scheduling slack; elsewhere 1 ms, above the
/// typical hrtimer wake-up latency of Linux/macOS.
pub const SPIN_TAIL_US: u64 = if cfg!(windows) { 2_000 } else { 1_000 };

/// What [`sleep_until_us`] needs: a monotonic clock, a (possibly coarse, late-waking) OS sleep
/// and one busy-wait step. Mocked in tests.
pub trait Sleeper {
    fn now_us(&self) -> u64;
    /// OS sleep; may wake up late, never early (std::thread::sleep's contract).
    fn os_sleep_us(&mut self, us: u64);
    /// One step of the busy-wait.
    fn spin(&mut self);
}

/// Sleep until `target_us` on `s`'s clock: OS sleep until `tail_us` before it, then spin.
/// Returns immediately when the target is already past.
pub fn sleep_until_us(s: &mut impl Sleeper, target_us: u64, tail_us: u64) {
    loop {
        let now = s.now_us();
        if now >= target_us {
            return;
        }
        let rem = target_us - now;
        if rem <= tail_us {
            break;
        }
        s.os_sleep_us(rem - tail_us);
    }
    while s.now_us() < target_us {
        s.spin();
    }
}

/// The real [`Sleeper`]: `Instant` and `std::thread::sleep`.
pub struct OsSleeper {
    pub t0: Instant,
}

impl Sleeper for OsSleeper {
    fn now_us(&self) -> u64 {
        self.t0.elapsed().as_micros() as u64
    }
    fn os_sleep_us(&mut self, us: u64) {
        std::thread::sleep(Duration::from_micros(us));
    }
    fn spin(&mut self) {
        std::hint::spin_loop();
    }
}

/// While alive, the Windows system timer runs at 1 ms (`timeBeginPeriod(1)`, undone in `Drop`).
/// A no-op elsewhere. [`TimerResolution::active`] says whether the request was granted.
pub struct TimerResolution {
    active: bool,
}

impl TimerResolution {
    /// Request a 1 ms tick (Windows); nothing elsewhere.
    pub fn request() -> TimerResolution {
        TimerResolution {
            active: imp::begin(),
        }
    }

    /// Whether a 1 ms tick was requested and granted (always false off Windows).
    pub fn active(&self) -> bool {
        self.active
    }
}

impl Drop for TimerResolution {
    fn drop(&mut self) {
        if self.active {
            imp::end();
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    //! The only unsafe code of kline-host: two winmm calls without pointers.
    /// Period requested from winmm (ms).
    const PERIOD_MS: u32 = 1;
    /// winmm `TIMERR_NOERROR`.
    const TIMERR_NOERROR: u32 = 0;

    #[link(name = "winmm")]
    extern "system" {
        fn timeBeginPeriod(u_period: u32) -> u32;
        fn timeEndPeriod(u_period: u32) -> u32;
    }

    pub fn begin() -> bool {
        // SAFETY: plain integer argument, no memory is shared; documented thread-safe.
        unsafe { timeBeginPeriod(PERIOD_MS) == TIMERR_NOERROR }
    }

    pub fn end() {
        // SAFETY: as above; paired with a successful begin().
        unsafe {
            timeEndPeriod(PERIOD_MS);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn begin() -> bool {
        false
    }
    pub fn end() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Virtual clock whose OS sleep wakes `late_us` late, optionally rounded up to a `tick_us`
    /// grid (Windows' default 15.625 ms timer).
    struct Mock {
        now: u64,
        late_us: u64,
        tick_us: u64,
        os_calls: Vec<u64>,
        spins: u64,
    }

    impl Sleeper for Mock {
        fn now_us(&self) -> u64 {
            self.now
        }
        fn os_sleep_us(&mut self, us: u64) {
            self.os_calls.push(us);
            let mut t = self.now + us + self.late_us;
            if self.tick_us > 0 {
                t = t.div_ceil(self.tick_us) * self.tick_us;
            }
            self.now = t;
        }
        fn spin(&mut self) {
            self.spins += 1;
            self.now += 1;
        }
    }

    fn mock(late_us: u64, tick_us: u64) -> Mock {
        Mock {
            now: 0,
            late_us,
            tick_us,
            os_calls: Vec::new(),
            spins: 0,
        }
    }

    #[test]
    fn a_late_os_wakeup_inside_the_tail_is_absorbed_by_the_spin() {
        // TiniL: 25 ms with +-1 ms (ISO 14230-2); the OS wakes 800 us late.
        let mut m = mock(800, 0);
        sleep_until_us(&mut m, 25_000, 1_000);
        assert_eq!(m.now, 25_000, "exactly on time, not 25.8 ms");
        assert_eq!(
            m.os_calls,
            vec![24_000],
            "OS asked for target minus tail only"
        );
        assert!(m.spins > 0);
    }

    #[test]
    fn a_coarse_tick_is_reported_not_hidden() {
        // Without timeBeginPeriod a 15.625 ms tick overshoots any tail shorter than a tick: the
        // algorithm cannot fix that, which is why Windows asks for a 1 ms tick and selftest
        // measures the result.
        let mut m = mock(0, 15_625);
        sleep_until_us(&mut m, 25_000, 2_000);
        assert_eq!(m.now, 31_250);
        // With a 1 ms tick (and 300 us scheduling delay) the same 25 ms lands on time.
        let mut m = mock(300, 1_000);
        sleep_until_us(&mut m, 25_000, 2_000);
        assert_eq!(m.now, 25_000);
    }

    #[test]
    fn past_targets_return_at_once() {
        let mut m = mock(0, 0);
        m.now = 5_000;
        sleep_until_us(&mut m, 4_000, 1_000);
        assert_eq!((m.now, m.spins), (5_000, 0));
        assert!(m.os_calls.is_empty());
    }

    #[test]
    fn real_clock_25ms_sleep_is_never_early() {
        // Real time; only the "never early" half is asserted (CI hosts are loaded).
        let mut s = OsSleeper { t0: Instant::now() };
        let start = s.now_us();
        sleep_until_us(&mut s, start + 25_000, SPIN_TAIL_US);
        assert!(s.now_us() >= start + 25_000);
    }

    #[test]
    fn timer_resolution_is_requested_only_on_windows() {
        let r = TimerResolution::request();
        assert_eq!(r.active(), cfg!(windows));
    }
}
