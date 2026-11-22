//! Smoke test: a serialport-based tester opens the simulator's pty like the FTDI cable and runs
//! init, a live-data read and Stop in real time. Break and DTR go over the control socket.
#![cfg(unix)]

mod common;

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use common::*;
use kline_sim::bus::{Bus, BusConfig};
use kline_sim::driver::Driver;
use kline_sim::ecu::{EcuProfile, SpecEcu};
use kline_sim::line::Line;
use kline_sim::pty::{ControlClient, PtySim};
use kline_sim::timing::Tolerance;

/// What a CLI transport for the simulator looks like: data through the serial port,
/// break / DTR / loopback through the control socket.
///
/// Sleeps that follow a break edge are paced from the previous sleep's deadline, not from
/// "now", as `kline_host`'s `KklLine` does. The simulator samples each 5-baud bit in its middle
/// on a 200 ms grid from the start bit, so a plain `thread::sleep` per bit lets the lateness add
/// up: on the GitHub macOS runner (macos-26-arm64, VirtualMac2,1) `thread::sleep(200 ms)` woke
/// up to 83 ms late, the edges drifted by more than half a bit from the second data bit on, the
/// simulator decoded the address 0x7A as 0xF4 (each bit sampled one bit late), did not accept it
/// and sent no keywords (`NoKeywords { received: [] }`). Paced, an edge is late only by its own
/// sleep, and the short chunks plus the spin tail keep that to a few ms.
struct SerialLine {
    port: Box<dyn serialport::SerialPort>,
    ctl: ControlClient,
    t0: Instant,
    /// Deadline of the last sleep since a break edge; `None` after DTR, flush, write or read.
    pace: Option<Instant>,
}

/// Longest single OS sleep (ours): a short sleep wakes up less late than a long one on the
/// GitHub macOS runner (1 ms: median 6.6 ms, max 7.6 ms; 200 ms: up to 83 ms late).
const SLEEP_CHUNK: Duration = Duration::from_millis(5);
/// Busy-wait the last this much of every sleep (ours: above the 1 ms sleep's 7.6 ms worst
/// wake-up there).
const SPIN_TAIL: Duration = Duration::from_millis(8);

/// OS-sleep in short chunks until [`SPIN_TAIL`] before `target`, then spin.
fn sleep_until(target: Instant) {
    loop {
        let left = target.saturating_duration_since(Instant::now());
        if left <= SPIN_TAIL {
            break;
        }
        std::thread::sleep((left - SPIN_TAIL).min(SLEEP_CHUNK));
    }
    while Instant::now() < target {
        std::hint::spin_loop();
    }
}

impl Line for SerialLine {
    fn now_ms(&self) -> u64 {
        self.t0.elapsed().as_millis() as u64
    }
    fn set_dtr(&mut self, on: bool) -> io::Result<()> {
        // Every init starts with a DTR edge; pacing starts after it (as in KklLine).
        self.pace = None;
        // TIOCMSET fails on a pty (ENOTTY); the control socket carries DTR.
        self.ctl.set_dtr(on)
    }
    fn set_rts(&mut self, on: bool) -> io::Result<()> {
        self.ctl.set_rts(on)
    }
    fn set_break(&mut self, on: bool) -> io::Result<()> {
        // A no-op on a pty, kept so the call sequence is the real one.
        let _ = if on {
            self.port.set_break()
        } else {
            self.port.clear_break()
        };
        self.ctl.set_break(on)
    }
    fn break_loopback_seen(&mut self) -> io::Result<bool> {
        self.ctl.loopback_seen()
    }
    fn sleep_ms(&mut self, ms: u32) {
        let d = Duration::from_millis(u64::from(ms));
        let target = self.pace.unwrap_or_else(Instant::now) + d;
        sleep_until(target);
        self.pace = Some(target);
    }
    fn flush_rx(&mut self) -> io::Result<()> {
        self.pace = None;
        self.port
            .clear(serialport::ClearBuffer::Input)
            .map_err(io::Error::other)
    }
    fn write(&mut self, bytes: &[u8], inter_byte_ms: u32) -> io::Result<()> {
        self.pace = None;
        if inter_byte_ms == 0 {
            self.port.write_all(bytes)?;
        } else {
            for (i, b) in bytes.iter().enumerate() {
                if i > 0 {
                    sleep_until(Instant::now() + Duration::from_millis(u64::from(inter_byte_ms)));
                }
                self.port.write_all(std::slice::from_ref(b))?;
            }
        }
        self.port.flush()
    }
    fn read(&mut self, count: usize, timeout_ms: u32) -> io::Result<Vec<u8>> {
        self.pace = None;
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        let mut out = vec![0u8; count];
        let mut n = 0;
        while n < count {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            self.port.set_timeout(left).map_err(io::Error::other)?;
            match self.port.read(&mut out[n..]) {
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::TimedOut => break,
                Err(e) => return Err(e),
            }
        }
        out.truncate(n);
        Ok(out)
    }
}

#[test]
fn pty_smoke_session() {
    let mut profile = EcuProfile::sirius32();
    profile.tolerance = Tolerance::lenient();
    let ecu = SpecEcu::new(profile, sirius(), sirius_scenario()).unwrap();
    let journal = ecu.journal();
    let bus = Bus::new(
        ecu,
        BusConfig::default(),
        Tolerance::lenient(),
        journal.clone(),
    );
    let ctl = std::env::temp_dir().join(format!("kline-sim-{}.ctl", std::process::id()));
    let _ = std::fs::remove_file(&ctl);
    let sim = match PtySim::spawn(bus, &ctl) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("SKIPPED: pty not available here ({e})");
            return;
        }
    };
    // macOS: serialport sets a speed above 0 with IOSSIOSPEED, which a pty refuses (ENOTTY);
    // the pty has no line speed, so open it with 0 there (Hydrogen final review).
    let baud = if cfg!(any(target_os = "macos", target_os = "ios")) {
        0
    } else {
        10_400
    };
    let port = serialport::new(sim.tty_path().to_string_lossy(), baud)
        .timeout(Duration::from_millis(50))
        .open()
        .expect("open pty slave with serialport");
    let line = SerialLine {
        port,
        ctl: ControlClient::connect(sim.control_path()).unwrap(),
        t0: Instant::now(),
        pace: None,
    };
    let mut d = Driver::new(line);
    let cfg = cfg(&sirius());
    // On failure show what the simulator decoded (e.g. "5-baud address 0xf4 not accepted").
    let init = d.init(&cfg).unwrap_or_else(|e| {
        panic!(
            "init failed: {e:?}; simulator notes {:?}, violations {:?}",
            journal.notes(),
            journal.violations()
        )
    });
    assert_eq!(init.keywords(), (0xD0, 0x8F));
    assert_eq!(init.address_complement(), Some(0x85));
    let mut s = d.session(init);
    let r = positive(&d.request(&mut s, &[0x21, 0xA9]).unwrap());
    assert_eq!(
        decode(&sirius(), "Live A9", &r),
        expected(&sirius(), "Live A9", &sirius_scenario())
    );
    d.stop(&mut s).unwrap();
    journal.assert_no_writes();
    // Raw bytes echo back like the KKL cable (ECU asleep after Stop, so only the echo).
    d.line.port.write_all(&[0xAB, 0xCD]).unwrap();
    let mut b = [0u8; 2];
    d.line.port.set_timeout(Duration::from_millis(500)).unwrap();
    d.line.port.read_exact(&mut b).unwrap();
    assert_eq!(b, [0xAB, 0xCD]);
    drop(sim);
    assert!(!ctl.exists(), "control socket removed on drop");
}
