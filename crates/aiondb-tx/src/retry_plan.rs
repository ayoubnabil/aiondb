//! Conflict-driven retry plan generator.
//!
//! Produces a retry plan when a write conflict is detected :
//!
//! - `attempt` counter incremented on every retry.
//! - `backoff` duration follows exponential with jitter.
//! - `max_attempts` caps to prevent infinite spinning.
//!
//! Used by the SQL layer to decide whether to retry a conflicted
//! transaction or surface the conflict to the client.

use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub multiplier: f32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(50),
            max_backoff: Duration::from_secs(2),
            multiplier: 2.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryOutcome {
    Retry { attempt: u32, sleep_ms: u64 },
    GiveUp,
}

pub fn plan(policy: &RetryPolicy, attempt: u32) -> RetryOutcome {
    if attempt >= policy.max_attempts {
        return RetryOutcome::GiveUp;
    }
    let mut backoff = policy.initial_backoff.as_millis() as u64;
    for _ in 0..attempt {
        backoff = ((backoff as f32) * policy.multiplier).min(policy.max_backoff.as_millis() as f32)
            as u64;
    }
    // Jitter : ±10% deterministic per-attempt.
    let jitter = backoff.checked_div(10).unwrap_or(0);
    let sleep_ms = backoff.saturating_sub(jitter / 2);
    RetryOutcome::Retry {
        attempt: attempt + 1,
        sleep_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_attempt_returns_initial_backoff() {
        let p = RetryPolicy::default();
        match plan(&p, 0) {
            RetryOutcome::Retry { attempt, sleep_ms } => {
                assert_eq!(attempt, 1);
                assert!(sleep_ms > 0 && sleep_ms <= 50);
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn backoff_doubles_per_attempt() {
        let p = RetryPolicy::default();
        let a1 = match plan(&p, 0) {
            RetryOutcome::Retry { sleep_ms, .. } => sleep_ms,
            _ => panic!(),
        };
        let a2 = match plan(&p, 1) {
            RetryOutcome::Retry { sleep_ms, .. } => sleep_ms,
            _ => panic!(),
        };
        let a3 = match plan(&p, 2) {
            RetryOutcome::Retry { sleep_ms, .. } => sleep_ms,
            _ => panic!(),
        };
        assert!(a2 > a1);
        assert!(a3 > a2);
    }

    #[test]
    fn capped_at_max_backoff() {
        let p = RetryPolicy {
            max_attempts: 20,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(500),
            multiplier: 2.0,
        };
        match plan(&p, 15) {
            RetryOutcome::Retry { sleep_ms, .. } => {
                assert!(sleep_ms <= 500);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn give_up_after_max_attempts() {
        let p = RetryPolicy::default();
        assert_eq!(plan(&p, p.max_attempts), RetryOutcome::GiveUp);
        assert_eq!(plan(&p, 100), RetryOutcome::GiveUp);
    }

    #[test]
    fn zero_max_attempts_gives_up_immediately() {
        let p = RetryPolicy {
            max_attempts: 0,
            ..RetryPolicy::default()
        };
        assert_eq!(plan(&p, 0), RetryOutcome::GiveUp);
    }
}
