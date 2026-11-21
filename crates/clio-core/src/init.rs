//! Init state machines: 5-baud (ISO8 and KWP) and KWP fast init. See docs/protocol.md "Init".

use alloc::vec;

use crate::error::LinkError;
use crate::exchange::{Exchange, ExchangeParams};
use crate::frame::{Framing, KwpHeader, Reply};
use crate::io::{Action, Event, Step};
use crate::safety::{self, Protocol, Target};
use crate::seq::Seq;
use crate::timing::{consts, InitKind, KTiming, LinkConfig};

/// KWP StartCommunication service id.
pub const START_COMMUNICATION: u8 = 0x81;
/// Positive StartCommunication response id.
pub const START_COMMUNICATION_OK: u8 = 0xC1;

/// What an init found out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOutcome {
    pub kind: InitKind,
    /// Byte bit-banged at 5 baud (None for fast init).
    pub address_byte: Option<u8>,
    /// First byte after the 5-baud address; 0x55 expected, not enforced (None for fast init).
    pub sync: Option<u8>,
    pub kw1: u8,
    pub kw2: u8,
    /// The ~address byte after ~KW2, logged, not validated (None when there is no handshake).
    pub address_complement: Option<u8>,
    /// Some(true/false) when the config has expected key bytes.
    pub keywords_match: Option<bool>,
}

impl InitOutcome {
    pub fn keywords(&self) -> (u8, u8) {
        (self.kw1, self.kw2)
    }
}

fn keywords_match(cfg: &LinkConfig, kw: (u8, u8)) -> Result<Option<bool>, LinkError> {
    match cfg.expected_keywords {
        None => Ok(None),
        Some(expected) if expected == kw => Ok(Some(true)),
        Some(expected) if cfg.reject_keyword_mismatch => Err(LinkError::KeywordMismatch {
            expected,
            received: kw,
        }),
        Some(_) => Ok(Some(false)),
    }
}

#[derive(Debug, Clone)]
enum FbState {
    StartBit,
    Keywords,
    Complement {
        sync: u8,
        kw1: u8,
        kw2: u8,
        matched: Option<bool>,
    },
    Ready(InitOutcome),
    Cleanup(LinkError),
    Finished,
}

/// 5-baud init for `Iso8FiveBaud` (no handshake) and `KwpFiveBaud` (with ~KW2 handshake),
/// using [`LinkConfig::effective_timing`] and [`LinkConfig::five_baud_address_byte`].
///
/// Sequence: [idle] DTR low, break 200 ms (start bit), loopback check, 8 data bits LSB first
/// 200 ms each (0 = break, 1 = idle), release 100 ms (stop bit), DTR high, flush RX, read
/// `55 KW1 KW2` within KW2Timeout + 100 ms. KWP only: wait KW2 delay, send ~KW2, read
/// `[echo, ~address]`. Then wait FirstRequest.
#[derive(Debug, Clone)]
pub struct FiveBaudInit {
    cfg: LinkConfig,
    t: KTiming,
    address_byte: u8,
    handshake: bool,
    seq: Seq,
    state: FbState,
}

impl FiveBaudInit {
    pub fn new(cfg: &LinkConfig) -> Result<FiveBaudInit, LinkError> {
        let t = cfg.effective_timing();
        let handshake = match t.init {
            // RE: IOTBD32V3 worker: the ~KW2 handshake is skipped when the ISO8 flag is set.
            InitKind::Iso8FiveBaud => false,
            InitKind::KwpFiveBaud { .. } => true,
            InitKind::KwpFast { .. } => return Err(LinkError::WrongInitKind),
        };
        Ok(FiveBaudInit {
            cfg: cfg.clone(),
            t,
            address_byte: cfg.five_baud_address_byte(),
            handshake,
            seq: Seq::default(),
            state: FbState::StartBit,
        })
    }

    /// The byte that will be bit-banged.
    pub fn address_byte(&self) -> u8 {
        self.address_byte
    }

    pub fn start(&mut self) -> Action {
        self.seq.sleep(self.cfg.idle_before_init_ms);
        self.seq.push(Action::SetDtr(false));
        self.seq.push(Action::SetBreak(true));
        self.seq.sleep(consts::FIVE_BAUD_BIT_MS);
        self.seq.push(Action::CheckBreakLoopback);
        self.seq.emit().unwrap_or(Action::CheckBreakLoopback)
    }

    fn next(&mut self) -> Step<InitOutcome, LinkError> {
        match self.seq.emit() {
            Some(a) => Step::Act(a),
            None => {
                self.state = FbState::Finished;
                Step::Failed(LinkError::UnexpectedEvent)
            }
        }
    }

    fn fail(&mut self, e: LinkError) -> Step<InitOutcome, LinkError> {
        self.state = FbState::Finished;
        Step::Failed(e)
    }

    /// Queue the FirstRequest wait, or finish now if it is zero.
    fn ready(&mut self, outcome: InitOutcome) -> Step<InitOutcome, LinkError> {
        self.seq.sleep(self.t.first_request_ms);
        if self.seq.has_queued() {
            self.state = FbState::Ready(outcome);
            self.next()
        } else {
            self.state = FbState::Finished;
            Step::Done(outcome)
        }
    }

    pub fn on_event(&mut self, ev: Event) -> Step<InitOutcome, LinkError> {
        if matches!(self.state, FbState::Finished) {
            return Step::Failed(LinkError::UnexpectedEvent);
        }
        if let Err(e) = self.seq.accept(&ev) {
            return self.fail(e);
        }
        if self.seq.has_queued() {
            return self.next();
        }
        let window = self.t.kw2_read_window_ms();
        let state = core::mem::replace(&mut self.state, FbState::Finished);
        match (state, ev) {
            (FbState::StartBit, Event::BreakLoopbackSeen(false)) => {
                self.seq.push(Action::SetBreak(false));
                self.seq.push(Action::SetDtr(true));
                self.state = FbState::Cleanup(LinkError::KLineInactive);
                self.next()
            }
            (FbState::StartBit, Event::BreakLoopbackSeen(true)) => {
                for i in 0..8 {
                    let one = (self.address_byte >> i) & 1 == 1;
                    self.seq.push(Action::SetBreak(!one));
                    self.seq.sleep(consts::FIVE_BAUD_BIT_MS);
                }
                self.seq.push(Action::SetBreak(false));
                self.seq.sleep(consts::FIVE_BAUD_STOP_MS);
                self.seq.push(Action::SetDtr(true));
                // Drops the break residue and the echo of the address bits.
                self.seq.push(Action::FlushRx);
                self.seq.push(Action::Read {
                    count: 3,
                    timeout_ms: window,
                });
                self.state = FbState::Keywords;
                self.next()
            }
            (FbState::Cleanup(e), Event::Done) => self.fail(e),
            (FbState::Keywords, Event::ReadTimeout(received)) => {
                self.fail(LinkError::NoKeywords { received })
            }
            (FbState::Keywords, Event::ReadOk(b)) => {
                let (sync, kw1, kw2) = (b[0], b[1], b[2]);
                let matched = match keywords_match(&self.cfg, (kw1, kw2)) {
                    Ok(m) => m,
                    Err(e) => return self.fail(e),
                };
                if self.handshake {
                    self.seq.sleep(self.t.kw2_delay_ms);
                    self.seq.push(Action::Write {
                        bytes: vec![!kw2],
                        inter_byte_ms: 0,
                    });
                    self.seq.push(Action::Read {
                        count: 2,
                        timeout_ms: window,
                    });
                    self.state = FbState::Complement {
                        sync,
                        kw1,
                        kw2,
                        matched,
                    };
                    self.next()
                } else {
                    let outcome = self.outcome(sync, kw1, kw2, None, matched);
                    self.ready(outcome)
                }
            }
            (
                FbState::Complement {
                    sync,
                    kw1,
                    kw2,
                    matched,
                },
                ev @ (Event::ReadOk(_) | Event::ReadTimeout(_)),
            ) => {
                let (complete, rx) = match ev {
                    Event::ReadOk(b) => (true, b),
                    Event::ReadTimeout(b) => (false, b),
                    _ => unreachable!(),
                };
                if let Some(&echo) = rx.first() {
                    if echo != !kw2 {
                        return self.fail(LinkError::Collision {
                            sent: vec![!kw2],
                            echo: rx,
                        });
                    }
                }
                if !complete {
                    return self.fail(LinkError::NoAddressComplement { received: rx });
                }
                let outcome = self.outcome(sync, kw1, kw2, Some(rx[1]), matched);
                self.ready(outcome)
            }
            (FbState::Ready(outcome), Event::Done) => Step::Done(outcome),
            _ => self.fail(LinkError::UnexpectedEvent),
        }
    }

    fn outcome(
        &self,
        sync: u8,
        kw1: u8,
        kw2: u8,
        address_complement: Option<u8>,
        keywords_match: Option<bool>,
    ) -> InitOutcome {
        InitOutcome {
            kind: self.t.init,
            address_byte: Some(self.address_byte),
            sync: Some(sync),
            kw1,
            kw2,
            address_complement,
            keywords_match,
        }
    }
}

#[derive(Debug, Clone)]
enum FastState {
    Pattern,
    Exchange(Exchange),
    Finished,
}

/// KWP fast init: [idle] DTR high, 1 ms, break 25 ms, idle 25 ms, then StartCommunication
/// `81 tgt F1 81 cs` and its `C1 KW1 KW2` reply.
#[derive(Debug, Clone)]
pub struct FastInit {
    cfg: LinkConfig,
    t: KTiming,
    seq: Seq,
    state: FastState,
}

impl FastInit {
    pub fn new(cfg: &LinkConfig) -> Result<FastInit, LinkError> {
        let t = cfg.effective_timing();
        if !matches!(t.init, InitKind::KwpFast { .. }) {
            return Err(LinkError::WrongInitKind);
        }
        Ok(FastInit {
            cfg: cfg.clone(),
            t,
            seq: Seq::default(),
            state: FastState::Pattern,
        })
    }

    /// Framing of StartCommunication: KW1 is not known yet, so the length goes in the format
    /// byte (RE: KWP2000Diag4 LenInFmt default true).
    pub fn framing(&self) -> Framing {
        Framing::Kwp(KwpHeader {
            target: self.cfg.ecu_address,
            source: self.cfg.tool_address,
            length_in_format: true,
        })
    }

    pub fn start(&mut self) -> Action {
        self.seq.sleep(self.cfg.idle_before_init_ms);
        self.seq.push(Action::SetDtr(true));
        self.seq.sleep(consts::FAST_INIT_PRE_MS);
        self.seq.push(Action::SetBreak(true));
        self.seq.sleep(consts::FAST_INIT_LOW_MS);
        self.seq.push(Action::SetBreak(false));
        self.seq.sleep(consts::FAST_INIT_HIGH_MS);
        self.seq.emit().unwrap_or(Action::SetBreak(false))
    }

    fn fail(&mut self, e: LinkError) -> Step<InitOutcome, LinkError> {
        self.state = FastState::Finished;
        Step::Failed(e)
    }

    pub fn on_event(&mut self, ev: Event) -> Step<InitOutcome, LinkError> {
        match core::mem::replace(&mut self.state, FastState::Finished) {
            FastState::Finished => Step::Failed(LinkError::UnexpectedEvent),
            FastState::Pattern => {
                if let Err(e) = self.seq.accept(&ev) {
                    return self.fail(e);
                }
                if let Some(a) = self.seq.emit() {
                    self.state = FastState::Pattern;
                    return Step::Act(a);
                }
                let target = Target {
                    protocol: Protocol::Kwp,
                    ecu_address: self.cfg.ecu_address,
                };
                let permit = match safety::check(&target, &[START_COMMUNICATION]) {
                    Ok(p) => p,
                    Err(d) => return self.fail(d.into()),
                };
                let t = &self.t;
                let params = ExchangeParams {
                    gap_ms: 0,
                    inter_byte_ms: t.inter_byte_ms,
                    read_timeout_ms: t.request_to_reply_timeout_ms,
                    pending_timeout_ms: self.cfg.response_pending_timeout_ms,
                    max_pending: self.cfg.max_response_pending,
                    reply_optional: false,
                };
                let mut ex = match Exchange::new(self.framing(), permit, params) {
                    Ok(ex) => ex,
                    Err(e) => return self.fail(e),
                };
                let a = ex.start();
                self.state = FastState::Exchange(ex);
                Step::Act(a)
            }
            FastState::Exchange(mut ex) => match ex.on_event(ev) {
                Step::Act(a) => {
                    self.state = FastState::Exchange(ex);
                    Step::Act(a)
                }
                Step::Failed(e) => self.fail(e),
                Step::Done(Some(Reply::Positive(p)))
                    if p.len() >= 3 && p[0] == START_COMMUNICATION_OK =>
                {
                    let kw = (p[1], p[2]);
                    match keywords_match(&self.cfg, kw) {
                        Ok(m) => Step::Done(InitOutcome {
                            kind: self.t.init,
                            address_byte: None,
                            sync: None,
                            kw1: kw.0,
                            kw2: kw.1,
                            address_complement: None,
                            keywords_match: m,
                        }),
                        Err(e) => self.fail(e),
                    }
                }
                Step::Done(Some(r)) => self.fail(LinkError::UnexpectedStartReply(r)),
                Step::Done(None) => self.fail(LinkError::NoReply),
            },
        }
    }
}
