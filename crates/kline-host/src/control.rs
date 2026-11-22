//! Client of the simulator's control socket (docs/sim.md "pty transport"): `BREAK 1|0`,
//! `DTR 1|0`, `RTS 1|0` answered `OK`, `LOOPBACK?` answered `1|0`. Written from the documented protocol so
//! the product binaries do not link the simulator.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// Reply timeout of one control command (ours).
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);

pub struct ControlClient {
    w: UnixStream,
    r: BufReader<UnixStream>,
}

impl ControlClient {
    pub fn connect(path: &Path) -> io::Result<ControlClient> {
        let w = UnixStream::connect(path)?;
        w.set_read_timeout(Some(CONTROL_TIMEOUT))?;
        let r = BufReader::new(w.try_clone()?);
        Ok(ControlClient { w, r })
    }

    fn cmd(&mut self, c: &str) -> io::Result<String> {
        self.w.write_all(format!("{c}\n").as_bytes())?;
        let mut line = String::new();
        self.r.read_line(&mut line)?;
        let line = line.trim().to_string();
        if line.is_empty() || line == "ERR" {
            return Err(io::Error::other(format!(
                "simulator control command {c:?} failed: {line:?}"
            )));
        }
        Ok(line)
    }

    pub fn set_break(&mut self, on: bool) -> io::Result<()> {
        self.cmd(if on { "BREAK 1" } else { "BREAK 0" }).map(drop)
    }

    pub fn set_dtr(&mut self, on: bool) -> io::Result<()> {
        self.cmd(if on { "DTR 1" } else { "DTR 0" }).map(drop)
    }

    pub fn set_rts(&mut self, on: bool) -> io::Result<()> {
        self.cmd(if on { "RTS 1" } else { "RTS 0" }).map(drop)
    }

    pub fn loopback_seen(&mut self) -> io::Result<bool> {
        Ok(self.cmd("LOOPBACK?")? == "1")
    }
}
