//! `kline selftest`: check the cable and the Mac before going to the car (docs/cli.md).
//!
//! Opens the port at 10400 baud, reports what the driver says, and measures how precisely the
//! host can toggle the break line (the 5-baud init is bit-banged with break: ISO 9141-2 allows
//! +-5 % on 200 ms bits, i.e. 10 ms; the fast-init low pulse TiniL is 25 +- 1 ms).
//!
//! What goes on the line: only break on/off with a 10 ms half-period, a pattern that is neither a
//! 5-baud bit (200 ms) nor a fast-init wake-up (25 ms low + StartCommunication). Nothing is
//! written. Run it with the cable plugged into the Mac only, not into the car.
//!
//! Optional (`--echo-rtt`): write [`ECHO_BYTE`] [`ECHO_COUNT`] times and time the echo, which
//! measures the USB latency (FTDI latency timer) the ~KW2 handshake must fit in. It needs a
//! looped-back K line: the cable on the car's OBD socket (powered K line, no init is sent) or
//! TX wired to RX. NOT VERIFIED on the car.
//!
//! The driver's baud: on macOS `serialport` returns the value it was asked for, not one read
//! back from the FTDI driver, so `baud_reported` there proves nothing about the line speed.

use std::io;

use serde::Serialize;

use crate::transport::Transport;

/// Half-period of the break toggles (ours: not a 5-baud bit, not TiniL).
pub const TOGGLE_HALF_MS: u32 = 10;
/// Number of break-on pulses.
pub const TOGGLES: u32 = 20;
/// 5-baud bit tolerance: 5 % of 200 ms (ISO 9141-2).
pub const FIVE_BAUD_TOLERANCE_US: u64 = 10_000;
/// Fast-init TiniL tolerance: +-1 ms (ISO 14230-2).
pub const FAST_INIT_TOLERANCE_US: u64 = 1_000;

/// Timing of the break edges against their ideal grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BreakJitter {
    pub edges: u32,
    pub half_ms: u32,
    /// Largest distance of an edge (end of the set_break call) from its ideal time.
    pub max_dev_us: u64,
    pub mean_dev_us: u64,
    /// Longest single set_break call.
    pub max_call_us: u64,
    /// Whether RX saw a break after the last set_break(true) (needs K-line power; a cable
    /// plugged only into the Mac normally shows nothing).
    pub loopback_seen: bool,
    pub five_baud_ok: bool,
    pub fast_init_ok: bool,
}

/// Toggle break `toggles` times, `half_ms` on and `half_ms` off, and time every edge.
pub fn measure_break_jitter(
    t: &mut dyn Transport,
    toggles: u32,
    half_ms: u32,
) -> io::Result<BreakJitter> {
    let t0 = t.now_us();
    let mut devs = Vec::new();
    let mut max_call = 0u64;
    let mut loopback_seen = false;
    for k in 0..toggles * 2 {
        let on = k % 2 == 0;
        let ideal = t0 + u64::from(k) * u64::from(half_ms) * 1000;
        let before = t.now_us();
        t.set_break(on)?;
        let after = t.now_us();
        max_call = max_call.max(after - before);
        devs.push(after.abs_diff(ideal));
        if on {
            t.sleep_ms(half_ms / 2);
            loopback_seen |= t.break_loopback_seen()?;
            let ideal_next = ideal + u64::from(half_ms) * 1000;
            let now = t.now_us();
            if ideal_next > now {
                t.sleep_ms(((ideal_next - now) / 1000) as u32);
            }
        } else {
            let ideal_next = ideal + u64::from(half_ms) * 1000;
            let now = t.now_us();
            if ideal_next > now {
                t.sleep_ms(((ideal_next - now) / 1000) as u32);
            }
        }
    }
    t.set_break(false)?;
    let max_dev = devs.iter().copied().max().unwrap_or(0);
    let mean_dev = if devs.is_empty() {
        0
    } else {
        devs.iter().sum::<u64>() / devs.len() as u64
    };
    Ok(BreakJitter {
        edges: toggles * 2,
        half_ms,
        max_dev_us: max_dev,
        mean_dev_us: mean_dev,
        max_call_us: max_call,
        loopback_seen,
        five_baud_ok: max_dev < FIVE_BAUD_TOLERANCE_US,
        fast_init_ok: max_dev < FAST_INIT_TOLERANCE_US,
    })
}

/// Byte written by the optional echo measurement. Not a frame and not a wake-up (an ECU wakes
/// only on a 200 ms-per-bit address or a 25 ms low pulse); `FF` is low for its start bit only
/// (~0.1 ms at 10400 baud). ours.
pub const ECHO_BYTE: u8 = 0xFF;
/// Echo wait per byte (ours: far above any USB latency timer).
pub const ECHO_TIMEOUT_MS: u32 = 100;
/// Bytes sent by the echo measurement.
pub const ECHO_COUNT: u32 = 10;
/// W4 minimum (ISO 14230-2: 25 ms): DDT2000 sends ~KW2 25 ms after KW2 arrives, so KW2 must
/// reach us well within the ECU's W4 window of 25..50 ms; an RX latency at or above this is
/// too late.
pub const W4_MIN_US: u64 = 25_000;

/// TX -> RX round trip of single bytes through the cable's K-line loopback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EchoRtt {
    pub sent: u32,
    pub echoed: u32,
    /// From before the write (which waits for transmission, tcdrain) to the echo in our buffer;
    /// includes ~1 ms of byte time and the driver's TX and RX latency (FTDI latency timer).
    pub min_us: Option<u64>,
    pub max_us: Option<u64>,
    pub mean_us: Option<u64>,
    /// Every byte came back and the slowest in less than [`W4_MIN_US`].
    pub kw2_handshake_ok: bool,
}

/// Write [`ECHO_BYTE`] `count` times and time each echo. Needs a looped-back K line (a KKL
/// cable on a powered K line, or TX wired to RX); otherwise nothing comes back.
pub fn measure_echo_rtt(t: &mut dyn Transport, count: u32) -> io::Result<EchoRtt> {
    let mut rtts = Vec::new();
    for _ in 0..count {
        t.flush_rx()?;
        let t0 = t.now_us();
        t.write(&[ECHO_BYTE], 0)?;
        let got = t.read(1, ECHO_TIMEOUT_MS)?;
        if got.first() == Some(&ECHO_BYTE) {
            rtts.push(t.now_us().saturating_sub(t0));
        }
    }
    let max = rtts.iter().copied().max();
    Ok(EchoRtt {
        sent: count,
        echoed: rtts.len() as u32,
        min_us: rtts.iter().copied().min(),
        max_us: max,
        mean_us: (!rtts.is_empty()).then(|| rtts.iter().sum::<u64>() / rtts.len() as u64),
        kw2_handshake_ok: rtts.len() as u32 == count && max.is_some_and(|m| m < W4_MIN_US),
    })
}

/// Host sleep precision, measured without any port: each target is slept [`SLEEP_REPS`] times
/// with the transport's clock ([`crate::transport::SystemClock`], OS sleep plus spin tail). The
/// targets and tolerances are the init timings that are bit-banged: 1 ms (fast-init DTR settle,
/// judged with the TiniL tolerance), 25 ms (TiniL, +-1 ms, ISO 14230-2) and 200 ms (5-baud bit,
/// +-10 ms, ISO 9141-2).
pub const SLEEP_TARGETS: [(u32, u64); 3] = [
    (1, FAST_INIT_TOLERANCE_US),
    (25, FAST_INIT_TOLERANCE_US),
    (200, FIVE_BAUD_TOLERANCE_US),
];
/// Repetitions per sleep target (ours: ~2.3 s in total).
pub const SLEEP_REPS: u32 = 10;

/// How late (or early) sleeps of one target woke up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SleepJitter {
    pub target_ms: u32,
    pub samples: u32,
    /// Wake-ups before the target (a correct sleep never has any).
    pub early: u32,
    pub min_late_us: u64,
    pub mean_late_us: u64,
    pub max_late_us: u64,
    pub tolerance_us: u64,
    /// No early wake-up and every one less than `tolerance_us` late.
    pub ok: bool,
}

/// Sleep `target_ms` `reps` times on `c` and measure each wake-up against its deadline.
pub fn measure_sleep_jitter(
    c: &mut dyn crate::transport::Clock,
    target_ms: u32,
    reps: u32,
    tolerance_us: u64,
) -> SleepJitter {
    let mut late = Vec::with_capacity(reps as usize);
    let mut early = 0;
    for _ in 0..reps {
        let deadline = c.now_us() + u64::from(target_ms) * 1000;
        c.sleep_until_us(deadline);
        let woke = c.now_us();
        if woke < deadline {
            early += 1;
        }
        late.push(woke.saturating_sub(deadline));
    }
    let max = late.iter().copied().max().unwrap_or(0);
    SleepJitter {
        target_ms,
        samples: reps,
        early,
        min_late_us: late.iter().copied().min().unwrap_or(0),
        mean_late_us: if late.is_empty() {
            0
        } else {
            late.iter().sum::<u64>() / late.len() as u64
        },
        max_late_us: max,
        tolerance_us,
        ok: early == 0 && max < tolerance_us,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Echoes each written byte after `latency_us`.
    struct Loop {
        now: u64,
        latency_us: u64,
        rx: Vec<(u64, u8)>,
        writes: usize,
    }

    impl Transport for Loop {
        fn now_ms(&self) -> u64 {
            self.now / 1000
        }
        fn now_us(&self) -> u64 {
            self.now
        }
        fn set_dtr(&mut self, _: bool) -> io::Result<()> {
            Ok(())
        }
        fn set_break(&mut self, _: bool) -> io::Result<()> {
            panic!("the echo test must not touch break")
        }
        fn break_loopback_seen(&mut self) -> io::Result<bool> {
            Ok(false)
        }
        fn sleep_ms(&mut self, ms: u32) {
            self.now += u64::from(ms) * 1000;
        }
        fn flush_rx(&mut self) -> io::Result<()> {
            let now = self.now;
            self.rx.retain(|(t, _)| *t > now);
            Ok(())
        }
        fn write(&mut self, b: &[u8], _: u32) -> io::Result<()> {
            self.writes += 1;
            self.now += 1000; // one byte at 10400 baud, tcdrain
            for &x in b {
                self.rx.push((self.now + self.latency_us, x));
            }
            Ok(())
        }
        fn read(&mut self, n: usize, timeout_ms: u32) -> io::Result<Vec<u8>> {
            let deadline = self.now + u64::from(timeout_ms) * 1000;
            match self.rx.first().copied() {
                Some((t, b)) if t <= deadline && n > 0 => {
                    self.now = self.now.max(t);
                    self.rx.remove(0);
                    Ok(vec![b])
                }
                _ => {
                    self.now = deadline;
                    Ok(Vec::new())
                }
            }
        }
    }

    #[test]
    fn echo_rtt_measures_latency_and_flags_the_kw2_window() {
        let mut fast = Loop {
            now: 0,
            latency_us: 1_000,
            rx: Vec::new(),
            writes: 0,
        };
        let e = measure_echo_rtt(&mut fast, 5).unwrap();
        assert_eq!((e.sent, e.echoed, fast.writes), (5, 5, 5));
        assert_eq!((e.min_us, e.max_us), (Some(2_000), Some(2_000)));
        assert!(e.kw2_handshake_ok, "{e:?}");

        // macOS AppleUSBFTDI default latency timer, ~16 ms each way.
        let mut slow = Loop {
            now: 0,
            latency_us: 30_000,
            rx: Vec::new(),
            writes: 0,
        };
        let e = measure_echo_rtt(&mut slow, 3).unwrap();
        assert_eq!(e.max_us, Some(31_000));
        assert!(!e.kw2_handshake_ok, "{e:?}");

        // No loopback: nothing back, not OK.
        let mut dead = Loop {
            now: 0,
            latency_us: 1_000_000,
            rx: Vec::new(),
            writes: 0,
        };
        let e = measure_echo_rtt(&mut dead, 2).unwrap();
        assert_eq!((e.echoed, e.max_us, e.kw2_handshake_ok), (0, None, false));
    }

    /// A line whose set_break costs `cost_us` and whose clock is virtual.
    struct Slow {
        now: u64,
        cost_us: u64,
        breaks: Vec<bool>,
    }

    impl Transport for Slow {
        fn now_ms(&self) -> u64 {
            self.now / 1000
        }
        fn now_us(&self) -> u64 {
            self.now
        }
        fn set_dtr(&mut self, _: bool) -> io::Result<()> {
            Ok(())
        }
        fn set_break(&mut self, on: bool) -> io::Result<()> {
            self.now += self.cost_us;
            self.breaks.push(on);
            Ok(())
        }
        fn break_loopback_seen(&mut self) -> io::Result<bool> {
            Ok(false)
        }
        fn sleep_ms(&mut self, ms: u32) {
            self.now += u64::from(ms) * 1000;
        }
        fn flush_rx(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn write(&mut self, _: &[u8], _: u32) -> io::Result<()> {
            panic!("selftest must not write")
        }
        fn read(&mut self, _: usize, _: u32) -> io::Result<Vec<u8>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn jitter_reflects_the_cost_of_each_break_call() {
        let mut fast = Slow {
            now: 0,
            cost_us: 200,
            breaks: Vec::new(),
        };
        let j = measure_break_jitter(&mut fast, 5, 10).unwrap();
        assert_eq!(j.edges, 10);
        assert_eq!(j.max_call_us, 200);
        assert!(j.fast_init_ok && j.five_baud_ok, "{j:?}");
        // Alternating, ends released, nothing written.
        assert_eq!(&fast.breaks[..4], [true, false, true, false]);
        assert_eq!(fast.breaks.last(), Some(&false));

        let mut slow = Slow {
            now: 0,
            cost_us: 3_000,
            breaks: Vec::new(),
        };
        let j = measure_break_jitter(&mut slow, 5, 10).unwrap();
        assert!(j.max_dev_us >= 3_000, "{j:?}");
        assert!(!j.fast_init_ok && j.five_baud_ok, "{j:?}");
    }

    /// Virtual clock whose sleeps wake up late by the next value of `late` (cycled), or
    /// `early_us` early.
    struct LateClock {
        now: u64,
        late: Vec<u64>,
        early_us: u64,
        i: usize,
    }

    impl crate::transport::Clock for LateClock {
        fn now_us(&self) -> u64 {
            self.now
        }
        fn sleep_until_us(&mut self, t: u64) {
            let l = self.late[self.i % self.late.len()];
            self.i += 1;
            self.now = (t + l).saturating_sub(self.early_us);
        }
    }

    #[test]
    fn sleep_jitter_stats_and_verdicts() {
        let mut c = LateClock {
            now: 0,
            late: vec![0, 300, 1_500],
            early_us: 0,
            i: 0,
        };
        let j = measure_sleep_jitter(&mut c, 25, 6, FAST_INIT_TOLERANCE_US);
        assert_eq!(
            (
                j.samples,
                j.early,
                j.min_late_us,
                j.mean_late_us,
                j.max_late_us
            ),
            (6, 0, 0, 600, 1_500)
        );
        assert!(!j.ok, "1.5 ms late breaks TiniL +-1 ms: {j:?}");

        let mut c = LateClock {
            now: 0,
            late: vec![100, 200],
            early_us: 0,
            i: 0,
        };
        let j = measure_sleep_jitter(&mut c, 200, 4, FIVE_BAUD_TOLERANCE_US);
        assert!(
            j.ok && j.max_late_us == 200 && j.mean_late_us == 150,
            "{j:?}"
        );

        // Waking early is always wrong, however small.
        let mut c = LateClock {
            now: 0,
            late: vec![0],
            early_us: 50,
            i: 0,
        };
        let j = measure_sleep_jitter(&mut c, 1, 3, FAST_INIT_TOLERANCE_US);
        assert_eq!((j.early, j.ok), (3, false));
    }
}
