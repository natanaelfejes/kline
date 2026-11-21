//! A diagnostic session after a successful init: requests, idle TesterPresent, Stop.
//!
//! Time comes in as `now_ms` values from the caller (any monotonic millisecond clock). The
//! session uses them only to compute the P3 / OnError gap still owed before the next write and
//! when TesterPresent is due; it never reads a clock itself.

use thiserror::Error;

use crate::error::LinkError;
use crate::exchange::{Exchange, ExchangeParams};
use crate::frame::{FrameError, Framing, KwpHeader, Reply};
use crate::init::InitOutcome;
use crate::io::{Action, Event, Step};
use crate::safety::{self, Approval, Denied, Protocol, Target};
use crate::timing::{InitKind, KTiming, LinkConfig};

/// ISO8 TesterPresent payload. RE: KWP2000Diag4 ISO8Ecu init.
pub const ISO8_TESTER_PRESENT: u8 = 0x36;
/// ISO8 Stop payload. RE: KWP2000Diag4 ISO8Ecu init.
pub const ISO8_STOP: u8 = 0x3F;
/// KWP TesterPresent payload. RE: KWP2000Diag4 KWP2000Ecu init.
pub const KWP_TESTER_PRESENT: u8 = 0x3E;
/// KWP StopCommunication payload. RE: KWP2000Diag4 KWP2000Ecu init.
pub const KWP_STOP_COMMUNICATION: u8 = 0x82;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionError {
    /// An exchange is already running; finish it first.
    #[error("an exchange is in progress")]
    Busy,
    /// Stop was sent; make a new init and session.
    #[error("session closed")]
    Closed,
    /// Refused by the safety policy. Nothing was written.
    #[error(transparent)]
    Denied(#[from] Denied),
    /// The payload cannot be framed.
    #[error("cannot frame request: {0}")]
    Frame(FrameError),
    /// The exchange failed on the line.
    #[error(transparent)]
    Link(LinkError),
}

impl From<LinkError> for SessionError {
    fn from(e: LinkError) -> Self {
        match e {
            LinkError::Frame(f) => SessionError::Frame(f),
            LinkError::Denied(d) => SessionError::Denied(d),
            e => SessionError::Link(e),
        }
    }
}

/// Result of one finished exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOutcome {
    /// Reply to a request (positive, negative or ISO8 status).
    Reply(Reply),
    /// TesterPresent sent; its reply if the ECU gave one (discarded by DDT2000).
    TesterPresent(Option<Reply>),
    /// Stop sent; its reply if any. The session is closed.
    Stopped(Option<Reply>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Request,
    TesterPresent,
    Stop,
}

#[derive(Debug, Clone)]
pub struct Session {
    cfg: LinkConfig,
    t: KTiming,
    framing: Framing,
    target: Target,
    current: Option<(Kind, Exchange)>,
    last_end_ms: u64,
    gap_ms: u32,
    next_tp_ms: u64,
    closed: bool,
}

impl Session {
    /// Session after `init` completed at `now_ms`.
    ///
    /// Framing: ISO8, or KWP with the length-in-format rule applied to the received KW1.
    /// TesterPresent timing follows DDT2000 (RE: IOTBD32V3 worker): due at once after a 5-baud
    /// init (FirstRequest was already waited inside the init), and TP - FirstRequest after a
    /// fast init.
    pub fn new(cfg: &LinkConfig, init: &InitOutcome, now_ms: u64) -> Session {
        // Timing of the variant in use; the init kind is what actually ran.
        let mut t = cfg.effective_timing();
        t.init = init.kind;
        let (framing, protocol) = if t.init.is_iso8() {
            (Framing::Iso8, Protocol::Iso8)
        } else {
            (
                Framing::Kwp(KwpHeader {
                    target: cfg.ecu_address,
                    source: cfg.tool_address,
                    length_in_format: KwpHeader::length_in_format_for_kw1(init.kw1),
                }),
                Protocol::Kwp,
            )
        };
        let (gap_ms, next_tp_ms) = match t.init {
            InitKind::KwpFast { .. } => (
                t.reply_to_request_ms,
                now_ms + u64::from(t.tester_present_ms.saturating_sub(t.first_request_ms)),
            ),
            _ => (0, now_ms),
        };
        Session {
            cfg: cfg.clone(),
            t,
            framing,
            target: Target {
                protocol,
                ecu_address: cfg.ecu_address,
            },
            current: None,
            last_end_ms: now_ms,
            gap_ms,
            next_tp_ms,
            closed: false,
        }
    }

    pub fn framing(&self) -> Framing {
        self.framing
    }

    pub fn target(&self) -> Target {
        self.target
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn is_busy(&self) -> bool {
        self.current.is_some()
    }

    /// True when the line has been idle for TesterPresent delay and nothing is running.
    pub fn tester_present_due(&self, now_ms: u64) -> bool {
        !self.closed && self.current.is_none() && now_ms >= self.next_tp_ms
    }

    /// Send a request. The payload is checked against the safety policy before anything
    /// is queued; a refusal writes nothing.
    pub fn request(&mut self, payload: &[u8], now_ms: u64) -> Result<Action, SessionError> {
        self.ready()?;
        let permit = safety::check(&self.target, payload)?;
        self.begin(Kind::Request, permit, now_ms)
    }

    /// Send the routine an [`Approval`] stands for (BMT 31C7/31C8 only).
    pub fn request_approved(
        &mut self,
        approval: Approval,
        now_ms: u64,
    ) -> Result<Action, SessionError> {
        self.ready()?;
        let permit = safety::check_approved(&self.target, approval)?;
        self.begin(Kind::Request, permit, now_ms)
    }

    /// Send TesterPresent (`36` ISO8, `3E` KWP). A missing reply is not an error.
    pub fn tester_present(&mut self, now_ms: u64) -> Result<Action, SessionError> {
        self.ready()?;
        let payload = if self.framing == Framing::Iso8 {
            ISO8_TESTER_PRESENT
        } else {
            KWP_TESTER_PRESENT
        };
        let permit = safety::check(&self.target, &[payload])?;
        self.begin(Kind::TesterPresent, permit, now_ms)
    }

    /// Send Stop (`3F` ISO8, `82` KWP). A missing reply is not an error. Closes the session.
    pub fn stop(&mut self, now_ms: u64) -> Result<Action, SessionError> {
        self.ready()?;
        let payload = if self.framing == Framing::Iso8 {
            ISO8_STOP
        } else {
            KWP_STOP_COMMUNICATION
        };
        let permit = safety::check(&self.target, &[payload])?;
        self.begin(Kind::Stop, permit, now_ms)
    }

    fn ready(&self) -> Result<(), SessionError> {
        if self.closed {
            Err(SessionError::Closed)
        } else if self.current.is_some() {
            Err(SessionError::Busy)
        } else {
            Ok(())
        }
    }

    fn begin(
        &mut self,
        kind: Kind,
        permit: safety::Permit,
        now_ms: u64,
    ) -> Result<Action, SessionError> {
        let t = &self.t;
        let owed = (self.last_end_ms + u64::from(self.gap_ms)).saturating_sub(now_ms);
        let params = ExchangeParams {
            gap_ms: u32::try_from(owed).unwrap_or(u32::MAX),
            inter_byte_ms: t.inter_byte_ms,
            read_timeout_ms: t.request_to_reply_timeout_ms,
            pending_timeout_ms: self.cfg.response_pending_timeout_ms,
            max_pending: self.cfg.max_response_pending,
            reply_optional: kind != Kind::Request,
        };
        let mut ex = Exchange::new(self.framing, permit, params)?;
        let a = ex.start();
        self.current = Some((kind, ex));
        Ok(a)
    }

    /// Feed the event answering the last action. `now_ms` is when it happened.
    pub fn on_event(&mut self, ev: Event, now_ms: u64) -> Step<SessionOutcome, SessionError> {
        let Some((kind, mut ex)) = self.current.take() else {
            return Step::Failed(SessionError::Link(LinkError::UnexpectedEvent));
        };
        let step = ex.on_event(ev);
        let t = self.t;
        let ok_gap = |r: &Option<Reply>| match r {
            Some(Reply::Negative { .. } | Reply::Iso8Status(_)) => t.on_error_ms,
            _ => t.reply_to_request_ms,
        };
        let result = match step {
            Step::Act(a) => {
                self.current = Some((kind, ex));
                return Step::Act(a);
            }
            Step::Failed(e) => {
                self.gap_ms = t.on_error_ms;
                Step::Failed(e.into())
            }
            Step::Done(r) => {
                self.gap_ms = ok_gap(&r);
                match (kind, r) {
                    (Kind::Request, Some(r)) => Step::Done(SessionOutcome::Reply(r)),
                    // Not produced: requests are never reply-optional. Kept total.
                    (Kind::Request, None) => {
                        self.gap_ms = t.on_error_ms;
                        Step::Failed(LinkError::NoReply.into())
                    }
                    (Kind::TesterPresent, r) => Step::Done(SessionOutcome::TesterPresent(r)),
                    (Kind::Stop, r) => Step::Done(SessionOutcome::Stopped(r)),
                }
            }
        };
        if kind == Kind::Stop {
            self.closed = true;
        }
        self.finish(now_ms, result)
    }

    fn finish(
        &mut self,
        now_ms: u64,
        step: Step<SessionOutcome, SessionError>,
    ) -> Step<SessionOutcome, SessionError> {
        self.last_end_ms = now_ms;
        self.next_tp_ms = now_ms + u64::from(self.t.tester_present_ms);
        step
    }
}
