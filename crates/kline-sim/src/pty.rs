//! The simulator behind a pseudo-terminal, so a serialport-based tester opens it like the
//! FTDI cable (`/dev/pts/N` on Linux, `/dev/ttysNNN` on macOS).
//!
//! # Break and DTR over a pty
//!
//! A pty has no K line: `tcsendbreak`/`TIOCSBRK` on the slave succeed but the master never
//! sees them, and `TIOCMSET` (DTR) fails with ENOTTY (checked on Linux 6.18 with serialport
//! 4.10). The 5-baud and fast-init wake-ups are made of break edges, so they travel over an
//! explicit **side channel**: a Unix stream socket next to the pty ([`PtySim::control_path`]).
//! Line protocol, one command per line, each answered with one line:
//!
//! | Command | Meaning | Reply |
//! |---|---|---|
//! | `BREAK 1` / `BREAK 0` | TX break on / off (K low / released) | `OK` |
//! | `DTR 1` / `DTR 0` | DTR high / low (logged only) | `OK` |
//! | `RTS 1` / `RTS 0` | RTS high / low (logged only) | `OK` |
//! | `LOOPBACK?` | was the last break seen on RX? | `1` or `0` |
//!
//! The command is applied when it arrives (wall clock), and the reply is sent after, so a
//! tester that waits for `OK` has the edge on the bus before it sleeps. A tester driver for the
//! sim forwards `set_break`/`set_dtr`/loopback checks to [`ControlClient`] instead of (or as
//! well as) the serial port. Data bytes go through the pty and are echoed back like the KKL
//! cable does. The break residue byte (0x00 on release) is written to the pty as well.
//!
//! Timing: wall clock in ms, polled every [`POLL_MS`]; jitter of a few ms is normal, so use
//! [`crate::timing::Tolerance::lenient`] for pty tests.

use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, SetArg};

use crate::bus::Bus;

/// Poll period of the simulator thread (ours).
pub const POLL_MS: u64 = 1;

/// A running simulator on a pty. Dropping it stops the thread and removes the socket.
pub struct PtySim {
    tty: PathBuf,
    ctl: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn nix_io(e: nix::Error) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}

fn set_nonblocking(fd: impl AsFd) -> io::Result<()> {
    let fd = fd.as_fd();
    let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL).map_err(nix_io)?);
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).map_err(nix_io)?;
    Ok(())
}

impl PtySim {
    /// Open a pty, bind the control socket at `control_path` (must not exist) and run `bus`
    /// in a thread.
    pub fn spawn(bus: Bus, control_path: impl Into<PathBuf>) -> io::Result<PtySim> {
        let ctl: PathBuf = control_path.into();
        let pty = nix::pty::openpty(None, None).map_err(nix_io)?;
        let tty = nix::unistd::ttyname(&pty.slave).map_err(nix_io)?;
        // Raw slave until the tester sets its own mode (no echo, no line editing).
        let mut t = tcgetattr(&pty.slave).map_err(nix_io)?;
        cfmakeraw(&mut t);
        tcsetattr(&pty.slave, SetArg::TCSANOW, &t).map_err(nix_io)?;
        set_nonblocking(&pty.master)?;
        let listener = UnixListener::bind(&ctl)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let master = pty.master;
        // Keep a slave fd open so the master never sees a hang-up between tester opens.
        let slave = pty.slave;
        let thread = std::thread::spawn(move || run(bus, master, slave, listener, st));
        Ok(PtySim {
            tty,
            ctl,
            stop,
            thread: Some(thread),
        })
    }

    /// Path the tester opens as its serial port.
    pub fn tty_path(&self) -> &Path {
        &self.tty
    }

    pub fn control_path(&self) -> &Path {
        &self.ctl
    }
}

impl Drop for PtySim {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.ctl);
    }
}

struct Conn {
    stream: UnixStream,
    buf: Vec<u8>,
}

fn run(
    mut bus: Bus,
    master: OwnedFd,
    _slave: OwnedFd,
    listener: UnixListener,
    stop: Arc<AtomicBool>,
) {
    let t0 = Instant::now();
    let now = || t0.elapsed().as_millis() as u64;
    let mut master = std::fs::File::from(master);
    let mut conns: Vec<Conn> = Vec::new();
    let mut buf = [0u8; 256];
    while !stop.load(Ordering::SeqCst) {
        if let Ok((s, _)) = listener.accept() {
            if s.set_nonblocking(true).is_ok() {
                conns.push(Conn {
                    stream: s,
                    buf: Vec::new(),
                });
            }
        }
        conns.retain_mut(|c| serve(c, &mut bus, now()));
        // Errors: WouldBlock (nothing to read), EIO (no slave open; we hold one).
        if let Ok(n) = master.read(&mut buf) {
            let t = now();
            for &b in &buf[..n] {
                bus.tester_byte(b, t);
            }
        }
        let t = now();
        bus.advance(t);
        let mut outb = Vec::new();
        while let Some(b) = bus.pop_rx(t) {
            outb.push(b);
        }
        if !outb.is_empty() {
            let _ = master.write_all(&outb);
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
}

/// Handle the commands that arrived on one control connection. False: connection closed.
fn serve(c: &mut Conn, bus: &mut Bus, t: u64) -> bool {
    let mut tmp = [0u8; 64];
    loop {
        match c.stream.read(&mut tmp) {
            Ok(0) => return false,
            Ok(n) => c.buf.extend_from_slice(&tmp[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(_) => return false,
        }
    }
    while let Some(pos) = c.buf.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = c.buf.drain(..=pos).collect();
        let cmd = String::from_utf8_lossy(&line).trim().to_string();
        let reply = match cmd.as_str() {
            "BREAK 1" => {
                bus.tester_break(true, t);
                "OK"
            }
            "BREAK 0" => {
                bus.tester_break(false, t);
                "OK"
            }
            "DTR 1" => {
                bus.tester_dtr(true, t);
                "OK"
            }
            "DTR 0" => {
                bus.tester_dtr(false, t);
                "OK"
            }
            "RTS 1" => {
                bus.tester_rts(true, t);
                "OK"
            }
            "RTS 0" => {
                bus.tester_rts(false, t);
                "OK"
            }
            "LOOPBACK?" => {
                if bus.break_loopback_seen() {
                    "1"
                } else {
                    "0"
                }
            }
            _ => "ERR",
        };
        if c.stream.write_all(format!("{reply}\n").as_bytes()).is_err() {
            return false;
        }
    }
    true
}

/// Tester side of the control socket.
pub struct ControlClient {
    w: UnixStream,
    r: BufReader<UnixStream>,
}

impl ControlClient {
    pub fn connect(path: impl AsRef<Path>) -> io::Result<ControlClient> {
        let w = UnixStream::connect(path)?;
        w.set_read_timeout(Some(Duration::from_secs(2)))?;
        let r = BufReader::new(w.try_clone()?);
        Ok(ControlClient { w, r })
    }

    fn cmd(&mut self, c: &str) -> io::Result<String> {
        self.w.write_all(format!("{c}\n").as_bytes())?;
        let mut line = String::new();
        self.r.read_line(&mut line)?;
        let line = line.trim().to_string();
        if line == "ERR" || line.is_empty() {
            return Err(io::Error::other(format!(
                "control command {c:?} failed: {line:?}"
            )));
        }
        Ok(line)
    }

    pub fn set_break(&mut self, on: bool) -> io::Result<()> {
        self.cmd(if on { "BREAK 1" } else { "BREAK 0" }).map(|_| ())
    }

    pub fn set_dtr(&mut self, on: bool) -> io::Result<()> {
        self.cmd(if on { "DTR 1" } else { "DTR 0" }).map(|_| ())
    }

    pub fn set_rts(&mut self, on: bool) -> io::Result<()> {
        self.cmd(if on { "RTS 1" } else { "RTS 0" }).map(|_| ())
    }

    pub fn loopback_seen(&mut self) -> io::Result<bool> {
        Ok(self.cmd("LOOPBACK?")? == "1")
    }
}
