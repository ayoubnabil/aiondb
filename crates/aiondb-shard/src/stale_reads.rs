//! Stale-read planning.
//!
//! Implements `AS OF SYSTEM TIME` semantics. Given a request for a
//! stale read at age `now - lag`, this module either:
//!
//! - Returns a follower read plan (timestamp + target replica) when
//!   the desired timestamp is at or below the per-shard closed
//!   frontier.
//! - Falls back to a leader read when the staleness budget is too
//!   tight or the closed frontier hasn't advanced yet.
//!
//! Used by the planner to decide where to route a read SELECT with
//! a stale-read modifier.

use std::time::Duration;

use crate::closed_timestamp::{target_closed_timestamp_now, ClosedTimestampTracker};
use crate::ShardId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StaleReadPlan {
    /// Read locally on any replica at `read_ts_us`. Caller picks the
    /// closest follower.
    FollowerRead { read_ts_us: u64 },
    /// Closed timestamp too old; route to the leaseholder for a
    /// strongly-consistent read.
    LeaseholderRead,
}

#[derive(Clone, Copy, Debug)]
pub struct StaleReadRequest {
    pub shard: ShardId,
    /// Maximum allowed staleness expressed as a wall-clock window.
    pub max_staleness: Duration,
    /// Optional explicit read timestamp (overrides `max_staleness`).
    pub explicit_read_ts_us: Option<u64>,
}

pub struct StaleReadPlanner<'a> {
    tracker: &'a ClosedTimestampTracker,
}

impl<'a> StaleReadPlanner<'a> {
    pub fn new(tracker: &'a ClosedTimestampTracker) -> Self {
        Self { tracker }
    }

    pub fn plan(&self, req: StaleReadRequest) -> StaleReadPlan {
        let closed = self.tracker.closed_ts(req.shard);
        let target = match req.explicit_read_ts_us {
            Some(t) => t,
            None => target_closed_timestamp_now(req.max_staleness),
        };
        if target <= closed && closed > 0 {
            StaleReadPlan::FollowerRead { read_ts_us: target }
        } else {
            StaleReadPlan::LeaseholderRead
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard(n: u32) -> ShardId {
        ShardId::new(n)
    }

    #[test]
    fn follower_read_when_target_within_closed_frontier() {
        let tracker = ClosedTimestampTracker::new();
        tracker.publish(shard(1), 10_000_000_000);
        let planner = StaleReadPlanner::new(&tracker);
        // Explicit timestamp well below closed frontier.
        let plan = planner.plan(StaleReadRequest {
            shard: shard(1),
            max_staleness: Duration::from_secs(1),
            explicit_read_ts_us: Some(1_000),
        });
        assert_eq!(plan, StaleReadPlan::FollowerRead { read_ts_us: 1_000 });
    }

    #[test]
    fn leaseholder_read_when_closed_frontier_unset() {
        let tracker = ClosedTimestampTracker::new();
        let planner = StaleReadPlanner::new(&tracker);
        let plan = planner.plan(StaleReadRequest {
            shard: shard(1),
            max_staleness: Duration::from_secs(1),
            explicit_read_ts_us: None,
        });
        assert_eq!(plan, StaleReadPlan::LeaseholderRead);
    }

    #[test]
    fn explicit_above_closed_falls_back_to_leader() {
        let tracker = ClosedTimestampTracker::new();
        tracker.publish(shard(1), 100);
        let planner = StaleReadPlanner::new(&tracker);
        let plan = planner.plan(StaleReadRequest {
            shard: shard(1),
            max_staleness: Duration::from_secs(1),
            explicit_read_ts_us: Some(1_000),
        });
        assert_eq!(plan, StaleReadPlan::LeaseholderRead);
    }
}
