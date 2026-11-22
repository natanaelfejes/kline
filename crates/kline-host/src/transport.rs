//! Transports: what kline-core's `Action`s need from a K line, with real time.
//!
//! - [`Transport`]: the operations of the sans-IO contract (docs/protocol.md "Sans-IO
//!   contract"). [`crate::link::Link`] maps each `Action` onto one call.
//! - [`KklLine`]: a [`Transport`] over any [`SerialIo`] and [`Clock`]: the FTDI KKL cable
//!   ([`SerialportIo`], 10400 8N1, break via `set_break`/`clear_break`, DTR) or the simulator's
//!   pty plus control socket ([`SimIo`], docs/sim.md "pty transport").
//!
//! Hardware behaviour of the FTDI cable under macOS (break latency, how a looped-back break
//! shows up on RX, DTR effect) is NOT VERIFIED; see docs/cli.md.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use crate::trace::Tracer;

/// What a driver needs from the K line. Timeouts are totals per call (docs/protocol.md).
pub trait Transport {
    /// Monotonic milliseconds since the transport was opened.
    fn now_ms(&self) -> u64;
    /// Same clock in microseconds (trace resolution).
    fn now_us(&self) -> u64 {
        self.now_ms() * 1000
    }
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
    /// The line speed the driver reports, if it can (`kline selftest`).
    fn baud_rate(&self) -> Option<u32> {
        None
    }
}

/// Wall clock for a [`KklLine`]; mocked in unit tests.
pub trait Clock {
    fn now_us(&self) -> u64;
    fn sleep_until_us(&mut self, t_us: u64);
}

/// Real time from `Instant`, with the high-resolution sleep of [`crate::hrtimer`] (OS sleep plus
/// a busy-wait tail; on Windows a 1 ms system tick while the clock lives).
pub struct SystemClock {
    sleeper: crate::hrtimer::OsSleeper,
    resolution: crate::hrtimer::TimerResolution,
}

impl SystemClock {
    pub fn new() -> SystemClock {
        SystemClock {
            resolution: crate::hrtimer::TimerResolution::request(),
            sleeper: crate::hrtimer::OsSleeper { t0: Instant::now() },
        }
    }

    /// Whether the Windows 1 ms timer tick was granted (always false elsewhere).
    pub fn timer_resolution_active(&self) -> bool {
        self.resolution.active()
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        SystemClock::new()
    }
}

impl Clock for SystemClock {
    fn now_us(&self) -> u64 {
        use crate::hrtimer::Sleeper as _;
        self.sleeper.now_us()
    }
    fn sleep_until_us(&mut self, t_us: u64) {
        crate::hrtimer::sleep_until_us(&mut self.sleeper, t_us, crate::hrtimer::SPIN_TAIL_US);
    }
}

/// The serial-port operations a [`KklLine`] uses.
pub trait SerialIo {
    fn set_break(&mut self, on: bool) -> io::Result<()>;
    fn set_dtr(&mut self, on: bool) -> io::Result<()>;
    /// `Some(seen)` when the port reports break loopback itself (the simulator's control
    /// socket); `None`: infer it from bytes on RX.
    fn break_seen(&mut self) -> io::Result<Option<bool>> {
        Ok(None)
    }
    fn bytes_to_read(&mut self) -> io::Result<usize>;
    /// Read what is available within `timeout` (0 bytes when it expires).
    fn read_some(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
    /// Write all bytes and wait until they are transmitted (tcdrain).
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn clear_input(&mut self) -> io::Result<()>;
    /// The line speed the driver reports, if any.
    fn baud_rate(&self) -> Option<u32> {
        None
    }
}

/// Window within which consecutive break/DTR sleeps are paced from the previous sleep's
/// deadline instead of from "now" (ours). It keeps the 5-baud bit edges on the 200 ms grid
/// (ISO 9141-2: 5 baud +-5 %) even when each `set_break` ioctl costs a millisecond or two.
/// Beyond it (a stall), the next sleep starts from now again.
pub const PACE_WINDOW_MS: u64 = 50;
/// Upper bound on bytes drained in one flush (ours; a K line at 10400 baud cannot deliver more
/// than ~1000 bytes/s, so this only stops a runaway loop).
const MAX_DRAIN: usize = 4096;

/// A K line over a serial port: the FTDI KKL cable or the simulator's pty.
pub struct KklLine<P: SerialIo, C: Clock> {
    port: P,
    clock: C,
    trace: Tracer,
    break_on: bool,
    loopback_rx: Vec<u8>,
    /// Deadline (us) of the previous sleep, while pacing break/DTR edges.
    pace_us: Option<u64>,
}

impl<P: SerialIo, C: Clock> KklLine<P, C> {
    pub fn new(port: P, clock: C, trace: Tracer) -> KklLine<P, C> {
        KklLine {
            port,
            clock,
            trace,
            break_on: false,
            loopback_rx: Vec::new(),
            pace_us: None,
        }
    }

    pub fn port(&self) -> &P {
        &self.port
    }

    fn t(&self, msg: impl AsRef<str>) {
        self.trace.line(self.clock.now_us(), msg);
    }

    /// Read whatever is waiting, for the trace (and the loopback check).
    fn drain(&mut self) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        while out.len() < MAX_DRAIN && self.port.bytes_to_read()? > 0 {
            let n = self.port.read_some(&mut buf, Duration::ZERO)?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        Ok(out)
    }
}

impl<P: SerialIo, C: Clock> Transport for KklLine<P, C> {
    fn baud_rate(&self) -> Option<u32> {
        self.port.baud_rate()
    }

    fn now_ms(&self) -> u64 {
        self.clock.now_us() / 1000
    }

    fn now_us(&self) -> u64 {
        self.clock.now_us()
    }

    fn set_dtr(&mut self, on: bool) -> io::Result<()> {
        // Every init sequence starts with a DTR edge (5-baud: DTR low before the start bit; fast
        // init: DTR high before the 1 ms + TiniL pattern), and only the idle sleep comes before
        // it. Pacing starts here, so an oversleep of that idle wait is not taken out of the
        // start bit or TiniL (Hydrogen final review). DTR goes high again only after the stop
        // bit, outside the paced 5-baud bits.
        self.pace_us = None;
        self.port.set_dtr(on)
    }

    fn set_break(&mut self, on: bool) -> io::Result<()> {
        if on && !self.break_on {
            // "Since the last set_break(true)": forget what came before the break.
            let old = self.drain()?;
            if !old.is_empty() {
                self.t(format!(
                    "  rx before break discarded: {}",
                    crate::hex_sp(&old)
                ));
            }
            self.loopback_rx.clear();
        }
        self.port.set_break(on)?;
        self.break_on = on;
        Ok(())
    }

    fn break_loopback_seen(&mut self) -> io::Result<bool> {
        if let Some(seen) = self.port.break_seen()? {
            return Ok(seen);
        }
        // A KKL cable loops TX to RX; a break arrives as one 0x00 ([`SerialportIo::open`]
        // clears IGNBRK, BRKINT and PARMRK). Any byte counts. NOT VERIFIED on the FTDI under
        // macOS; without a byte the init warns and goes on unless --strict-loopback.
        let b = self.drain()?;
        self.loopback_rx.extend_from_slice(&b);
        if !self.loopback_rx.is_empty() {
            self.t(format!(
                "  rx during break: {}",
                crate::hex_sp(&self.loopback_rx)
            ));
        }
        Ok(!self.loopback_rx.is_empty())
    }

    fn sleep_ms(&mut self, ms: u32) {
        let now = self.clock.now_us();
        let target = match self.pace_us {
            Some(p) if now.saturating_sub(p) < PACE_WINDOW_MS * 1000 => p + u64::from(ms) * 1000,
            _ => now + u64::from(ms) * 1000,
        };
        self.clock.sleep_until_us(target);
        // Only edges are paced: a sleep that follows a break/DTR change keeps the grid.
        self.pace_us = Some(target);
    }

    fn flush_rx(&mut self) -> io::Result<()> {
        self.pace_us = None;
        let b = self.drain()?;
        if !b.is_empty() {
            self.t(format!("  flushed: {}", crate::hex_sp(&b)));
        }
        self.port.clear_input()
    }

    fn write(&mut self, bytes: &[u8], inter_byte_ms: u32) -> io::Result<()> {
        self.pace_us = None;
        if inter_byte_ms == 0 {
            return self.port.write_all(bytes);
        }
        for (i, b) in bytes.iter().enumerate() {
            if i > 0 {
                let t = self.clock.now_us() + u64::from(inter_byte_ms) * 1000;
                self.clock.sleep_until_us(t);
            }
            self.port.write_all(std::slice::from_ref(b))?;
        }
        Ok(())
    }

    fn read(&mut self, count: usize, timeout_ms: u32) -> io::Result<Vec<u8>> {
        self.pace_us = None;
        let deadline = self.clock.now_us() + u64::from(timeout_ms) * 1000;
        let mut out = vec![0u8; count];
        let mut n = 0;
        while n < count {
            let now = self.clock.now_us();
            if now >= deadline {
                break;
            }
            let k = self
                .port
                .read_some(&mut out[n..], Duration::from_micros(deadline - now))?;
            n += k;
        }
        out.truncate(n);
        Ok(out)
    }
}

/// [`SerialIo`] for a `serialport` port: the FTDI KKL cable.
pub struct SerialportIo {
    port: Box<dyn serialport::SerialPort>,
}

/// Input flags for the K line: `flags` with IGNBRK, BRKINT and PARMRK cleared, so a break on RX
/// is read as a single `00` byte (POSIX "General Terminal Interface", 11.2.2), which the 5-baud
/// loopback check counts. `serialport` opens with `cfmakeraw`, which on macOS/BSD *sets*
/// IGNBRK (the break is dropped; glibc's clears it). PARMRK stays off: with it a break reads
/// as `FF 00 00` and every data byte `FF` doubles, which would corrupt KWP frames.
#[cfg(unix)]
pub fn kline_input_flags(flags: nix::sys::termios::InputFlags) -> nix::sys::termios::InputFlags {
    use nix::sys::termios::InputFlags as F;
    flags & !(F::IGNBRK | F::BRKINT | F::PARMRK)
}

/// Baud to open a simulator pty with: 0 on macOS, where `serialport` sets any speed above 0
/// with the `IOSSIOSPEED` ioctl, which a pty refuses with ENOTTY (Hydrogen final review); the
/// pty has no line speed anyway. Elsewhere the real value, as for the cable.
pub fn pty_baud(baud: u32) -> u32 {
    if cfg!(any(target_os = "macos", target_os = "ios")) {
        0
    } else {
        baud
    }
}

impl SerialportIo {
    /// Open `path` at `baud` 8N1, no flow control, DTR high (idle, docs/protocol.md), with
    /// breaks delivered as a `00` byte ([`kline_input_flags`]). `baud` 0 keeps the speed
    /// (simulator pty on macOS, [`pty_baud`]).
    pub fn open(path: &str, baud: u32) -> io::Result<SerialportIo> {
        let builder = serialport::new(path, baud)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .flow_control(serialport::FlowControl::None)
            .timeout(Duration::from_millis(10));
        #[cfg(unix)]
        let mut port: Box<dyn serialport::SerialPort> = {
            use nix::sys::termios::{tcgetattr, tcsetattr, SetArg};
            use serialport::SerialPort as _;
            use std::os::unix::io::AsRawFd;
            let mut native = builder.open_native().map_err(io::Error::from)?;
            let fd = native.as_raw_fd();
            let mut tio = tcgetattr(fd).map_err(io::Error::from)?;
            tio.input_flags = kline_input_flags(tio.input_flags);
            tcsetattr(fd, SetArg::TCSANOW, &tio).map_err(io::Error::from)?;
            if baud > 0 {
                // tcsetattr may have rewritten the speed from libc's view of it (macOS sets
                // 10400 with IOSSIOSPEED after tcsetattr; Linux with termios2): set it again.
                native.set_baud_rate(baud).map_err(io::Error::from)?;
            }
            let check = tcgetattr(fd).map_err(io::Error::from)?;
            if check.input_flags != kline_input_flags(check.input_flags) {
                return Err(io::Error::other(
                    "the port kept IGNBRK/BRKINT/PARMRK set: a 5-baud break would be lost",
                ));
            }
            Box::new(native)
        };
        #[cfg(not(unix))]
        let mut port = builder.open().map_err(io::Error::from)?;
        // A pty refuses DTR (ENOTTY); a real cable must accept it.
        let _ = port.write_data_terminal_ready(true);
        Ok(SerialportIo { port })
    }
}

impl SerialIo for SerialportIo {
    fn set_break(&mut self, on: bool) -> io::Result<()> {
        if on {
            self.port.set_break().map_err(io::Error::from)
        } else {
            self.port.clear_break().map_err(io::Error::from)
        }
    }

    fn set_dtr(&mut self, on: bool) -> io::Result<()> {
        self.port
            .write_data_terminal_ready(on)
            .map_err(io::Error::from)
    }

    fn bytes_to_read(&mut self) -> io::Result<usize> {
        Ok(self.port.bytes_to_read().map_err(io::Error::from)? as usize)
    }

    fn read_some(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        if timeout.is_zero() && self.bytes_to_read()? == 0 {
            return Ok(0);
        }
        self.port
            .set_timeout(timeout.max(Duration::from_millis(1)))
            .map_err(io::Error::from)?;
        match self.port.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(0),
            Err(e) => Err(e),
        }
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.port.write_all(bytes)?;
        self.port.flush()
    }

    fn clear_input(&mut self) -> io::Result<()> {
        self.port
            .clear(serialport::ClearBuffer::Input)
            .map_err(io::Error::from)
    }

    /// On macOS `serialport` 4.10 sets any speed with Apple's `IOSSIOSPEED` ioctl (after
    /// `tcsetattr`) and returns the value it was *asked* for, cached, not read back from the
    /// driver (serialport `TTYPort::baud_rate`); elsewhere the termios speed. So on macOS this
    /// says nothing about the speed the FTDI chip runs at.
    fn baud_rate(&self) -> Option<u32> {
        self.port.baud_rate().ok()
    }
}

/// [`SerialIo`] for the simulator: data over its pty, break / DTR / loopback over its control
/// socket (docs/sim.md "pty transport").
#[cfg(unix)]
pub struct SimIo {
    port: SerialportIo,
    ctl: crate::control::ControlClient,
}

#[cfg(unix)]
impl SimIo {
    pub fn open(tty: &str, control: &std::path::Path, baud: u32) -> io::Result<SimIo> {
        Ok(SimIo {
            port: SerialportIo::open(tty, pty_baud(baud))?,
            ctl: crate::control::ControlClient::connect(control)?,
        })
    }
}

#[cfg(unix)]
impl SerialIo for SimIo {
    fn set_break(&mut self, on: bool) -> io::Result<()> {
        // A no-op on a pty; kept so the call sequence is the real one.
        let _ = self.port.set_break(on);
        self.ctl.set_break(on)
    }
    fn set_dtr(&mut self, on: bool) -> io::Result<()> {
        self.ctl.set_dtr(on)
    }
    fn break_seen(&mut self) -> io::Result<Option<bool>> {
        self.ctl.loopback_seen().map(Some)
    }
    fn bytes_to_read(&mut self) -> io::Result<usize> {
        self.port.bytes_to_read()
    }
    fn read_some(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        self.port.read_some(buf, timeout)
    }
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.port.write_all(bytes)
    }
    fn clear_input(&mut self) -> io::Result<()> {
        self.port.clear_input()
    }
}

/// Open the K line `spec` names at `baud`.
pub fn open(spec: &PortSpec, baud: u32, trace: Tracer) -> io::Result<Box<dyn Transport>> {
    match spec {
        PortSpec::Serial(p) => Ok(Box::new(KklLine::new(
            SerialportIo::open(p, baud)?,
            SystemClock::new(),
            trace,
        ))),
        #[cfg(unix)]
        PortSpec::Sim { tty, control } => Ok(Box::new(KklLine::new(
            SimIo::open(tty, control, baud)?,
            SystemClock::new(),
            trace,
        ))),
        #[cfg(not(unix))]
        PortSpec::Sim { .. } => Err(io::Error::other("the simulator transport needs a Unix pty")),
    }
}

/// Which K line to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortSpec {
    /// A serial device (the FTDI cable).
    Serial(String),
    /// The simulator: its pty and control socket.
    Sim {
        tty: String,
        control: std::path::PathBuf,
    },
}

#[cfg(test)]
mod tests {
    //! Hardware paths against a mock port: the call sequence and timing the FTDI cable would
    //! see. What the real cable does with it is NOT VERIFIED.
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Break(bool, u64),
        Dtr(bool, u64),
        Write(Vec<u8>, u64),
        Clear(u64),
    }

    #[derive(Default)]
    struct State {
        now_us: u64,
        calls: Vec<Call>,
        /// Bytes arriving at given times (us).
        rx: VecDeque<(u64, u8)>,
        /// Cost of each break/DTR ioctl (us): simulated USB latency.
        ioctl_us: u64,
        /// How late the next sleep wakes up (us), once: a host timer oversleeping.
        oversleep_once_us: u64,
    }

    #[derive(Clone, Default)]
    struct Mock(Rc<RefCell<State>>);

    struct MockClock(Mock);

    impl Clock for MockClock {
        fn now_us(&self) -> u64 {
            self.0 .0.borrow().now_us
        }
        fn sleep_until_us(&mut self, t: u64) {
            let mut s = self.0 .0.borrow_mut();
            let late = std::mem::take(&mut s.oversleep_once_us);
            s.now_us = s.now_us.max(t) + late;
        }
    }

    impl SerialIo for Mock {
        fn set_break(&mut self, on: bool) -> io::Result<()> {
            let mut s = self.0.borrow_mut();
            s.now_us += s.ioctl_us;
            let t = s.now_us;
            s.calls.push(Call::Break(on, t));
            Ok(())
        }
        fn set_dtr(&mut self, on: bool) -> io::Result<()> {
            let mut s = self.0.borrow_mut();
            s.now_us += s.ioctl_us;
            let t = s.now_us;
            s.calls.push(Call::Dtr(on, t));
            Ok(())
        }
        fn bytes_to_read(&mut self) -> io::Result<usize> {
            let s = self.0.borrow();
            Ok(s.rx.iter().filter(|(t, _)| *t <= s.now_us).count())
        }
        fn read_some(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
            let mut s = self.0.borrow_mut();
            let deadline = s.now_us + timeout.as_micros() as u64;
            match s.rx.front() {
                Some(&(t, _)) if t <= deadline => {
                    s.now_us = s.now_us.max(t);
                    let mut n = 0;
                    while n < buf.len() {
                        match s.rx.front() {
                            Some(&(t, b)) if t <= s.now_us => {
                                buf[n] = b;
                                n += 1;
                                s.rx.pop_front();
                            }
                            _ => break,
                        }
                    }
                    Ok(n)
                }
                _ => {
                    s.now_us = deadline;
                    Ok(0)
                }
            }
        }
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            let mut s = self.0.borrow_mut();
            let t = s.now_us;
            s.calls.push(Call::Write(bytes.to_vec(), t));
            // ~1 ms per byte at 10400 baud, and the cable echoes it.
            for &b in bytes {
                s.now_us += 1000;
                let t = s.now_us;
                s.rx.push_back((t, b));
            }
            Ok(())
        }
        fn clear_input(&mut self) -> io::Result<()> {
            let mut s = self.0.borrow_mut();
            let now = s.now_us;
            s.rx.retain(|(t, _)| *t > now);
            s.calls.push(Call::Clear(now));
            Ok(())
        }
    }

    fn line(ioctl_us: u64) -> (KklLine<Mock, MockClock>, Mock) {
        let m = Mock::default();
        m.0.borrow_mut().ioctl_us = ioctl_us;
        (
            KklLine::new(m.clone(), MockClock(m.clone()), Tracer::off()),
            m,
        )
    }

    #[test]
    fn five_baud_edges_stay_on_the_200ms_grid_despite_ioctl_latency() {
        let (mut l, m) = line(3_000);
        // Start bit + 0x7A LSB first: 0 1 0 1 1 1 1 0, then stop.
        l.set_break(true).unwrap();
        l.sleep_ms(200);
        for i in 0..8 {
            l.set_break((0x7A >> i) & 1 == 0).unwrap();
            l.sleep_ms(200);
        }
        l.set_break(false).unwrap();
        let edges: Vec<u64> =
            m.0.borrow()
                .calls
                .iter()
                .filter_map(|c| match c {
                    Call::Break(_, t) => Some(*t),
                    _ => None,
                })
                .collect();
        assert_eq!(edges.len(), 10);
        for (i, t) in edges.iter().enumerate() {
            let ideal = i as u64 * 200_000;
            // Each edge lags its ideal time by one ioctl only, never accumulating.
            assert!(
                *t >= ideal && *t <= ideal + 3_000 * 2,
                "edge {i} at {t} us, ideal {ideal}"
            );
        }
    }

    fn break_times(m: &Mock) -> Vec<(bool, u64)> {
        m.0.borrow()
            .calls
            .iter()
            .filter_map(|c| match c {
                Call::Break(on, t) => Some((*on, *t)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn idle_oversleep_does_not_shorten_tinil_or_the_start_bit() {
        // Hydrogen final review: a 20 ms late wake-up from the 300 ms idle must not be taken out
        // of the fast-init low pulse (25 +- 1 ms, ISO 14230-2) or the 5-baud start bit.
        let (mut l, m) = line(500);
        m.0.borrow_mut().oversleep_once_us = 20_000;
        // FastInit::start: idle, DTR high, 1 ms, break 25 ms, release 25 ms.
        l.sleep_ms(300);
        l.set_dtr(true).unwrap();
        l.sleep_ms(1);
        l.set_break(true).unwrap();
        l.sleep_ms(25);
        l.set_break(false).unwrap();
        l.sleep_ms(25);
        let b = break_times(&m);
        let tinil = b[1].1 - b[0].1;
        assert!((24_000..=26_000).contains(&tinil), "TiniL {tinil} us");

        let (mut l, m) = line(500);
        m.0.borrow_mut().oversleep_once_us = 20_000;
        // FiveBaudInit::start: idle, DTR low, start bit 200 ms, then the first data bit.
        l.sleep_ms(300);
        l.set_dtr(false).unwrap();
        l.set_break(true).unwrap();
        l.sleep_ms(200);
        l.set_break(false).unwrap();
        let b = break_times(&m);
        let start = b[1].1 - b[0].1;
        assert!((190_000..=210_000).contains(&start), "start bit {start} us");
    }

    #[cfg(unix)]
    #[test]
    fn input_flags_deliver_a_break_as_a_byte() {
        use nix::sys::termios::InputFlags as F;
        // macOS cfmakeraw: IGNBRK set. Cleared, with BRKINT and PARMRK; the rest kept.
        let f = kline_input_flags(F::IGNBRK | F::BRKINT | F::PARMRK | F::IGNPAR);
        assert_eq!(f, F::IGNPAR);
        assert_eq!(kline_input_flags(F::empty()), F::empty());
    }

    #[cfg(unix)]
    #[test]
    fn opened_pty_has_ignbrk_cleared() {
        use nix::sys::termios::{tcgetattr, tcsetattr, InputFlags as F, SetArg};
        let Ok(pty) = nix::pty::openpty(None, None) else {
            eprintln!("SKIPPED: no pty");
            return;
        };
        use std::os::unix::io::AsRawFd;
        let slave = pty.slave.as_raw_fd();
        // Start from what macOS cfmakeraw leaves behind.
        let mut t = tcgetattr(slave).unwrap();
        t.input_flags |= F::IGNBRK | F::PARMRK;
        tcsetattr(slave, SetArg::TCSANOW, &t).unwrap();
        let path = nix::unistd::ttyname(slave).unwrap();
        let _port = SerialportIo::open(path.to_str().unwrap(), pty_baud(10_400)).unwrap();
        let t = tcgetattr(slave).unwrap();
        assert!(
            !t.input_flags.intersects(F::IGNBRK | F::BRKINT | F::PARMRK),
            "{:?}",
            t.input_flags
        );
    }

    #[test]
    fn pty_baud_is_zero_only_on_macos() {
        let want = if cfg!(any(target_os = "macos", target_os = "ios")) {
            0
        } else {
            10_400
        };
        assert_eq!(pty_baud(10_400), want);
    }

    #[test]
    fn read_returns_exactly_count_or_partial_on_total_timeout() {
        let (mut l, m) = line(0);
        m.0.borrow_mut()
            .rx
            .extend([(10_000, 0x55), (20_000, 0xD0), (600_000, 0x8F)]);
        let got = l.read(3, 400).unwrap();
        assert_eq!(got, vec![0x55, 0xD0]);
        assert_eq!(
            m.0.borrow().now_us,
            400_000,
            "total timeout, not inter-byte"
        );
        let got = l.read(1, 400).unwrap();
        assert_eq!(got, vec![0x8F]);
    }

    #[test]
    fn break_loopback_is_inferred_from_rx_bytes() {
        let (mut l, m) = line(0);
        // Junk before the break must not count as loopback.
        m.0.borrow_mut().rx.push_back((0, 0x11));
        l.set_break(true).unwrap();
        assert!(
            !l.break_loopback_seen().unwrap(),
            "no byte during break yet"
        );
        m.0.borrow_mut().rx.push_back((5_000, 0x00));
        l.sleep_ms(200);
        assert!(l.break_loopback_seen().unwrap());
    }

    #[test]
    fn write_with_inter_byte_gap_goes_byte_by_byte() {
        let (mut l, m) = line(0);
        l.write(&[0x82, 0x7A, 0xF1], 7).unwrap();
        let writes: Vec<(Vec<u8>, u64)> =
            m.0.borrow()
                .calls
                .iter()
                .filter_map(|c| match c {
                    Call::Write(b, t) => Some((b.clone(), *t)),
                    _ => None,
                })
                .collect();
        assert_eq!(
            writes,
            vec![(vec![0x82], 0), (vec![0x7A], 8_000), (vec![0xF1], 16_000)]
        );
        l.write(&[1, 2], 0).unwrap();
        assert!(matches!(m.0.borrow().calls.last(), Some(Call::Write(b, _)) if b == &[1, 2]));
    }

    #[test]
    fn flush_drains_then_clears() {
        let (mut l, m) = line(0);
        m.0.borrow_mut().rx.extend([(0, 0xAA), (0, 0xBB)]);
        l.flush_rx().unwrap();
        assert!(m.0.borrow().rx.is_empty());
        assert!(matches!(m.0.borrow().calls.last(), Some(Call::Clear(_))));
    }
}
