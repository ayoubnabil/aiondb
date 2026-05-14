//! Per-shard circuit breaker.
//!
//! Three states :
//!
//! - **Closed**   : normal operation, errors counted in window.
//! - **Open**     : every call rejected for `cooldown`, no traffic.
//! - **HalfOpen** : a single probe is allowed; success → Closed,
//!   failure → Open again.
//!
//! Prevents an unhealthy shard from cascading failures into the
//! rest of the cluster.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Clone, Debug)]
pub struct CircuitBreaker {
    inner: Arc<std::sync::Mutex<BreakerInner>>,
    config: BreakerConfig,
}

#[derive(Clone, Copy, Debug)]
pub struct BreakerConfig {
    pub failure_threshold: u32,
    pub window: Duration,
    pub cooldown: Duration,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            window: Duration::from_secs(30),
            cooldown: Duration::from_secs(15),
        }
    }
}

#[derive(Debug)]
struct BreakerInner {
    state: BreakerState,
    failures: VecDeque<Instant>,
    opened_at: Option<Instant>,
    probes_in_flight: u32,
}

impl CircuitBreaker {
    pub fn new(config: BreakerConfig) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(BreakerInner {
                state: BreakerState::Closed,
                failures: VecDeque::new(),
                opened_at: None,
                probes_in_flight: 0,
            })),
            config,
        }
    }

    pub fn state(&self) -> BreakerState {
        let mut g = self.inner.lock().unwrap();
        self.transition_locked(&mut g);
        g.state
    }

    pub fn allow(&self) -> bool {
        let mut g = self.inner.lock().unwrap();
        self.transition_locked(&mut g);
        match g.state {
            BreakerState::Closed => true,
            BreakerState::Open => false,
            BreakerState::HalfOpen => {
                if g.probes_in_flight == 0 {
                    g.probes_in_flight += 1;
                    true
                } else {
                    false
                }
            }
        }
    }

    pub fn record_success(&self) {
        let mut g = self.inner.lock().unwrap();
        match g.state {
            BreakerState::HalfOpen => {
                g.state = BreakerState::Closed;
                g.failures.clear();
                g.opened_at = None;
                g.probes_in_flight = 0;
            }
            BreakerState::Closed => {
                g.failures.clear();
            }
            BreakerState::Open => {}
        }
    }

    pub fn record_failure(&self) {
        let mut g = self.inner.lock().unwrap();
        let now = Instant::now();
        g.failures.push_back(now);
        while let Some(&t) = g.failures.front() {
            if now.saturating_duration_since(t) > self.config.window {
                g.failures.pop_front();
            } else {
                break;
            }
        }
        match g.state {
            BreakerState::Closed => {
                if g.failures.len() as u32 >= self.config.failure_threshold {
                    g.state = BreakerState::Open;
                    g.opened_at = Some(now);
                    g.probes_in_flight = 0;
                }
            }
            BreakerState::HalfOpen => {
                g.state = BreakerState::Open;
                g.opened_at = Some(now);
                g.probes_in_flight = 0;
            }
            BreakerState::Open => {}
        }
    }

    fn transition_locked(&self, g: &mut BreakerInner) {
        if g.state == BreakerState::Open {
            if let Some(t) = g.opened_at {
                if Instant::now().saturating_duration_since(t) >= self.config.cooldown {
                    g.state = BreakerState::HalfOpen;
                    g.probes_in_flight = 0;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick_config() -> BreakerConfig {
        BreakerConfig {
            failure_threshold: 3,
            window: Duration::from_secs(60),
            cooldown: Duration::from_millis(20),
        }
    }

    #[test]
    fn starts_closed() {
        let b = CircuitBreaker::new(quick_config());
        assert_eq!(b.state(), BreakerState::Closed);
        assert!(b.allow());
    }

    #[test]
    fn opens_after_threshold_failures() {
        let b = CircuitBreaker::new(quick_config());
        for _ in 0..3 {
            b.record_failure();
        }
        assert_eq!(b.state(), BreakerState::Open);
        assert!(!b.allow());
    }

    #[test]
    fn transitions_to_half_open_after_cooldown() {
        let b = CircuitBreaker::new(quick_config());
        for _ in 0..3 {
            b.record_failure();
        }
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(b.state(), BreakerState::HalfOpen);
    }

    #[test]
    fn half_open_allows_one_probe() {
        let b = CircuitBreaker::new(quick_config());
        for _ in 0..3 {
            b.record_failure();
        }
        std::thread::sleep(Duration::from_millis(30));
        assert!(b.allow());
        assert!(!b.allow()); // second probe blocked
    }

    #[test]
    fn successful_probe_returns_to_closed() {
        let b = CircuitBreaker::new(quick_config());
        for _ in 0..3 {
            b.record_failure();
        }
        std::thread::sleep(Duration::from_millis(30));
        b.allow();
        b.record_success();
        assert_eq!(b.state(), BreakerState::Closed);
    }

    #[test]
    fn failed_probe_reopens() {
        let b = CircuitBreaker::new(quick_config());
        for _ in 0..3 {
            b.record_failure();
        }
        std::thread::sleep(Duration::from_millis(30));
        b.allow();
        b.record_failure();
        assert_eq!(b.state(), BreakerState::Open);
    }

    #[test]
    fn success_in_closed_clears_failure_count() {
        let b = CircuitBreaker::new(quick_config());
        b.record_failure();
        b.record_failure();
        b.record_success();
        b.record_failure();
        b.record_failure();
        assert_eq!(b.state(), BreakerState::Closed);
    }
}
