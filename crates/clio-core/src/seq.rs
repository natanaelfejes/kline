//! Internal action queue shared by the machines.

use alloc::collections::VecDeque;

use crate::error::LinkError;
use crate::io::{Action, Event};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Done,
    Loopback,
    Read(usize),
}

/// A queue of actions plus the kind of event the outstanding action expects.
///
/// Invariant: a `Read` or `CheckBreakLoopback` is always the last action of a batch, so the
/// machine reacts to every data-carrying event.
#[derive(Debug, Clone, Default)]
pub(crate) struct Seq {
    queue: VecDeque<Action>,
    outstanding: Option<Expect>,
}

impl Seq {
    pub(crate) fn push(&mut self, a: Action) {
        self.queue.push_back(a);
    }

    /// Queue a sleep unless it is zero.
    pub(crate) fn sleep(&mut self, ms: u32) {
        if ms > 0 {
            self.push(Action::Sleep { ms });
        }
    }

    pub(crate) fn has_queued(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Pop the next action and remember what it expects.
    pub(crate) fn emit(&mut self) -> Option<Action> {
        let a = self.queue.pop_front()?;
        self.outstanding = Some(match &a {
            Action::Read { count, .. } => Expect::Read(*count),
            Action::CheckBreakLoopback => Expect::Loopback,
            _ => Expect::Done,
        });
        Some(a)
    }

    /// Check that `ev` answers the outstanding action.
    pub(crate) fn accept(&mut self, ev: &Event) -> Result<(), LinkError> {
        let ok = match (self.outstanding.take(), ev) {
            (Some(Expect::Done), Event::Done) => true,
            (Some(Expect::Loopback), Event::BreakLoopbackSeen(_)) => true,
            (Some(Expect::Read(n)), Event::ReadOk(b)) => b.len() == n,
            (Some(Expect::Read(n)), Event::ReadTimeout(b)) => b.len() < n,
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            Err(LinkError::UnexpectedEvent)
        }
    }
}
