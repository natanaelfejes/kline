//! A diagnostic session after a successful init: requests, idle TesterPresent, Stop.
//!
//! Time comes in as `now_ms` values from the caller (any monotonic millisecond clock). The
//! session uses them only to compute the P3 / OnError gap still owed before the next write and
//! when TesterPresent is due; it never reads a clock itself.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use thiserror::Error;

use crate::error::LinkError;
use crate::exchange::{Exchange, ExchangeParams};
use crate::frame::{FrameError, Framing, KwpHeader, Reply};
use crate::init::InitOutcome;
use crate::io::{Action, Event, Step};
use crate::safety::{self, Denied, Protocol, ProtocolFrame, Target};
#[cfg(feature = "approved-routines")]
use crate::safety::{Approval, ApprovalChallenge, ChallengeIds, DenyReason, RiskInfo, Tier};
use crate::scan::ScanRequest;
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
    /// The request payload cannot be framed (nothing was written). Errors decoding the ECU's
    /// reply are `Link(LinkError::Frame(..))`.
    #[error("cannot frame request: {0}")]
    Frame(FrameError),
    /// The exchange failed on the line.
    #[error(transparent)]
    Link(LinkError),
    /// The OS random source failed; no approval challenge was made.
    #[error("no randomness available for an approval code")]
    NoRandomness,
}

impl From<LinkError> for SessionError {
    fn from(e: LinkError) -> Self {
        match e {
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

/// Not `Clone`: a cloned session would replay its queued exchange (security review M2
/// finding 2).
#[derive(Debug)]
pub struct Session {
    cfg: LinkConfig,
    t: KTiming,
    framing: Framing,
    target: Target,
    current: Option<Current>,
    last_end_ms: u64,
    gap_ms: u32,
    next_tp_ms: u64,
    closed: bool,
    /// Latest positive `61 lid ..` reply to `21 lid` (KWP) in this session: identification
    /// (`21 80`) and pre-read backups for Tier 2 writes. A failed or negative read of `lid`
    /// removes the entry.
    config_reads: BTreeMap<u8, Vec<u8>>,
    /// Random per-session nonce, made with the first approval challenge.
    #[cfg(feature = "approved-routines")]
    nonce: Option<u64>,
    /// Request id of the one outstanding approval challenge.
    #[cfg(feature = "approved-routines")]
    pending_approval: Option<u64>,
}

#[derive(Debug)]
struct Current {
    kind: Kind,
    exchange: Exchange,
    /// `Some(lid)` for a KWP `21 lid` request, to record its reply.
    read_lid: Option<u8>,
}

impl Session {
    /// Session on the link `init` opened, at `now_ms`. Config, init kind, key bytes and policy
    /// target all come from the outcome.
    ///
    /// Framing: ISO8, or KWP with the length-in-format rule applied to the received KW1.
    /// Both init machines already waited FirstRequest, so no gap is owed. TesterPresent timing
    /// follows DDT2000 (RE: IOTBD32V3 worker): due at once after a 5-baud init, and after
    /// TP - FirstRequest after a fast init (TP at TesterPresent delay from StartCommunication).
    pub fn new(init: InitOutcome, now_ms: u64) -> Session {
        let cfg = init.config().clone();
        let mut t = cfg.effective_timing();
        t.init = init.kind();
        let target = init.target();
        let framing = match target.protocol {
            Protocol::Iso8 => Framing::Iso8,
            Protocol::Kwp => Framing::Kwp(KwpHeader {
                target: target.ecu_address,
                source: cfg.tool_address,
                length_in_format: KwpHeader::length_in_format_for_kw1(init.keywords().0),
            }),
        };
        let next_tp_ms = match t.init {
            InitKind::KwpFast { .. } => {
                now_ms + u64::from(t.tester_present_ms.saturating_sub(t.first_request_ms))
            }
            _ => now_ms,
        };
        Session {
            cfg,
            t,
            framing,
            target,
            current: None,
            last_end_ms: now_ms,
            gap_ms: 0,
            next_tp_ms,
            closed: false,
            config_reads: BTreeMap::new(),
            #[cfg(feature = "approved-routines")]
            nonce: None,
            #[cfg(feature = "approved-routines")]
            pending_approval: None,
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

    /// The positive reply payload (`61 lid ..`) of the last successful `21 lid` read in this
    /// session, if any: the backup a Tier 2 write of `lid` is checked against.
    pub fn config_read(&self, lid: u8) -> Option<&[u8]> {
        self.config_reads.get(&lid).map(Vec::as_slice)
    }

    /// Send a Tier 0 request. The payload is checked against the safety policy before anything
    /// is queued; a refusal writes nothing. `81`/`82`/ISO8 `36`/`3F` are refused here: use
    /// [`Session::tester_present`] and [`Session::stop`].
    pub fn request(&mut self, payload: &[u8], now_ms: u64) -> Result<Action, SessionError> {
        self.ready()?;
        let permit = safety::check(&self.target, payload)?;
        self.begin(Kind::Request, permit, now_ms)
    }

    /// Send one read of a [`crate::scan::ScanPlan`].
    pub fn request_scan(&mut self, req: ScanRequest, now_ms: u64) -> Result<Action, SessionError> {
        self.request(req.payload(), now_ms)
    }

    /// Start a human approval for one Tier 1 (service) `payload` on this session's target.
    /// Refused unless the policy puts `payload` in Tier 1. Invalidates any earlier challenge.
    #[cfg(feature = "approved-routines")]
    pub fn service_challenge(&mut self, payload: &[u8]) -> Result<ApprovalChallenge, SessionError> {
        self.challenge(Tier::Service, payload, None)
    }

    /// Start a human approval for one Tier 2 (config write) `payload`. `identification` is the
    /// exact `21 80` reply payload the caller chose its ECU definition from; it must equal this
    /// session's latest positive `21 80` reply (BMT config indices differ between definition
    /// versions, capability catalogue 3.4). The backup read ([`safety::config_backup_read`])
    /// must also have been answered positively in this session. Invalidates any earlier
    /// challenge.
    #[cfg(feature = "approved-routines")]
    pub fn config_write_challenge(
        &mut self,
        payload: &[u8],
        identification: &[u8],
    ) -> Result<ApprovalChallenge, SessionError> {
        self.challenge(Tier::ConfigWrite, payload, Some(identification))
    }

    /// Start a human *expert* approval for one undocumented `payload` (Tier 3, unknown effect).
    /// Refused for documented requests (use their tier) and for forbidden ones. Invalidates any
    /// earlier challenge.
    #[cfg(feature = "approved-routines")]
    pub fn expert_challenge(&mut self, payload: &[u8]) -> Result<ApprovalChallenge, SessionError> {
        self.challenge(Tier::Expert, payload, None)
    }

    #[cfg(feature = "approved-routines")]
    fn challenge(
        &mut self,
        tier: Tier,
        payload: &[u8],
        identification: Option<&[u8]>,
    ) -> Result<ApprovalChallenge, SessionError> {
        self.pending_approval = None;
        let risk = self.approvable(tier, payload, identification)?;
        let nonce = match self.nonce {
            Some(n) => n,
            None => {
                let n = safety::approval_random().ok_or(SessionError::NoRandomness)?;
                self.nonce = Some(n);
                n
            }
        };
        let request = safety::approval_random().ok_or(SessionError::NoRandomness)?;
        let code = safety::approval_random().ok_or(SessionError::NoRandomness)?;
        self.pending_approval = Some(request);
        Ok(ApprovalChallenge::new(
            ChallengeIds {
                session: nonce,
                request,
                code,
            },
            self.target,
            tier,
            payload,
            identification,
            risk,
        ))
    }

    /// Check that `payload` is exactly `tier` here and its preconditions hold; its risk.
    #[cfg(feature = "approved-routines")]
    fn approvable(
        &self,
        tier: Tier,
        payload: &[u8],
        identification: Option<&[u8]>,
    ) -> Result<RiskInfo, SessionError> {
        let deny = |reason| -> SessionError { Denied::new(payload, reason).into() };
        let risk = safety::assess(&self.target, payload)?;
        if risk.tier != tier || tier == Tier::Read {
            return Err(deny(DenyReason::ApprovalMismatch));
        }
        if tier == Tier::ConfigWrite {
            let ident = self.config_read(safety::IDENTIFICATION_READ[1]);
            if ident.is_none() || ident != identification {
                return Err(deny(DenyReason::IdentificationMismatch));
            }
            let backup = safety::config_backup_read(&self.target, payload)
                .ok_or_else(|| deny(DenyReason::ApprovalMismatch))?;
            if self.config_read(backup[1]).is_none() {
                return Err(deny(DenyReason::PreReadRequired {
                    identifier: backup[1],
                }));
            }
        }
        Ok(risk)
    }

    /// Send the exact `payload` an [`Approval`] from this session's last challenge stands for.
    /// The approval is consumed and the challenge closed whatever the outcome.
    #[cfg(feature = "approved-routines")]
    pub fn request_approved(
        &mut self,
        payload: &[u8],
        approval: Approval,
        now_ms: u64,
    ) -> Result<Action, SessionError> {
        let pending = self.pending_approval.take();
        self.ready()?;
        let bound = self.nonce.is_some()
            && Some(approval.session) == self.nonce
            && Some(approval.request) == pending
            && approval.target == self.target
            && approval.payload == payload;
        if !bound {
            return Err(Denied::new(payload, DenyReason::ApprovalMismatch).into());
        }
        let risk = self.approvable(approval.tier, payload, approval.identification.as_deref())?;
        if risk != approval.risk {
            return Err(Denied::new(payload, DenyReason::ApprovalMismatch).into());
        }
        let permit = safety::Permit::new(payload, self.target, approval.tier);
        self.begin(Kind::Request, permit, now_ms)
    }

    /// Send TesterPresent (`36` ISO8, `3E` KWP). A missing reply is not an error.
    pub fn tester_present(&mut self, now_ms: u64) -> Result<Action, SessionError> {
        self.ready()?;
        let permit = safety::protocol_permit(&self.target, ProtocolFrame::TesterPresent);
        self.begin(Kind::TesterPresent, permit, now_ms)
    }

    /// Send Stop (`3F` ISO8, `82` KWP). A missing reply is not an error. Closes the session.
    pub fn stop(&mut self, now_ms: u64) -> Result<Action, SessionError> {
        self.ready()?;
        let permit = safety::protocol_permit(&self.target, ProtocolFrame::Stop);
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
        let read_lid = match (self.target.protocol, permit.payload()) {
            (Protocol::Kwp, [0x21, lid]) => Some(*lid),
            _ => None,
        };
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
        let mut exchange = Exchange::new(self.framing, permit, params).map_err(|e| match e {
            LinkError::Frame(f) => SessionError::Frame(f),
            e => SessionError::from(e),
        })?;
        let a = exchange.start();
        self.current = Some(Current {
            kind,
            exchange,
            read_lid,
        });
        Ok(a)
    }

    /// Feed the event answering the last action. `now_ms` is when it happened.
    pub fn on_event(&mut self, ev: Event, now_ms: u64) -> Step<SessionOutcome, SessionError> {
        let Some(mut cur) = self.current.take() else {
            return Step::Failed(SessionError::Link(LinkError::UnexpectedEvent));
        };
        let kind = cur.kind;
        let step = cur.exchange.on_event(ev);
        let t = self.t;
        let ok_gap = |r: &Option<Reply>| match r {
            Some(Reply::Negative { .. } | Reply::Iso8Status(_)) => t.on_error_ms,
            _ => t.reply_to_request_ms,
        };
        let result = match step {
            Step::Act(a) => {
                self.current = Some(cur);
                return Step::Act(a);
            }
            Step::Failed(e) => {
                if let Some(lid) = cur.read_lid {
                    self.config_reads.remove(&lid);
                }
                self.gap_ms = t.on_error_ms;
                Step::Failed(e.into())
            }
            Step::Done(r) => {
                self.gap_ms = ok_gap(&r);
                match (kind, r) {
                    (Kind::Request, Some(r)) => {
                        if let Some(lid) = cur.read_lid {
                            match &r {
                                Reply::Positive(p)
                                    if p.len() >= 2 && p[0] == 0x61 && p[1] == lid =>
                                {
                                    self.config_reads.insert(lid, p.clone());
                                }
                                _ => {
                                    self.config_reads.remove(&lid);
                                }
                            }
                        }
                        Step::Done(SessionOutcome::Reply(r))
                    }
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
