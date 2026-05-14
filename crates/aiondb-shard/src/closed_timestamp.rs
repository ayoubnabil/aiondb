//! Closed-timestamp tracking for follower / stale reads.
//!
//! Inspired by CockroachDB's closed-timestamp mechanism. The leaseholder
//! of a shard periodically advertises a "closed" timestamp `T`: a wall
//! clock instant for which **no future write commit at any timestamp
//! `≤ T`** will ever be admitted. Followers can therefore serve consistent
//! reads at any timestamp `≤ T` without coordinating with the leader for
//! each read, unlocking low-latency stale reads and CDC resolved markers.
//!
//! ## Invariants
//!
//! 1. The closed timestamp is monotonic per shard. Calling
//!    [`ClosedTimestampTracker::publish`] with a value lower than the
//!    current frontier is a no-op (returns `false`).
//! 2. The closed timestamp is always strictly less than the leaseholder's
//!    current writable horizon. The caller is responsible for picking a
//!    safe target -- usually `now() - lag` for a configured `lag`.
//! 3. Followers subscribe via [`ClosedTimestampTracker::subscribe`] and
//!    receive each advance through a `tokio::sync::watch` channel.
//!
//! The tracker is a passive bookkeeping primitive: it does NOT enforce
//! that writes respect the closed timestamp. The transaction manager must
//! refuse to assign a commit timestamp below the published closed value.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aiondb_core::{DbError, DbResult};
use tokio::sync::watch;

use crate::ShardId;

/// Default lag the leaseholder leaves between "now" and the closed
/// frontier. Generous default keeps writes flowing without falling
/// behind closed-timestamp publication.
pub const DEFAULT_CLOSED_TIMESTAMP_LAG: Duration = Duration::from_secs(3);

/// Lifecycle of a single shard's closed-timestamp frontier.
#[derive(Debug)]
struct ShardFrontier {
    closed_ts_us: u64,
    tx: watch::Sender<u64>,
    rx: watch::Receiver<u64>,
}

impl ShardFrontier {
    fn new(initial: u64) -> Self {
        let (tx, rx) = watch::channel(initial);
        Self {
            closed_ts_us: initial,
            tx,
            rx,
        }
    }
}

/// Cluster-wide registry of closed-timestamp frontiers, keyed by
/// [`ShardId`]. Cheap to clone -- backed by a shared `Mutex` over a
/// `HashMap`.
#[derive(Clone, Debug, Default)]
pub struct ClosedTimestampTracker {
    inner: Arc<std::sync::Mutex<HashMap<ShardId, ShardFrontier>>>,
}

impl ClosedTimestampTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the current closed timestamp for a shard. Returns `0` when the
    /// shard has never published, signalling "no safe stale-read horizon
    /// yet". Use [`Self::wait_for_at_least`] when the caller wants to
    /// block until a target frontier is reached.
    pub fn closed_ts(&self, shard: ShardId) -> u64 {
        let guard = self.lock();
        guard
            .get(&shard)
            .map(|frontier| frontier.closed_ts_us)
            .unwrap_or(0)
    }

    /// Publish a new closed timestamp. Returns `true` when the frontier
    /// moved forward, `false` if the publish was a no-op (value already
    /// at or beyond the proposed target).
    pub fn publish(&self, shard: ShardId, closed_ts_us: u64) -> bool {
        let mut guard = self.lock();
        let frontier = guard.entry(shard).or_insert_with(|| ShardFrontier::new(0));
        if closed_ts_us <= frontier.closed_ts_us {
            return false;
        }
        frontier.closed_ts_us = closed_ts_us;
        let _ = frontier.tx.send(closed_ts_us);
        true
    }

    /// Subscribe to closed-timestamp advances for a shard. The receiver
    /// always observes the current frontier on first read.
    pub fn subscribe(&self, shard: ShardId) -> watch::Receiver<u64> {
        let mut guard = self.lock();
        let frontier = guard.entry(shard).or_insert_with(|| ShardFrontier::new(0));
        frontier.rx.clone()
    }

    /// Block (async) until the shard's closed timestamp is at least
    /// `target_us`. Returns `Ok(())` on success, or `Err` if the timeout
    /// elapses first.
    pub async fn wait_for_at_least(
        &self,
        shard: ShardId,
        target_us: u64,
        timeout: Duration,
    ) -> DbResult<()> {
        if self.closed_ts(shard) >= target_us {
            return Ok(());
        }
        let mut rx = self.subscribe(shard);
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if *rx.borrow() >= target_us {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(DbError::internal(format!(
                    "timed out waiting for closed timestamp ≥ {target_us} on shard {shard}"
                )));
            }
            match tokio::time::timeout(remaining, rx.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_closed)) => {
                    return Err(DbError::internal(format!(
                        "closed timestamp publisher dropped before reaching {target_us} on shard {shard}"
                    )));
                }
                Err(_elapsed) => {
                    return Err(DbError::internal(format!(
                        "timed out waiting for closed timestamp ≥ {target_us} on shard {shard}"
                    )));
                }
            }
        }
    }

    /// Number of shards currently tracked.
    pub fn shard_count(&self) -> usize {
        self.lock().len()
    }

    /// Snapshot every shard's current closed timestamp. Useful for
    /// `pg_stat_replication`-style introspection.
    pub fn snapshot(&self) -> Vec<(ShardId, u64)> {
        let guard = self.lock();
        let mut entries: Vec<_> = guard
            .iter()
            .map(|(shard, frontier)| (*shard, frontier.closed_ts_us))
            .collect();
        entries.sort_by_key(|(shard, _)| *shard);
        entries
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ShardId, ShardFrontier>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Compute a safe closed-timestamp target from wall-clock time and a lag
/// parameter. Returned value is in microseconds since the Unix epoch.
///
/// Centralised so leaseholder loops in the runtime all agree on the
/// frontier formula. Saturates instead of wrapping so a misconfigured
/// `lag` cannot underflow.
pub fn target_closed_timestamp_now(lag: Duration) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let lag_us = u64::try_from(lag.as_micros()).unwrap_or(u64::MAX);
    now_us.saturating_sub(lag_us)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard(n: u32) -> ShardId {
        ShardId::new(n)
    }

    #[test]
    fn fresh_tracker_reports_zero() {
        let t = ClosedTimestampTracker::new();
        assert_eq!(t.closed_ts(shard(1)), 0);
        assert_eq!(t.shard_count(), 0);
    }

    #[test]
    fn publish_advances_frontier_monotonically() {
        let t = ClosedTimestampTracker::new();
        assert!(t.publish(shard(1), 100));
        assert_eq!(t.closed_ts(shard(1)), 100);
        assert!(t.publish(shard(1), 200));
        assert_eq!(t.closed_ts(shard(1)), 200);
        // Lower values are rejected.
        assert!(!t.publish(shard(1), 150));
        assert_eq!(t.closed_ts(shard(1)), 200);
        // Equal values are no-ops.
        assert!(!t.publish(shard(1), 200));
        assert_eq!(t.closed_ts(shard(1)), 200);
    }

    #[test]
    fn shards_are_independent() {
        let t = ClosedTimestampTracker::new();
        t.publish(shard(1), 100);
        t.publish(shard(2), 50);
        assert_eq!(t.closed_ts(shard(1)), 100);
        assert_eq!(t.closed_ts(shard(2)), 50);
    }

    #[tokio::test]
    async fn subscribe_observes_initial_and_advances() {
        let t = ClosedTimestampTracker::new();
        t.publish(shard(7), 10);
        let mut rx = t.subscribe(shard(7));
        // Consume the current value so `changed()` only fires on the
        // *next* advance instead of returning immediately on the
        // already-pending notification for 10.
        assert_eq!(*rx.borrow_and_update(), 10);

        let t2 = t.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            t2.publish(shard(7), 42);
        });
        rx.changed().await.expect("subscriber observes advance");
        assert_eq!(*rx.borrow(), 42);
    }

    #[tokio::test]
    async fn wait_for_at_least_returns_when_published() {
        let t = ClosedTimestampTracker::new();
        let t2 = t.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            t2.publish(shard(9), 500);
        });
        t.wait_for_at_least(shard(9), 500, Duration::from_secs(1))
            .await
            .expect("publish should unblock waiter");
    }

    #[tokio::test]
    async fn wait_for_at_least_times_out() {
        let t = ClosedTimestampTracker::new();
        let err = t
            .wait_for_at_least(shard(11), 1_000, Duration::from_millis(40))
            .await
            .expect_err("timeout must propagate");
        assert!(err.to_string().contains("timed out"));
    }

    #[test]
    fn snapshot_returns_sorted_pairs() {
        let t = ClosedTimestampTracker::new();
        t.publish(shard(3), 30);
        t.publish(shard(1), 10);
        t.publish(shard(2), 20);
        let snap = t.snapshot();
        assert_eq!(snap, vec![(shard(1), 10), (shard(2), 20), (shard(3), 30)]);
    }

    #[test]
    fn target_closed_timestamp_now_subtracts_lag_safely() {
        // Lag well beyond now should saturate to zero, not wrap.
        let target = target_closed_timestamp_now(Duration::from_secs(60 * 60 * 24 * 365 * 1000));
        assert_eq!(target, 0);
        let small_lag = target_closed_timestamp_now(Duration::from_secs(1));
        assert!(small_lag > 0, "small lag must not wipe the timestamp");
    }
}
