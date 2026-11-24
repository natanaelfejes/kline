//! The simulated K line between a dumb KKL cable and one ECU.
//!
//! What the bus models (docs/sim.md):
//! - **Echo**: every byte the tester transmits appears in the tester's RX at the same time
//!   (the KKL cable has TX and RX on the one K wire).
//! - **Break**: the tester holding TX in break pulls K low. The RX side sees it (break
//!   loopback), and on release optionally one 0x00 "break residue" byte (FTDI reports a break
//!   as 0x00 with a framing error; NOT VERIFIED for the real cable).
//! - **Wake-up detection** from the break edges: a 5-baud address byte (start bit, 8 data bits
//!   LSB first, stop bit, 200 ms each) or a fast-init low pulse.
//! - **Scheduling**: the ECU queues reply bytes at future times; the tester's transport takes
//!   the bytes whose time has come.
//!
//! Time is milliseconds on whatever clock the transport uses (virtual in-process, wall clock on
//! the pty). Collisions between ECU and tester transmissions are not modelled.

use std::collections::VecDeque;

use crate::journal::{Journal, ViolationKind};
use crate::timing::{Tolerance, BYTE_MS, FIVE_BAUD_BIT_MS};

/// A wake-up the bus decoded from the break edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// A 5-baud address byte. `t_start`: falling edge of the start bit; `t_end`: end of the
    /// stop bit (`t_start + 2000`), from where W1 counts.
    FiveBaud { byte: u8, t_start: u64, t_end: u64 },
    /// A fast-init low pulse ending (line released) at `t_high`.
    Fast { t_low: u64, t_high: u64 },
}

/// Bytes the ECU wants to send, with their times.
#[derive(Debug, Default)]
pub struct Out {
    bytes: Vec<(u64, u8)>,
}

impl Out {
    /// Queue `frame` starting at `start`, one byte every [`BYTE_MS`] + `gap_ms`. Returns the
    /// time the last byte is complete.
    pub fn send(&mut self, start: u64, frame: &[u8], gap_ms: u64) -> u64 {
        let mut t = start;
        for (i, &b) in frame.iter().enumerate() {
            if i > 0 {
                t += gap_ms;
            }
            t += BYTE_MS;
            self.bytes.push((t, b));
        }
        t
    }

    /// The queued bytes with their times.
    pub fn bytes(&self) -> &[(u64, u8)] {
        &self.bytes
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Move every byte of `other` into this queue.
    pub fn append(&mut self, other: Out) {
        self.bytes.extend(other.bytes);
    }
}

/// An ECU (or a replay) attached to the bus.
pub trait Responder: Send {
    /// The bus decoded a wake-up pattern.
    fn on_wake(&mut self, wake: Wake, out: &mut Out);
    /// A byte from the tester, complete on the line at `t_ms` (not called while K is held low
    /// or a wake-up is being decoded).
    fn on_byte(&mut self, t_ms: u64, byte: u8, out: &mut Out);
    /// Earliest time the responder wants [`Responder::on_timer`] called.
    fn next_timer(&self) -> Option<u64>;
    fn on_timer(&mut self, t_ms: u64, out: &mut Out);
}

/// Cable and line options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusConfig {
    /// The cable echoes TX to RX and loops a break back (false: as if K were disconnected
    /// from the cable's RX, the tester sees nothing at all).
    pub echo: bool,
    /// One 0x00 in RX when a break is released.
    pub break_residue: bool,
}

impl Default for BusConfig {
    fn default() -> Self {
        BusConfig {
            echo: true,
            break_residue: true,
        }
    }
}

#[derive(Debug)]
struct WakeFrame {
    t0: u64,
    /// Rising/falling edges after t0: (time, low).
    edges: Vec<(u64, bool)>,
    /// Already violated a precondition (idle time); decoded but not delivered.
    rejected: bool,
}

impl WakeFrame {
    fn level_low_at(&self, t: u64) -> bool {
        let mut low = true;
        for &(et, l) in &self.edges {
            if et <= t {
                low = l;
            }
        }
        low
    }
}

pub struct Bus {
    ecu: Box<dyn Responder>,
    cfg: BusConfig,
    tol: Tolerance,
    journal: Journal,
    low: bool,
    rx: VecDeque<(u64, u8)>,
    loopback: bool,
    frame: Option<WakeFrame>,
    /// Last time anything happened on the line (for the idle-before-wake check).
    last_activity: u64,
}

impl Bus {
    /// `tol` drives the wake-up checks; the ECU checks the rest itself. `journal` receives
    /// wake-up violations (usually the ECU's journal).
    pub fn new(
        ecu: impl Responder + 'static,
        cfg: BusConfig,
        tol: Tolerance,
        journal: Journal,
    ) -> Bus {
        Bus {
            ecu: Box::new(ecu),
            cfg,
            tol,
            journal,
            low: false,
            rx: VecDeque::new(),
            loopback: false,
            frame: None,
            last_activity: 0,
        }
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    fn push_rx(&mut self, t: u64, b: u8) {
        // Keep RX ordered by time; equal times keep insertion order.
        let pos = self
            .rx
            .iter()
            .rposition(|&(rt, _)| rt <= t)
            .map_or(0, |p| p + 1);
        self.rx.insert(pos, (t, b));
    }

    fn take_out(&mut self, out: Out) {
        for (t, b) in out.bytes {
            self.last_activity = self.last_activity.max(t);
            self.push_rx(t, b);
        }
    }

    /// The tester sets (`true`) or clears its TX break at `t`.
    pub fn tester_break(&mut self, on: bool, t: u64) {
        self.advance(t);
        if on {
            self.loopback = self.cfg.echo;
        }
        if on == self.low {
            return;
        }
        self.low = on;
        if on {
            match &mut self.frame {
                Some(f) => f.edges.push((t, true)),
                None => {
                    let idle = t.saturating_sub(self.last_activity);
                    let rejected = idle < self.tol.idle_before_wake_ms;
                    if rejected {
                        self.journal.violation(
                            t,
                            ViolationKind::IdleBeforeWake,
                            format!(
                                "line idle {idle} ms before the wake-up, need {}",
                                self.tol.idle_before_wake_ms
                            ),
                        );
                    }
                    self.frame = Some(WakeFrame {
                        t0: t,
                        edges: Vec::new(),
                        rejected,
                    });
                }
            }
        } else {
            if self.cfg.echo && self.cfg.break_residue {
                self.push_rx(t, 0x00);
            }
            if let Some(f) = &mut self.frame {
                let low_ms = t - f.t0;
                if f.edges.is_empty() && low_ms < FIVE_BAUD_BIT_MS / 2 {
                    // A short pulse: fast init, or garbage.
                    let (t0, rejected) = (f.t0, f.rejected);
                    self.frame = None;
                    let (lo, hi) = self.tol.fast_init_low_ms;
                    if rejected {
                        // Idle violation already recorded.
                    } else if (lo..=hi).contains(&low_ms) {
                        let mut out = Out::default();
                        self.ecu.on_wake(
                            Wake::Fast {
                                t_low: t0,
                                t_high: t,
                            },
                            &mut out,
                        );
                        self.take_out(out);
                    } else {
                        self.journal.violation(
                            t,
                            ViolationKind::FastInitPulse,
                            format!("low pulse of {low_ms} ms, accepted {lo}..={hi}"),
                        );
                    }
                } else {
                    f.edges.push((t, false));
                }
            }
        }
        self.last_activity = t;
    }

    pub fn tester_dtr(&mut self, on: bool, t: u64) {
        self.advance(t);
        self.journal
            .note(t, format!("DTR {}", if on { "high" } else { "low" }));
    }

    /// The tester changed RTS (logged only: the simulated cable does not use it).
    pub fn tester_rts(&mut self, on: bool, t: u64) {
        self.advance(t);
        self.journal
            .note(t, format!("RTS {}", if on { "high" } else { "low" }));
    }

    /// A byte from the tester, complete on the line at `t`. Echoed to RX.
    pub fn tester_byte(&mut self, b: u8, t: u64) {
        self.advance(t);
        if self.cfg.echo {
            self.push_rx(t, b);
        }
        self.last_activity = t;
        if self.low || self.frame.is_some() {
            self.journal.note(
                t,
                format!("byte {b:#04x} while K low / wake-up in progress"),
            );
            return;
        }
        let mut out = Out::default();
        self.ecu.on_byte(t, b, &mut out);
        self.take_out(out);
    }

    /// Whether the break of the last `tester_break(true)` was seen on RX.
    pub fn break_loopback_seen(&self) -> bool {
        self.loopback
    }

    fn wake_timer(&self) -> Option<u64> {
        // Decide in the middle of the stop bit.
        self.frame
            .as_ref()
            .map(|f| f.t0 + 9 * FIVE_BAUD_BIT_MS + FIVE_BAUD_BIT_MS / 2)
    }

    /// Earliest pending timer (wake-up decoder or ECU).
    pub fn next_timer(&self) -> Option<u64> {
        match (self.wake_timer(), self.ecu.next_timer()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Time of the next RX byte, if any is queued.
    pub fn next_rx_time(&self) -> Option<u64> {
        self.rx.front().map(|&(t, _)| t)
    }

    /// Run all timers due at or before `t`.
    pub fn advance(&mut self, t: u64) {
        while let Some(nt) = self.next_timer() {
            if nt > t {
                break;
            }
            if self.wake_timer() == Some(nt) {
                self.decode_wake();
            } else {
                let mut out = Out::default();
                self.ecu.on_timer(nt, &mut out);
                self.take_out(out);
            }
        }
    }

    fn decode_wake(&mut self) {
        let Some(f) = self.frame.take() else { return };
        let t_mid_stop = f.t0 + 9 * FIVE_BAUD_BIT_MS + FIVE_BAUD_BIT_MS / 2;
        let t_end = f.t0 + 10 * FIVE_BAUD_BIT_MS;
        let reject = |bus: &mut Bus, detail: String| {
            bus.journal
                .violation(t_mid_stop, ViolationKind::FiveBaudTiming, detail);
        };
        if f.rejected {
            return;
        }
        let sample = |k: u64| f.level_low_at(f.t0 + k * FIVE_BAUD_BIT_MS + FIVE_BAUD_BIT_MS / 2);
        if !sample(0) {
            return reject(self, "start bit not low in its middle".into());
        }
        if sample(9) {
            return reject(self, "stop bit not high (line still low)".into());
        }
        if let Some(tol) = self.tol.five_baud_edge_ms {
            for &(et, _) in &f.edges {
                let off = et - f.t0;
                let k = (off + FIVE_BAUD_BIT_MS / 2) / FIVE_BAUD_BIT_MS;
                let dev = off.abs_diff(k * FIVE_BAUD_BIT_MS);
                if dev > tol {
                    return reject(
                        self,
                        format!("edge at +{off} ms is {dev} ms off the 200 ms grid (max {tol})"),
                    );
                }
            }
        }
        let mut byte = 0u8;
        for i in 0..8 {
            if !sample(i + 1) {
                byte |= 1 << i;
            }
        }
        self.journal
            .note(t_mid_stop, format!("5-baud address {byte:#04x}"));
        let mut out = Out::default();
        self.ecu.on_wake(
            Wake::FiveBaud {
                byte,
                t_start: f.t0,
                t_end,
            },
            &mut out,
        );
        self.take_out(out);
    }

    /// Take the next RX byte whose time is at or before `t`.
    pub fn pop_rx(&mut self, t: u64) -> Option<u8> {
        match self.rx.front() {
            Some(&(rt, b)) if rt <= t => {
                self.rx.pop_front();
                Some(b)
            }
            _ => None,
        }
    }

    /// Drop every RX byte that has arrived by `t` (later bytes are still on their way).
    pub fn flush_rx(&mut self, t: u64) {
        self.advance(t);
        while self.pop_rx(t).is_some() {}
    }
}
