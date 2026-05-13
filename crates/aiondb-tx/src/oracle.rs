use std::sync::atomic::{AtomicU64, Ordering};

use aiondb_core::{DbError, DbResult};

#[derive(Debug, Default)]
pub struct CommitTimestampOracle {
    next_commit_ts: AtomicU64,
}

impl CommitTimestampOracle {
    pub fn try_next(&self) -> DbResult<u64> {
        let previous = self
            .next_commit_ts
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current.checked_add(1)
            })
            .map_err(|_| DbError::internal("commit timestamp space exhausted"))?;
        previous
            .checked_add(1)
            .ok_or_else(|| DbError::internal("commit timestamp space exhausted"))
    }

    pub fn next(&self) -> u64 {
        self.try_next().unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- next() starts at 1 ---
    #[test]
    fn next_starts_at_1() {
        let oracle = CommitTimestampOracle::default();
        assert_eq!(oracle.next(), 1);
    }

    // --- next() increments: 1, 2, 3, ... ---
    #[test]
    fn next_increments_sequentially() {
        let oracle = CommitTimestampOracle::default();
        assert_eq!(oracle.next(), 1);
        assert_eq!(oracle.next(), 2);
        assert_eq!(oracle.next(), 3);
    }

    // --- Sequential calls produce strictly increasing values ---
    #[test]
    fn sequential_calls_strictly_increasing() {
        let oracle = CommitTimestampOracle::default();
        let mut prev = oracle.next();
        for _ in 0..100 {
            let curr = oracle.next();
            assert!(curr > prev, "Expected {curr} > {prev}");
            prev = curr;
        }
    }

    // --- Two independent oracles produce independent sequences ---
    #[test]
    fn independent_oracles_have_independent_sequences() {
        let oracle_a = CommitTimestampOracle::default();
        let oracle_b = CommitTimestampOracle::default();
        assert_eq!(oracle_a.next(), 1);
        assert_eq!(oracle_a.next(), 2);
        // oracle_b is independent, starts at 1
        assert_eq!(oracle_b.next(), 1);
    }

    // --- next() never returns zero ---
    #[test]
    fn next_never_returns_zero() {
        let oracle = CommitTimestampOracle::default();
        for _ in 0..50 {
            assert_ne!(oracle.next(), 0);
        }
    }

    #[test]
    fn try_next_rejects_commit_timestamp_overflow() {
        let oracle = CommitTimestampOracle::default();
        oracle.next_commit_ts.store(u64::MAX, Ordering::SeqCst);

        let err = oracle
            .try_next()
            .expect_err("commit timestamp overflow must fail");
        assert!(err.to_string().contains("commit timestamp space exhausted"));
        assert_eq!(oracle.next_commit_ts.load(Ordering::SeqCst), u64::MAX);
    }

    #[test]
    fn next_saturates_after_commit_timestamp_exhaustion() {
        let oracle = CommitTimestampOracle::default();
        oracle.next_commit_ts.store(u64::MAX, Ordering::SeqCst);

        assert_eq!(oracle.next(), u64::MAX);
        assert_eq!(oracle.next_commit_ts.load(Ordering::SeqCst), u64::MAX);
    }

    // --- Consecutive differences are always exactly 1 ---
    #[test]
    fn consecutive_difference_is_one() {
        let oracle = CommitTimestampOracle::default();
        let mut prev = oracle.next();
        for _ in 0..20 {
            let curr = oracle.next();
            assert_eq!(curr - prev, 1);
            prev = curr;
        }
    }

    // --- Default creates oracle with next returning 1 (verifying default state) ---
    #[test]
    fn default_initial_state() {
        let oracle = CommitTimestampOracle::default();
        // The very first call should be 1
        assert_eq!(oracle.next(), 1);
    }

    // --- Oracle implements Debug ---
    #[test]
    fn oracle_implements_debug() {
        let oracle = CommitTimestampOracle::default();
        let dbg = format!("{oracle:?}");
        assert!(dbg.contains("CommitTimestampOracle"));
    }

    // --- After many calls, value matches expected count ---
    #[test]
    fn value_after_n_calls_equals_n() {
        let oracle = CommitTimestampOracle::default();
        let n = 500;
        let mut last = 0;
        for _ in 0..n {
            last = oracle.next();
        }
        assert_eq!(last, n);
    }
}
