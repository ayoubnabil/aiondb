//! Decorrelated-jitter back-off planner.
//!
//! Implements the AWS Architecture Blog "decorrelated jitter"
//! algorithm. Each attempt's sleep is sampled uniformly from
//! `[base, prev_sleep * 3]`, capped at `max`. This avoids the
//! synchronized "thundering herd" of plain exponential back-off and
//! the boundedness problems of full-jitter exponential.
//!
//! The planner is deterministic given a seed so tests can reproduce
//! retry traces.

use std::time::Duration;

#[derive(Clone, Debug)]
pub struct BackoffPlanner {
    base: Duration,
    cap: Duration,
    prev: Duration,
    seed: u64,
    attempt: u32,
}

impl BackoffPlanner {
    pub fn new(base: Duration, cap: Duration) -> Self {
        Self::with_seed(base, cap, 0xA5A5_5A5A)
    }

    pub fn with_seed(base: Duration, cap: Duration, seed: u64) -> Self {
        Self {
            base,
            cap,
            prev: base,
            seed,
            attempt: 0,
        }
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn next_delay(&mut self) -> Duration {
        self.attempt = self.attempt.saturating_add(1);
        let lo = self.base.as_nanos() as u64;
        let hi = (self.prev.as_nanos() as u64).saturating_mul(3);
        let hi = hi.max(lo + 1);
        let r = self.next_rand();
        let span = hi - lo;
        let pick = lo + (r % span);
        let capped = pick.min(self.cap.as_nanos() as u64);
        self.prev = Duration::from_nanos(capped);
        self.prev
    }

    pub fn reset(&mut self) {
        self.prev = self.base;
        self.attempt = 0;
    }

    fn next_rand(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.seed;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.seed = x.max(1);
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_delay_in_range() {
        let mut p = BackoffPlanner::new(Duration::from_millis(10), Duration::from_secs(10));
        let d = p.next_delay();
        assert!(d >= Duration::from_millis(10));
        assert!(d <= Duration::from_millis(30));
    }

    #[test]
    fn delay_capped() {
        let mut p = BackoffPlanner::new(Duration::from_millis(10), Duration::from_millis(50));
        for _ in 0..50 {
            let d = p.next_delay();
            assert!(d <= Duration::from_millis(50));
        }
    }

    #[test]
    fn attempt_counts_up() {
        let mut p = BackoffPlanner::new(Duration::from_millis(1), Duration::from_secs(1));
        for i in 1..=5 {
            p.next_delay();
            assert_eq!(p.attempt(), i);
        }
    }

    #[test]
    fn reset_clears_state() {
        let mut p = BackoffPlanner::new(Duration::from_millis(1), Duration::from_secs(1));
        for _ in 0..3 {
            p.next_delay();
        }
        p.reset();
        assert_eq!(p.attempt(), 0);
    }

    #[test]
    fn seeded_planners_are_deterministic() {
        let mut a = BackoffPlanner::with_seed(Duration::from_millis(1), Duration::from_secs(1), 1);
        let mut b = BackoffPlanner::with_seed(Duration::from_millis(1), Duration::from_secs(1), 1);
        for _ in 0..10 {
            assert_eq!(a.next_delay(), b.next_delay());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = BackoffPlanner::with_seed(Duration::from_millis(1), Duration::from_secs(1), 1);
        let mut b = BackoffPlanner::with_seed(Duration::from_millis(1), Duration::from_secs(1), 2);
        let mut differ = false;
        for _ in 0..10 {
            if a.next_delay() != b.next_delay() {
                differ = true;
            }
        }
        assert!(differ);
    }
}
