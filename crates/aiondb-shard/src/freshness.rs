//! Stale-read freshness verifier.
//!
//! Given a read result's "as-of" timestamp + the client's declared
//! staleness budget, the verifier returns :
//!
//! - `Fresh` when the read meets the budget.
//! - `TooStale` when the read fell outside the budget; the caller
//!   should retry on the leaseholder.
//!
//! Used both server-side (to validate before returning) and
//! client-side (to invalidate cached results).

use std::time::{Duration, SystemTime};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FreshnessVerdict {
    Fresh,
    TooStale { age_us: u64, budget_us: u64 },
}

pub fn verify(read_ts_us: u64, staleness_budget: Duration) -> FreshnessVerdict {
    let now_us = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_micros()).ok())
        .unwrap_or(0);
    let age = now_us.saturating_sub(read_ts_us);
    let budget = u64::try_from(staleness_budget.as_micros()).unwrap_or(u64::MAX);
    if age <= budget {
        FreshnessVerdict::Fresh
    } else {
        FreshnessVerdict::TooStale {
            age_us: age,
            budget_us: budget,
        }
    }
}

/// Variant that takes an explicit `now` rather than reading the wall
/// clock. Useful for deterministic tests.
pub fn verify_at(read_ts_us: u64, now_us: u64, staleness_budget: Duration) -> FreshnessVerdict {
    let age = now_us.saturating_sub(read_ts_us);
    let budget = u64::try_from(staleness_budget.as_micros()).unwrap_or(u64::MAX);
    if age <= budget {
        FreshnessVerdict::Fresh
    } else {
        FreshnessVerdict::TooStale {
            age_us: age,
            budget_us: budget,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_when_age_within_budget() {
        assert_eq!(
            verify_at(1000, 1500, Duration::from_micros(600)),
            FreshnessVerdict::Fresh
        );
    }

    #[test]
    fn fresh_when_age_exactly_equals_budget() {
        assert_eq!(
            verify_at(1000, 1500, Duration::from_micros(500)),
            FreshnessVerdict::Fresh
        );
    }

    #[test]
    fn too_stale_when_age_exceeds_budget() {
        match verify_at(1000, 2000, Duration::from_micros(500)) {
            FreshnessVerdict::TooStale { age_us, budget_us } => {
                assert_eq!(age_us, 1000);
                assert_eq!(budget_us, 500);
            }
            other => panic!("expected TooStale, got {other:?}"),
        }
    }

    #[test]
    fn read_ts_in_future_treated_as_age_zero() {
        // saturating_sub clamps to zero.
        assert_eq!(
            verify_at(1500, 1000, Duration::from_micros(0)),
            FreshnessVerdict::Fresh
        );
    }

    #[test]
    fn live_verifier_does_not_panic() {
        // Best-effort smoke test : the live verifier should not panic
        // and should return Fresh for a brand-new read_ts.
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();
        let read_ts_us = u64::try_from(now.as_micros()).unwrap();
        let verdict = verify(read_ts_us, Duration::from_secs(1));
        assert!(matches!(verdict, FreshnessVerdict::Fresh));
    }
}
