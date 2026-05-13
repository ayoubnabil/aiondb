//! Retry budget.
//!
//! Inspired by Twitter Finagle's retry budget: only `retry_ratio`
//! retries per success are allowed, with a small `min_per_second`
//! floor so a brand-new client can still retry a fresh failure. This
//! prevents retry storms during partial outages — if half the
//! requests fail, naive retries double the load on a system that's
//! already struggling.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct RetryBudget {
    inner: Arc<std::sync::Mutex<BudgetState>>,
    config: BudgetConfig,
}

#[derive(Clone, Copy, Debug)]
pub struct BudgetConfig {
    pub retry_ratio: f64,
    pub min_per_second: u32,
    pub window: Duration,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            retry_ratio: 0.2,
            min_per_second: 1,
            window: Duration::from_secs(10),
        }
    }
}

#[derive(Default, Debug)]
struct BudgetState {
    successes: VecDeque<Instant>,
    retries: VecDeque<Instant>,
}

impl RetryBudget {
    pub fn new(config: BudgetConfig) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(BudgetState::default())),
            config,
        }
    }

    pub fn record_success(&self) {
        let mut g = self.inner.lock().unwrap();
        let now = Instant::now();
        g.successes.push_back(now);
        self.prune_locked(&mut g, now);
    }

    pub fn try_retry(&self) -> bool {
        let mut g = self.inner.lock().unwrap();
        let now = Instant::now();
        self.prune_locked(&mut g, now);
        let allowed = self.allowed_retries_locked(&g);
        if (g.retries.len() as u32) < allowed {
            g.retries.push_back(now);
            true
        } else {
            false
        }
    }

    pub fn available(&self) -> u32 {
        let mut g = self.inner.lock().unwrap();
        let now = Instant::now();
        self.prune_locked(&mut g, now);
        let allowed = self.allowed_retries_locked(&g);
        allowed.saturating_sub(g.retries.len() as u32)
    }

    fn allowed_retries_locked(&self, g: &BudgetState) -> u32 {
        let by_ratio = (g.successes.len() as f64 * self.config.retry_ratio).floor() as u32;
        let by_floor = self.config.min_per_second * self.config.window.as_secs() as u32;
        by_ratio.max(by_floor)
    }

    fn prune_locked(&self, g: &mut BudgetState, now: Instant) {
        let window = self.config.window;
        while let Some(&t) = g.successes.front() {
            if now.saturating_duration_since(t) > window {
                g.successes.pop_front();
            } else {
                break;
            }
        }
        while let Some(&t) = g.retries.front() {
            if now.saturating_duration_since(t) > window {
                g.retries.pop_front();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_allows_initial_retries() {
        let b = RetryBudget::new(BudgetConfig {
            retry_ratio: 0.0,
            min_per_second: 1,
            window: Duration::from_secs(10),
        });
        // 1 * 10 = 10 retries allowed by floor.
        for _ in 0..10 {
            assert!(b.try_retry());
        }
        assert!(!b.try_retry());
    }

    #[test]
    fn ratio_scales_with_successes() {
        let b = RetryBudget::new(BudgetConfig {
            retry_ratio: 0.5,
            min_per_second: 0,
            window: Duration::from_secs(60),
        });
        for _ in 0..10 {
            b.record_success();
        }
        // 10 * 0.5 = 5 retries allowed.
        for _ in 0..5 {
            assert!(b.try_retry());
        }
        assert!(!b.try_retry());
    }

    #[test]
    fn available_drops_after_retries_used() {
        let b = RetryBudget::new(BudgetConfig {
            retry_ratio: 1.0,
            min_per_second: 0,
            window: Duration::from_secs(60),
        });
        for _ in 0..3 {
            b.record_success();
        }
        assert_eq!(b.available(), 3);
        b.try_retry();
        assert_eq!(b.available(), 2);
    }

    #[test]
    fn old_successes_are_pruned() {
        let b = RetryBudget::new(BudgetConfig {
            retry_ratio: 1.0,
            min_per_second: 0,
            window: Duration::from_millis(10),
        });
        b.record_success();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(b.available(), 0);
    }

    #[test]
    fn try_retry_false_when_exhausted() {
        let b = RetryBudget::new(BudgetConfig {
            retry_ratio: 0.0,
            min_per_second: 0,
            window: Duration::from_secs(10),
        });
        assert!(!b.try_retry());
    }
}
