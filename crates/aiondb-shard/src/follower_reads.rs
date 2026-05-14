//! Follower-read coordinator.
//!
//! Combines the [`ClosedTimestampTracker`] with a request gate so the
//! query layer can safely serve **stale reads from followers** without
//! consulting the leaseholder. The rules:
//!
//! - A read at `read_ts` is admissible on a follower if and only if
//!   `read_ts <= closed_ts(shard)`. Below the closed timestamp the
//!   leaseholder has promised never to commit any future write, so the
//!   follower's local copy at `read_ts` is identical to the
//!   leaseholder's.
//! - If `read_ts > closed_ts`, the read must either wait for the
//!   closed timestamp to advance (bounded by [`FollowerReadPolicy::wait_budget`])
//!   or get routed back to the leaseholder.
//!
//! This module also exposes a [`FollowerReadPolicy::pick_read_ts`]
//! helper that chooses a safe stale-read timestamp using the per-shard
//! closed frontier and a configured `max_staleness`.

use std::time::Duration;

use aiondb_core::{DbError, DbResult};
use tokio::time;
use tracing::trace;

use crate::closed_timestamp::ClosedTimestampTracker;
use crate::ShardId;

/// Default upper bound on stale-read wait. Bigger than typical
/// closed-timestamp lag so the policy almost always serves stale
/// reads without bouncing to the leaseholder.
pub const DEFAULT_WAIT_BUDGET: Duration = Duration::from_millis(200);

/// Default maximum staleness when picking a read timestamp.
pub const DEFAULT_MAX_STALENESS: Duration = Duration::from_secs(5);

/// Reason a follower read was rejected / deferred.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FollowerReadOutcome {
    /// Read may proceed on the follower.
    Admit,
    /// Closed timestamp has not advanced to `read_ts` and never will
    /// inside the configured wait budget; the caller must route to the
    /// leaseholder.
    RouteToLeaseholder { closed_ts_us: u64, target_us: u64 },
}

/// Policy parameters for follower reads.
#[derive(Clone, Debug)]
pub struct FollowerReadPolicy {
    /// Maximum time to wait for the closed timestamp to advance.
    pub wait_budget: Duration,
    /// Maximum staleness for [`FollowerReadPolicy::pick_read_ts`].
    pub max_staleness: Duration,
}

impl Default for FollowerReadPolicy {
    fn default() -> Self {
        Self {
            wait_budget: DEFAULT_WAIT_BUDGET,
            max_staleness: DEFAULT_MAX_STALENESS,
        }
    }
}

impl FollowerReadPolicy {
    /// Best wall-clock-derived read timestamp for `shard`. Returns
    /// `min(closed_ts, now - max_staleness)`, capped above by the
    /// shard's actual closed frontier. The result is always a value
    /// safe for follower reads.
    pub fn pick_read_ts(&self, tracker: &ClosedTimestampTracker, shard: ShardId) -> u64 {
        use crate::closed_timestamp::target_closed_timestamp_now;
        let aspirational = target_closed_timestamp_now(self.max_staleness);
        let closed = tracker.closed_ts(shard);
        aspirational.min(closed)
    }
}

/// Coordinator that gates follower reads against the closed timestamp.
#[derive(Clone, Debug)]
pub struct FollowerReadCoordinator {
    tracker: ClosedTimestampTracker,
    policy: FollowerReadPolicy,
}

impl FollowerReadCoordinator {
    pub fn new(tracker: ClosedTimestampTracker, policy: FollowerReadPolicy) -> Self {
        Self { tracker, policy }
    }

    pub fn policy(&self) -> &FollowerReadPolicy {
        &self.policy
    }

    /// Synchronous gate: returns `Admit` when the read timestamp is at
    /// or below the current closed frontier. Otherwise returns
    /// `RouteToLeaseholder` without waiting.
    pub fn try_admit(&self, shard: ShardId, read_ts_us: u64) -> FollowerReadOutcome {
        let closed = self.tracker.closed_ts(shard);
        if read_ts_us <= closed {
            FollowerReadOutcome::Admit
        } else {
            FollowerReadOutcome::RouteToLeaseholder {
                closed_ts_us: closed,
                target_us: read_ts_us,
            }
        }
    }

    /// Async gate that waits up to `policy.wait_budget` for the closed
    /// timestamp to advance to `read_ts`. Returns `Admit` on success
    /// or `RouteToLeaseholder` when the budget elapses.
    pub async fn admit(&self, shard: ShardId, read_ts_us: u64) -> DbResult<FollowerReadOutcome> {
        if let FollowerReadOutcome::Admit = self.try_admit(shard, read_ts_us) {
            return Ok(FollowerReadOutcome::Admit);
        }
        match time::timeout(
            self.policy.wait_budget,
            self.tracker
                .wait_for_at_least(shard, read_ts_us, self.policy.wait_budget),
        )
        .await
        {
            Ok(Ok(())) => Ok(FollowerReadOutcome::Admit),
            _ => {
                trace!(
                    shard = ?shard,
                    read_ts_us,
                    closed_ts_us = self.tracker.closed_ts(shard),
                    "follower read budget exhausted; routing to leaseholder"
                );
                Ok(FollowerReadOutcome::RouteToLeaseholder {
                    closed_ts_us: self.tracker.closed_ts(shard),
                    target_us: read_ts_us,
                })
            }
        }
    }

    /// Variant returning a `DbError` instead of a sentinel outcome.
    /// Useful for callers that want to convert "must route to leader"
    /// into a higher-level retry signal.
    pub async fn require_admit(&self, shard: ShardId, read_ts_us: u64) -> DbResult<()> {
        match self.admit(shard, read_ts_us).await? {
            FollowerReadOutcome::Admit => Ok(()),
            FollowerReadOutcome::RouteToLeaseholder {
                closed_ts_us,
                target_us,
            } => Err(DbError::internal(format!(
                "follower read at {target_us}us requires leaseholder; closed={closed_ts_us}us"
            ))),
        }
    }

    pub fn tracker(&self) -> &ClosedTimestampTracker {
        &self.tracker
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard(n: u32) -> ShardId {
        ShardId::new(n)
    }

    #[tokio::test]
    async fn read_at_or_below_closed_ts_admits_immediately() {
        let tracker = ClosedTimestampTracker::new();
        tracker.publish(shard(1), 100);
        let coord = FollowerReadCoordinator::new(tracker, FollowerReadPolicy::default());
        assert_eq!(coord.try_admit(shard(1), 99), FollowerReadOutcome::Admit);
        assert_eq!(coord.try_admit(shard(1), 100), FollowerReadOutcome::Admit);
    }

    #[tokio::test]
    async fn read_above_closed_ts_routes_to_leaseholder_sync() {
        let tracker = ClosedTimestampTracker::new();
        tracker.publish(shard(1), 100);
        let coord = FollowerReadCoordinator::new(tracker, FollowerReadPolicy::default());
        assert!(matches!(
            coord.try_admit(shard(1), 101),
            FollowerReadOutcome::RouteToLeaseholder {
                closed_ts_us: 100,
                target_us: 101
            }
        ));
    }

    #[tokio::test]
    async fn async_admit_waits_for_advance() {
        let tracker = ClosedTimestampTracker::new();
        tracker.publish(shard(1), 100);
        let t2 = tracker.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            t2.publish(shard(1), 200);
        });
        let coord = FollowerReadCoordinator::new(
            tracker,
            FollowerReadPolicy {
                wait_budget: Duration::from_millis(200),
                max_staleness: Duration::from_secs(1),
            },
        );
        let out = coord.admit(shard(1), 150).await.unwrap();
        assert_eq!(out, FollowerReadOutcome::Admit);
    }

    #[tokio::test]
    async fn async_admit_routes_after_budget_elapses() {
        let tracker = ClosedTimestampTracker::new();
        tracker.publish(shard(1), 50);
        let coord = FollowerReadCoordinator::new(
            tracker,
            FollowerReadPolicy {
                wait_budget: Duration::from_millis(30),
                max_staleness: Duration::from_secs(1),
            },
        );
        let out = coord.admit(shard(1), 100).await.unwrap();
        assert!(matches!(
            out,
            FollowerReadOutcome::RouteToLeaseholder { .. }
        ));
    }

    #[test]
    fn pick_read_ts_caps_at_closed_frontier() {
        let tracker = ClosedTimestampTracker::new();
        tracker.publish(shard(1), 1_000);
        let policy = FollowerReadPolicy {
            wait_budget: Duration::from_millis(50),
            max_staleness: Duration::from_secs(1000),
        };
        let chosen = policy.pick_read_ts(&tracker, shard(1));
        assert!(chosen <= 1_000, "must not exceed closed frontier: {chosen}");
    }

    #[tokio::test]
    async fn require_admit_errors_when_routing_required() {
        let tracker = ClosedTimestampTracker::new();
        tracker.publish(shard(1), 10);
        let coord = FollowerReadCoordinator::new(
            tracker,
            FollowerReadPolicy {
                wait_budget: Duration::from_millis(10),
                max_staleness: Duration::from_secs(1),
            },
        );
        let err = coord.require_admit(shard(1), 100).await.unwrap_err();
        assert!(err.to_string().contains("leaseholder"), "err: {err}");
    }
}
