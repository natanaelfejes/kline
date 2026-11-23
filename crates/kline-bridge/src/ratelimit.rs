//! Per-client command counters and rate limits (PLAN-RESEARCH C3: "Command counter must
//! strictly increase per bond (stored in NVS), rejects stale writes; LOCK/UNLOCK rate
//! limited"; MASTER-PLAN-v2 3.1 rule 4).

use alloc::collections::{BTreeMap, VecDeque};

/// Identifies one bonded BLE peer. The firmware maps its bond table index to this.
pub type ClientId = u8;

/// The last accepted counter of every bonded client. The firmware saves [`Counters::get`] to
/// NVS after each accepted command and restores it with [`Counters::restore`] at boot, so a
/// replayed write is refused across resets too.
#[derive(Debug, Default, Clone)]
pub struct Counters {
    last: BTreeMap<ClientId, u32>,
}

impl Counters {
    pub fn new() -> Counters {
        Counters::default()
    }

    /// Accept `counter` from `client` if it is greater than the last accepted one.
    pub fn accept(&mut self, client: ClientId, counter: u32) -> bool {
        match self.last.get(&client) {
            Some(&last) if counter <= last => false,
            _ => {
                self.last.insert(client, counter);
                true
            }
        }
    }

    pub fn get(&self, client: ClientId) -> Option<u32> {
        self.last.get(&client).copied()
    }

    pub fn restore(&mut self, client: ClientId, last: u32) {
        self.last.insert(client, last);
    }

    /// Forget `client`'s counter (a new bond): its next counter starts afresh. A bond at
    /// `u32::MAX` can accept nothing more until it bonds again.
    pub fn forget(&mut self, client: ClientId) {
        self.last.remove(&client);
    }
}

/// At most `max` events per sliding `window_ms`, and at least `min_interval_ms` between two.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    max: usize,
    window_ms: u64,
    min_interval_ms: u64,
    times: VecDeque<u64>,
}

impl RateLimiter {
    pub fn new(max: usize, window_ms: u64, min_interval_ms: u64) -> RateLimiter {
        RateLimiter {
            max,
            window_ms,
            min_interval_ms,
            times: VecDeque::new(),
        }
    }

    /// Take one slot at `now_ms`, or refuse (nothing is recorded then).
    pub fn try_acquire(&mut self, now_ms: u64) -> bool {
        while self
            .times
            .front()
            .is_some_and(|&t| now_ms.saturating_sub(t) >= self.window_ms)
        {
            self.times.pop_front();
        }
        if self.times.len() >= self.max {
            return false;
        }
        if self
            .times
            .back()
            .is_some_and(|&t| now_ms.saturating_sub(t) < self.min_interval_ms)
        {
            return false;
        }
        self.times.push_back(now_ms);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_must_increase_per_client() {
        let mut c = Counters::new();
        assert!(c.accept(1, 5));
        assert!(!c.accept(1, 5));
        assert!(!c.accept(1, 4));
        assert!(c.accept(1, 6));
        assert!(c.accept(2, 1));
        c.restore(3, 100);
        assert!(!c.accept(3, 100));
        assert!(c.accept(4, u32::MAX));
        assert!(!c.accept(4, u32::MAX), "MAX: locked until a new bond");
        c.forget(4);
        assert!(c.accept(4, 1));
    }

    #[test]
    fn window_and_interval() {
        let mut r = RateLimiter::new(2, 1000, 100);
        assert!(r.try_acquire(0));
        assert!(!r.try_acquire(50));
        assert!(r.try_acquire(100));
        assert!(!r.try_acquire(500));
        assert!(r.try_acquire(1000));
    }
}
