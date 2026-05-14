//! Bounded staleness routing policy.
//!
//! Picks the read tier based on the caller's staleness budget :
//!
//! - **Strong** : read from leaseholder (always fresh).
//! - **Bounded** : read from any follower that has caught up within
//!   `staleness_budget` of wall clock.
//! - **AsOfSystemTime** : read at an explicit timestamp.
//!
//! Returns the recommended tier and the read timestamp so the SQL
//! planner can route accordingly.

use std::time::Duration;

use crate::closed_timestamp::{target_closed_timestamp_now, ClosedTimestampTracker};
use crate::ShardId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadTier {
    Leaseholder,
    Follower { read_ts_us: u64 },
    AsOf { read_ts_us: u64 },
}

#[derive(Clone, Copy, Debug)]
pub struct StalenessRequest {
    pub shard: ShardId,
    pub explicit_as_of: Option<u64>,
    pub staleness_budget: Option<Duration>,
}

pub fn pick_tier(tracker: &ClosedTimestampTracker, req: StalenessRequest) -> ReadTier {
    if let Some(ts) = req.explicit_as_of {
        return ReadTier::AsOf { read_ts_us: ts };
    }
    let Some(budget) = req.staleness_budget else {
        return ReadTier::Leaseholder;
    };
    let target = target_closed_timestamp_now(budget);
    let closed = tracker.closed_ts(req.shard);
    if target > 0 && target <= closed {
        ReadTier::Follower { read_ts_us: target }
    } else {
        ReadTier::Leaseholder
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard(n: u32) -> ShardId {
        ShardId::new(n)
    }

    #[test]
    fn explicit_as_of_always_wins() {
        let tracker = ClosedTimestampTracker::new();
        let plan = pick_tier(
            &tracker,
            StalenessRequest {
                shard: shard(1),
                explicit_as_of: Some(123),
                staleness_budget: Some(Duration::from_secs(1)),
            },
        );
        assert_eq!(plan, ReadTier::AsOf { read_ts_us: 123 });
    }

    #[test]
    fn no_budget_routes_to_leaseholder() {
        let tracker = ClosedTimestampTracker::new();
        let plan = pick_tier(
            &tracker,
            StalenessRequest {
                shard: shard(1),
                explicit_as_of: None,
                staleness_budget: None,
            },
        );
        assert_eq!(plan, ReadTier::Leaseholder);
    }

    #[test]
    fn budget_with_advanced_closed_picks_follower() {
        let tracker = ClosedTimestampTracker::new();
        tracker.publish(shard(1), u64::MAX / 2);
        let plan = pick_tier(
            &tracker,
            StalenessRequest {
                shard: shard(1),
                explicit_as_of: None,
                staleness_budget: Some(Duration::from_millis(1)),
            },
        );
        assert!(matches!(plan, ReadTier::Follower { .. }));
    }

    #[test]
    fn budget_with_stale_closed_falls_back_to_leaseholder() {
        let tracker = ClosedTimestampTracker::new();
        // Closed frontier not advanced.
        let plan = pick_tier(
            &tracker,
            StalenessRequest {
                shard: shard(1),
                explicit_as_of: None,
                staleness_budget: Some(Duration::from_millis(1)),
            },
        );
        assert_eq!(plan, ReadTier::Leaseholder);
    }
}
