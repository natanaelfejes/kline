//! Smoke test: a serialport-based tester opens the simulator's pty like the FTDI cable and runs
//! init, a live-data read and Stop in real time. Break and DTR go over the control socket.
#![cfg(unix)]

mod common;

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use clio_sim::bus::{Bus, BusConfig};
use clio_sim::driver::Driver;
use clio_sim::ecu::{EcuProfile, SpecEcu};
use clio_sim::line::Line;
use clio_sim::pty::{ControlClient, PtySim};
use clio_sim::timing::Tolerance;
use common::*;

/// What a CLI transport for the simulator looks like: data through the serial port,
/// break / DTR / loopback through the control socket.
struct SerialLine {
    port: Box<dyn serialport::SerialPort>,
    ctl: ControlClient,
    t0: Instant,
}

impl Line for SerialLine {
    fn now_ms(&self) -> u64 {
        self.t0.elapsed().as_millis() as u64
    }
    fn set_dtr(&mut self, on: bool) -> io::Result<()> {
        // TIOCMSET fails on a pty (ENOTTY); the control socket carries DTR.
        self.ctl.set_dtr(on)
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
        std::thread::sleep(Duration::from_millis(u64::from(ms)));
    }
    fn flush_rx(&mut self) -> io::Result<()> {
        self.port
            .clear(serialport::ClearBuffer::Input)
            .map_err(io::Error::other)
    }
    fn write(&mut self, bytes: &[u8], inter_byte_ms: u32) -> io::Result<()> {
        if inter_byte_ms == 0 {
            self.port.write_all(bytes)?;
        } else {
            for (i, b) in bytes.iter().enumerate() {
                if i > 0 {
                    self.sleep_ms(inter_byte_ms);
                }
                self.port.write_all(std::slice::from_ref(b))?;
            }
        }
        self.port.flush()
    }
    fn read(&mut self, count: usize, timeout_ms: u32) -> io::Result<Vec<u8>> {
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
    let ctl = std::env::temp_dir().join(format!("clio-sim-{}.ctl", std::process::id()));
    let _ = std::fs::remove_file(&ctl);
    let sim = match PtySim::spawn(bus, &ctl) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("SKIPPED: pty not available here ({e})");
            return;
        }
    };
    let port = serialport::new(sim.tty_path().to_string_lossy(), 10_400)
        .timeout(Duration::from_millis(50))
        .open()
        .expect("open pty slave with serialport");
    let line = SerialLine {
        port,
        ctl: ControlClient::connect(sim.control_path()).unwrap(),
        t0: Instant::now(),
    };
    let mut d = Driver::new(line);
    let cfg = cfg(&sirius());
    let init = d.init(&cfg).unwrap();
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
