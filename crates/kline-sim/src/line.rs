//! The tester's end of a K line, and the in-process byte-level duplex with a virtual clock.

use std::io;

use kline_core::{Action, Event};

use crate::bus::{Bus, BusConfig, Responder};
use crate::journal::Journal;
use crate::timing::{Tolerance, BYTE_MS};

/// What a transport driver needs from a serial port to run kline-core's machines. The future
/// CLI implements it for the FTDI cable; [`VirtualLine`] implements it in-process.
pub trait Line {
    /// Monotonic milliseconds.
    fn now_ms(&self) -> u64;
    fn set_dtr(&mut self, on: bool) -> io::Result<()>;
    fn set_break(&mut self, on: bool) -> io::Result<()>;
    /// Whether RX saw the break since the last `set_break(true)`.
    fn break_loopback_seen(&mut self) -> io::Result<bool>;
    fn sleep_ms(&mut self, ms: u32);
    fn flush_rx(&mut self) -> io::Result<()>;
    /// `inter_byte_ms == 0`: one write; else byte by byte with this gap.
    fn write(&mut self, bytes: &[u8], inter_byte_ms: u32) -> io::Result<()>;
    /// Up to `count` bytes within `timeout_ms` in total; fewer means the timeout expired.
    fn read(&mut self, count: usize, timeout_ms: u32) -> io::Result<Vec<u8>>;

    /// Perform one kline-core [`Action`] and return the [`Event`] that answers it.
    fn perform(&mut self, action: &Action) -> io::Result<Event> {
        Ok(match action {
            Action::SetDtr(on) => {
                self.set_dtr(*on)?;
                Event::Done
            }
            Action::SetBreak(on) => {
                self.set_break(*on)?;
                Event::Done
            }
            Action::Sleep { ms } => {
                self.sleep_ms(*ms);
                Event::Done
            }
            Action::CheckBreakLoopback => Event::BreakLoopbackSeen(self.break_loopback_seen()?),
            Action::FlushRx => {
                self.flush_rx()?;
                Event::Done
            }
            Action::Write {
                frame,
                inter_byte_ms,
            } => {
                self.write(frame.bytes(), *inter_byte_ms)?;
                Event::Done
            }
            Action::Read { count, timeout_ms } => {
                let b = self.read(*count, *timeout_ms)?;
                if b.len() == *count {
                    Event::ReadOk(b)
                } else {
                    Event::ReadTimeout(b)
                }
            }
        })
    }
}

/// In-process duplex: the tester side of a [`Bus`] with a virtual millisecond clock.
/// Sleeping and waiting advance the clock instantly, so a 5-baud init takes no real time and
/// every run is deterministic.
pub struct VirtualLine {
    bus: Bus,
    now: u64,
}

impl VirtualLine {
    pub fn new(
        ecu: impl Responder + 'static,
        cfg: BusConfig,
        tol: Tolerance,
        journal: Journal,
    ) -> VirtualLine {
        VirtualLine {
            bus: Bus::new(ecu, cfg, tol, journal),
            now: 0,
        }
    }

    /// Line for a [`crate::ecu::SpecEcu`] with the default cable and the ECU's own tolerance.
    pub fn with_ecu(ecu: crate::ecu::SpecEcu) -> VirtualLine {
        let j = ecu.journal();
        let tol = ecu.profile().tolerance;
        VirtualLine::new(ecu, BusConfig::default(), tol, j)
    }

    pub fn bus(&self) -> &Bus {
        &self.bus
    }

    pub fn journal(&self) -> &Journal {
        self.bus.journal()
    }

    /// Move the clock forward (same as `sleep_ms`).
    pub fn advance_ms(&mut self, ms: u64) {
        self.now += ms;
        self.bus.advance(self.now);
    }
}

impl Line for VirtualLine {
    fn now_ms(&self) -> u64 {
        self.now
    }

    fn set_dtr(&mut self, on: bool) -> io::Result<()> {
        self.bus.tester_dtr(on, self.now);
        Ok(())
    }

    fn set_break(&mut self, on: bool) -> io::Result<()> {
        self.bus.tester_break(on, self.now);
        Ok(())
    }

    fn break_loopback_seen(&mut self) -> io::Result<bool> {
        Ok(self.bus.break_loopback_seen())
    }

    fn sleep_ms(&mut self, ms: u32) {
        self.advance_ms(u64::from(ms));
    }

    fn flush_rx(&mut self) -> io::Result<()> {
        self.bus.flush_rx(self.now);
        Ok(())
    }

    fn write(&mut self, bytes: &[u8], inter_byte_ms: u32) -> io::Result<()> {
        for (i, &b) in bytes.iter().enumerate() {
            if i > 0 {
                self.now += u64::from(inter_byte_ms);
            }
            self.now += BYTE_MS;
            self.bus.tester_byte(b, self.now);
        }
        Ok(())
    }

    fn read(&mut self, count: usize, timeout_ms: u32) -> io::Result<Vec<u8>> {
        let deadline = self.now + u64::from(timeout_ms);
        let mut out = Vec::with_capacity(count);
        loop {
            self.bus.advance(self.now);
            while out.len() < count {
                match self.bus.pop_rx(self.now) {
                    Some(b) => out.push(b),
                    None => break,
                }
            }
            if out.len() == count {
                return Ok(out);
            }
            let next = match (self.bus.next_rx_time(), self.bus.next_timer()) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            match next {
                Some(n) if n <= deadline => self.now = n.max(self.now + 1).min(deadline),
                _ => {
                    self.now = deadline;
                    self.bus.advance(self.now);
                    while out.len() < count {
                        match self.bus.pop_rx(self.now) {
                            Some(b) => out.push(b),
                            None => break,
                        }
                    }
                    return Ok(out);
                }
            }
        }
    }
}
