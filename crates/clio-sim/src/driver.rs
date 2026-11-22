//! Runs clio-core's sans-IO machines over a [`Line`], optionally capturing a JSONL-able log
//! of what the tester wrote and read (the format [`crate::replay`] plays back).
//!
//! This is a test driver. The CLI will own the real one; it should record the same log.

use std::io;

use clio_core::init::{FastInit, FiveBaudInit, InitOutcome};
#[cfg(feature = "approved-routines")]
use clio_core::safety::Approval;
use clio_core::session::{Session, SessionError, SessionOutcome};
use clio_core::{Action, InitKind, LinkConfig, LinkError, Step};
use thiserror::Error;

use crate::line::Line;
use crate::replay::{Dir, EntryKind, LogEntry};

#[derive(Debug, Error)]
pub enum DriveError {
    #[error("transport: {0}")]
    Io(#[from] io::Error),
    #[error("init: {0}")]
    Init(LinkError),
    #[error("session: {0}")]
    Session(SessionError),
}

impl DriveError {
    /// The session error, if this is one (tests match on it).
    pub fn session(&self) -> Option<&SessionError> {
        match self {
            DriveError::Session(e) => Some(e),
            _ => None,
        }
    }

    pub fn init(&self) -> Option<&LinkError> {
        match self {
            DriveError::Init(e) => Some(e),
            _ => None,
        }
    }
}

pub struct Driver<L: Line> {
    pub line: L,
    capture: Option<Vec<LogEntry>>,
    init_marker: Option<(EntryKind, Vec<u8>)>,
}

impl<L: Line> Driver<L> {
    pub fn new(line: L) -> Driver<L> {
        Driver {
            line,
            capture: None,
            init_marker: None,
        }
    }

    /// Like `new`, recording every write and read.
    pub fn capturing(line: L) -> Driver<L> {
        Driver {
            capture: Some(Vec::new()),
            ..Driver::new(line)
        }
    }

    /// The captured log (empty if not capturing).
    pub fn captured(&self) -> &[LogEntry] {
        self.capture.as_deref().unwrap_or(&[])
    }

    fn act(&mut self, a: &Action) -> io::Result<clio_core::Event> {
        let t = self.line.now_ms();
        if let (Action::SetBreak(true), Some((kind, bytes))) = (a, &self.init_marker) {
            // First break of the init: where the wake-up starts on the wire.
            let e = LogEntry::new(t, Dir::Tx, bytes).with_kind(*kind);
            self.init_marker = None;
            if let Some(c) = &mut self.capture {
                c.push(e);
            }
        }
        let ev = self.line.perform(a)?;
        if let Some(c) = &mut self.capture {
            match (a, &ev) {
                (Action::Write { frame, .. }, _) => {
                    c.push(LogEntry::new(t, Dir::Tx, frame.bytes()))
                }
                (
                    Action::Read { .. },
                    clio_core::Event::ReadOk(b) | clio_core::Event::ReadTimeout(b),
                ) if !b.is_empty() => c.push(LogEntry::new(self.line.now_ms(), Dir::Rx, b)),
                _ => {}
            }
        }
        Ok(ev)
    }

    /// Run the init `cfg` selects (5-baud or fast).
    pub fn init(&mut self, cfg: &LinkConfig) -> Result<InitOutcome, DriveError> {
        match cfg.init_kind() {
            InitKind::KwpFast { .. } => {
                let mut m = FastInit::new(cfg).map_err(DriveError::Init)?;
                self.init_marker = Some((EntryKind::FastInit, Vec::new()));
                let first = m.start();
                self.pump(first, |ev, _| m.on_event(ev), DriveError::Init)
            }
            _ => {
                let mut m = FiveBaudInit::new(cfg).map_err(DriveError::Init)?;
                self.init_marker = Some((EntryKind::FiveBaud, vec![m.address_byte()]));
                let first = m.start();
                self.pump(first, |ev, _| m.on_event(ev), DriveError::Init)
            }
        }
    }

    /// Session on the link `init` opened, starting now. Consumes the outcome: one init, one
    /// session.
    pub fn session(&self, init: InitOutcome) -> Session {
        Session::new(init, self.line.now_ms())
    }

    pub fn request(
        &mut self,
        s: &mut Session,
        payload: &[u8],
    ) -> Result<SessionOutcome, DriveError> {
        let first = s
            .request(payload, self.line.now_ms())
            .map_err(DriveError::Session)?;
        self.run(s, first)
    }

    /// Send the payload `approval` stands for (feature `approved-routines`).
    #[cfg(feature = "approved-routines")]
    pub fn request_approved(
        &mut self,
        s: &mut Session,
        approval: Approval,
    ) -> Result<SessionOutcome, DriveError> {
        let payload = approval.payload().to_vec();
        let first = s
            .request_approved(&payload, approval, self.line.now_ms())
            .map_err(DriveError::Session)?;
        self.run(s, first)
    }

    pub fn tester_present(&mut self, s: &mut Session) -> Result<SessionOutcome, DriveError> {
        let first = s
            .tester_present(self.line.now_ms())
            .map_err(DriveError::Session)?;
        self.run(s, first)
    }

    pub fn stop(&mut self, s: &mut Session) -> Result<SessionOutcome, DriveError> {
        let first = s.stop(self.line.now_ms()).map_err(DriveError::Session)?;
        self.run(s, first)
    }

    fn run(&mut self, s: &mut Session, first: Action) -> Result<SessionOutcome, DriveError> {
        self.pump(first, |ev, now| s.on_event(ev, now), DriveError::Session)
    }

    /// Perform actions until the machine finishes.
    fn pump<T, E>(
        &mut self,
        first: Action,
        mut feed: impl FnMut(clio_core::Event, u64) -> Step<T, E>,
        wrap: fn(E) -> DriveError,
    ) -> Result<T, DriveError> {
        let mut a = first;
        loop {
            let ev = self.act(&a)?;
            match feed(ev, self.line.now_ms()) {
                Step::Act(next) => a = next,
                Step::Done(t) => return Ok(t),
                Step::Failed(e) => return Err(wrap(e)),
            }
        }
    }
}
