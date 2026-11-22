//! One request/response exchange: gap, flush, write, echo, reply. Internal; every exchange is
//! built from a [`Permit`], so nothing reaches `Write` without passing the safety policy.

use alloc::vec::Vec;

use crate::echo::{strip_echo, EchoError};
use crate::error::LinkError;
use crate::frame::{Framing, Reply, ReplyNeed};
use crate::io::{Action, Event, Frame, Step};
use crate::safety::{Permit, Protocol};
use crate::seq::Seq;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ExchangeParams {
    /// Sleep before flushing and writing (P3 / OnError gap still owed).
    pub gap_ms: u32,
    pub inter_byte_ms: u32,
    /// Timeout of each read (RequestToReply).
    pub read_timeout_ms: u32,
    /// Timeout of the first-byte read after a 0x78 reply.
    pub pending_timeout_ms: u32,
    pub max_pending: u8,
    /// No reply at all is acceptable (TesterPresent, Stop).
    pub reply_optional: bool,
}

#[derive(Debug)]
enum State {
    Echo { residue_used: bool, rx: Vec<u8> },
    First,
    Body { frame: Vec<u8> },
    Header { frame: Vec<u8> },
    Finished,
}

/// Not `Clone`: a cloned exchange would write its frame twice (security review M2 finding 2).
#[derive(Debug)]
pub(crate) struct Exchange {
    framing: Framing,
    frame: Vec<u8>,
    p: ExchangeParams,
    pending: u8,
    seq: Seq,
    state: State,
}

/// `None`: no reply and `reply_optional`.
pub(crate) type ExchangeResult = Option<Reply>;

impl Exchange {
    pub(crate) fn new(
        framing: Framing,
        permit: Permit,
        p: ExchangeParams,
    ) -> Result<Exchange, LinkError> {
        let t = permit.target();
        let matches = match framing {
            Framing::Iso8 => t.protocol == Protocol::Iso8,
            Framing::Kwp(h) => t.protocol == Protocol::Kwp && h.target == t.ecu_address,
        };
        if !matches {
            return Err(LinkError::PermitMismatch);
        }
        let frame = framing.encode(permit.payload())?;
        Ok(Exchange {
            framing,
            frame,
            p,
            pending: 0,
            seq: Seq::default(),
            state: State::Echo {
                residue_used: false,
                rx: Vec::new(),
            },
        })
    }

    pub(crate) fn start(&mut self) -> Action {
        self.seq.sleep(self.p.gap_ms);
        // RE: IOTBD32V3 send routine: purge RX before every transmission.
        self.seq.push(Action::FlushRx);
        self.seq.push(Action::Write {
            frame: Frame::new(self.frame.clone()),
            inter_byte_ms: self.p.inter_byte_ms,
        });
        self.read(self.frame.len(), self.p.read_timeout_ms);
        self.seq.emit().unwrap_or(Action::FlushRx)
    }

    fn read(&mut self, count: usize, timeout_ms: u32) {
        self.seq.push(Action::Read { count, timeout_ms });
    }

    fn fail(&mut self, e: LinkError) -> Step<ExchangeResult, LinkError> {
        self.state = State::Finished;
        Step::Failed(e)
    }

    fn next(&mut self) -> Step<ExchangeResult, LinkError> {
        match self.seq.emit() {
            Some(a) => Step::Act(a),
            None => self.fail(LinkError::UnexpectedEvent),
        }
    }

    pub(crate) fn on_event(&mut self, ev: Event) -> Step<ExchangeResult, LinkError> {
        if matches!(self.state, State::Finished) {
            return Step::Failed(LinkError::UnexpectedEvent);
        }
        if let Err(e) = self.seq.accept(&ev) {
            return self.fail(e);
        }
        if self.seq.has_queued() {
            return self.next();
        }
        let state = core::mem::replace(&mut self.state, State::Finished);
        match (state, ev) {
            (
                State::Echo {
                    residue_used,
                    mut rx,
                },
                Event::ReadOk(b),
            ) => {
                rx.extend_from_slice(&b);
                match strip_echo(&self.frame, &rx) {
                    Ok(_) => self.expect_first(self.p.read_timeout_ms),
                    // One leading 0x00 break residue: read one more byte.
                    Err(EchoError::Incomplete) if !residue_used && rx.first() == Some(&0) => {
                        self.state = State::Echo {
                            residue_used: true,
                            rx,
                        };
                        self.read(1, self.p.read_timeout_ms);
                        self.next()
                    }
                    Err(_) => self.fail(LinkError::Collision {
                        sent: self.frame.clone(),
                        echo: rx,
                    }),
                }
            }
            (State::Echo { mut rx, .. }, Event::ReadTimeout(b)) => {
                rx.extend_from_slice(&b);
                if rx.is_empty() {
                    // RE: IOTBD32V3: zero echo bytes means the line is dead (0x3F0).
                    self.fail(LinkError::KLineInactive)
                } else {
                    self.fail(LinkError::Collision {
                        sent: self.frame.clone(),
                        echo: rx,
                    })
                }
            }
            (State::First, Event::ReadTimeout(_)) => {
                if self.p.reply_optional && self.pending == 0 {
                    self.state = State::Finished;
                    Step::Done(None)
                } else {
                    self.fail(LinkError::NoReply)
                }
            }
            (State::First, Event::ReadOk(b)) => match self.framing.need_after_first(b[0]) {
                Ok(ReplyNeed::Bytes(n)) => {
                    self.state = State::Body { frame: b };
                    self.read(n, self.p.read_timeout_ms);
                    self.next()
                }
                Ok(ReplyNeed::HeaderWithLength) => {
                    self.state = State::Header { frame: b };
                    self.read(3, self.p.read_timeout_ms);
                    self.next()
                }
                Err(e) => self.fail(e.into()),
            },
            (State::Header { mut frame }, Event::ReadOk(b)) => {
                frame.extend_from_slice(&b);
                match Framing::need_after_length(frame[3]) {
                    Ok(n) => {
                        self.state = State::Body { frame };
                        self.read(n, self.p.read_timeout_ms);
                        self.next()
                    }
                    Err(e) => self.fail(e.into()),
                }
            }
            (State::Body { mut frame } | State::Header { mut frame }, Event::ReadTimeout(b)) => {
                frame.extend_from_slice(&b);
                self.fail(LinkError::IncompleteReply { received: frame })
            }
            (State::Body { mut frame }, Event::ReadOk(b)) => {
                frame.extend_from_slice(&b);
                let reply = self
                    .framing
                    .decode(&frame)
                    .and_then(|p| Reply::classify(&self.framing, p));
                match reply {
                    Ok(Reply::ResponsePending { .. }) => {
                        self.pending = self.pending.saturating_add(1);
                        if self.pending > self.p.max_pending {
                            self.fail(LinkError::TooManyPending {
                                count: self.pending,
                            })
                        } else {
                            // Not in DDT2000: keep waiting for the real reply, no new request.
                            self.expect_first(self.p.pending_timeout_ms)
                        }
                    }
                    Ok(r) => {
                        self.state = State::Finished;
                        Step::Done(Some(r))
                    }
                    Err(e) => self.fail(e.into()),
                }
            }
            _ => self.fail(LinkError::UnexpectedEvent),
        }
    }

    fn expect_first(&mut self, timeout_ms: u32) -> Step<ExchangeResult, LinkError> {
        self.state = State::First;
        self.read(1, timeout_ms);
        self.next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::KwpHeader;
    use crate::safety::{Target, Tier};

    fn params() -> ExchangeParams {
        ExchangeParams {
            gap_ms: 0,
            inter_byte_ms: 0,
            read_timeout_ms: 300,
            pending_timeout_ms: 5000,
            max_pending: 10,
            reply_optional: false,
        }
    }

    #[test]
    fn permit_must_match_framing() {
        let kwp_7a = Framing::Kwp(KwpHeader {
            target: 0x7A,
            source: 0xF1,
            length_in_format: true,
        });
        let to = |protocol, ecu_address| Target {
            protocol,
            ecu_address,
        };
        let p = |t| Permit::new(&[0x21, 0xA0], t, Tier::Read);
        for (framing, target) in [
            (kwp_7a, to(Protocol::Kwp, 0x26)),
            (kwp_7a, to(Protocol::Iso8, 0x7A)),
            (Framing::Iso8, to(Protocol::Kwp, 0x7A)),
        ] {
            assert_eq!(
                Exchange::new(framing, p(target), params()).unwrap_err(),
                LinkError::PermitMismatch
            );
        }
        assert!(Exchange::new(kwp_7a, p(to(Protocol::Kwp, 0x7A)), params()).is_ok());
        assert!(Exchange::new(Framing::Iso8, p(to(Protocol::Iso8, 0x7A)), params()).is_ok());
    }
}
