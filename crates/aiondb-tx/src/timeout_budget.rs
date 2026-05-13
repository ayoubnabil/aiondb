//! Hierarchical timeout budget.
//!
//! When a client query is dispatched to a coordinator, the
//! coordinator may fan out to N shards. Each downstream RPC must
//! finish before the original client deadline. The budget tracks
//! the remaining time as the request hops; a sub-RPC that exceeds
//! its slice is cancelled immediately rather than letting the
//! whole query run past the client's patience.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct TimeoutBudget {
    deadline: Instant,
}

impl TimeoutBudget {
    pub fn new(total: Duration) -> Self {
        Self {
            deadline: Instant::now() + total,
        }
    }

    pub fn from_deadline(deadline: Instant) -> Self {
        Self { deadline }
    }

    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    pub fn is_expired(&self) -> bool {
        self.remaining() == Duration::ZERO
    }

    /// Carve out a fraction of the remaining budget for a sub-call.
    /// `fraction` is clamped to [0.0, 1.0].
    pub fn sub_budget(&self, fraction: f64) -> Duration {
        let f = fraction.clamp(0.0, 1.0);
        let rem = self.remaining().as_nanos() as f64 * f;
        Duration::from_nanos(rem as u64)
    }

    /// Carve out N equal slices.
    pub fn split(&self, n: usize) -> Duration {
        if n == 0 {
            return self.remaining();
        }
        self.remaining() / n as u32
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_starts_close_to_total() {
        let b = TimeoutBudget::new(Duration::from_millis(100));
        let r = b.remaining();
        assert!(r > Duration::from_millis(90));
        assert!(r <= Duration::from_millis(100));
    }

    #[test]
    fn expired_returns_zero_remaining() {
        let b = TimeoutBudget::new(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(10));
        assert!(b.is_expired());
        assert_eq!(b.remaining(), Duration::ZERO);
    }

    #[test]
    fn sub_budget_carves_fraction() {
        let b = TimeoutBudget::new(Duration::from_secs(10));
        let half = b.sub_budget(0.5);
        assert!(half >= Duration::from_secs(4));
        assert!(half <= Duration::from_secs(5));
    }

    #[test]
    fn sub_budget_clamps_fraction() {
        let b = TimeoutBudget::new(Duration::from_secs(1));
        let full = b.sub_budget(5.0);
        assert!(full <= Duration::from_secs(1));
    }

    #[test]
    fn split_divides_evenly() {
        let b = TimeoutBudget::new(Duration::from_secs(10));
        let s = b.split(4);
        assert!(s >= Duration::from_secs(2));
        assert!(s <= Duration::from_secs(3));
    }

    #[test]
    fn split_n_zero_returns_full_remaining() {
        let b = TimeoutBudget::new(Duration::from_secs(5));
        let s = b.split(0);
        assert!(s >= Duration::from_secs(4));
    }

    #[test]
    fn from_deadline_preserves_remaining() {
        let dl = Instant::now() + Duration::from_secs(3);
        let b = TimeoutBudget::from_deadline(dl);
        assert_eq!(b.deadline(), dl);
    }
}
