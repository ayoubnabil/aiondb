//! Replica/node join rate limiter.
//!
//! Caps the number of concurrent joins and the per-second join rate
//! to avoid a thundering-herd that would overload an unstable cluster.
//! Uses an in-memory leaky bucket plus a small "in-flight" counter.
//!
//! Callers acquire a permit before snapshotting a new replica; they
//! drop the permit when the snapshot finishes. The limiter never
//! blocks the runtime — calls return [`JoinDecision`] immediately.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinDecision {
    Accepted,
    RateExceeded,
    TooManyInFlight,
}

#[derive(Clone, Debug)]
pub struct JoinPermit {
    inner: Arc<JoinRateLimiterInner>,
}

impl Drop for JoinPermit {
    fn drop(&mut self) {
        let mut g = self.inner.state.lock().unwrap();
        if g.in_flight > 0 {
            g.in_flight -= 1;
        }
    }
}

#[derive(Debug)]
struct State {
    in_flight: u32,
    window: VecDeque<Instant>,
}

#[derive(Debug)]
struct JoinRateLimiterInner {
    state: std::sync::Mutex<State>,
    max_in_flight: u32,
    max_per_second: u32,
    window: Duration,
}

#[derive(Clone, Debug)]
pub struct JoinRateLimiter {
    inner: Arc<JoinRateLimiterInner>,
}

impl JoinRateLimiter {
    pub fn new(max_in_flight: u32, max_per_second: u32) -> Self {
        Self {
            inner: Arc::new(JoinRateLimiterInner {
                state: std::sync::Mutex::new(State {
                    in_flight: 0,
                    window: VecDeque::new(),
                }),
                max_in_flight,
                max_per_second,
                window: Duration::from_secs(1),
            }),
        }
    }

    pub fn try_acquire(&self) -> Result<JoinPermit, JoinDecision> {
        let mut g = self.inner.state.lock().unwrap();
        let now = Instant::now();
        while let Some(&t) = g.window.front() {
            if now.saturating_duration_since(t) > self.inner.window {
                g.window.pop_front();
            } else {
                break;
            }
        }
        if g.window.len() as u32 >= self.inner.max_per_second {
            return Err(JoinDecision::RateExceeded);
        }
        if g.in_flight >= self.inner.max_in_flight {
            return Err(JoinDecision::TooManyInFlight);
        }
        g.in_flight += 1;
        g.window.push_back(now);
        Ok(JoinPermit {
            inner: self.inner.clone(),
        })
    }

    pub fn in_flight(&self) -> u32 {
        self.inner.state.lock().unwrap().in_flight
    }

    pub fn observed_rate(&self) -> u32 {
        let mut g = self.inner.state.lock().unwrap();
        let now = Instant::now();
        while let Some(&t) = g.window.front() {
            if now.saturating_duration_since(t) > self.inner.window {
                g.window.pop_front();
            } else {
                break;
            }
        }
        g.window.len() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_under_limits() {
        let l = JoinRateLimiter::new(4, 100);
        let p = l.try_acquire().unwrap();
        assert_eq!(l.in_flight(), 1);
        drop(p);
        assert_eq!(l.in_flight(), 0);
    }

    #[test]
    fn rejects_when_in_flight_full() {
        let l = JoinRateLimiter::new(2, 100);
        let _p1 = l.try_acquire().unwrap();
        let _p2 = l.try_acquire().unwrap();
        assert_eq!(l.try_acquire().unwrap_err(), JoinDecision::TooManyInFlight);
    }

    #[test]
    fn rejects_when_rate_exceeded() {
        let l = JoinRateLimiter::new(10, 2);
        let _p1 = l.try_acquire().unwrap();
        let _p2 = l.try_acquire().unwrap();
        assert_eq!(l.try_acquire().unwrap_err(), JoinDecision::RateExceeded);
    }

    #[test]
    fn permit_drop_frees_slot() {
        let l = JoinRateLimiter::new(1, 100);
        {
            let _p = l.try_acquire().unwrap();
            assert!(l.try_acquire().is_err());
        }
        assert!(l.try_acquire().is_ok());
    }

    #[test]
    fn observed_rate_tracks_window() {
        let l = JoinRateLimiter::new(10, 10);
        let _p = l.try_acquire().unwrap();
        let _p2 = l.try_acquire().unwrap();
        assert_eq!(l.observed_rate(), 2);
    }
}
