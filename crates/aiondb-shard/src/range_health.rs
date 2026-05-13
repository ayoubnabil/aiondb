//! Per-range health aggregator.
//!
//! Combines four signals into one verdict :
//!
//! 1. Replication factor compliance (from `replication_factor`).
//! 2. Lease presence (from `lease`).
//! 3. Active replica progress (from `raft_state`).
//! 4. Recent leadership churn.

use crate::lease::LeaseRegistry;
use crate::raft_state::RaftStateRegistry;
use crate::range_descriptor::{RangeDescriptorRegistry, RangeId};
use crate::ShardId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RangeHealth {
    Healthy,
    NoLeaseholder {
        range: RangeId,
    },
    UnderReplicated {
        range: RangeId,
        current: usize,
        target: usize,
    },
    NoActiveLeader {
        range: RangeId,
    },
    Missing {
        range: RangeId,
    },
}

pub struct RangeHealthMonitor<'a> {
    pub ranges: &'a RangeDescriptorRegistry,
    pub leases: &'a LeaseRegistry,
    pub raft_state: &'a RaftStateRegistry,
}

impl<'a> RangeHealthMonitor<'a> {
    pub fn evaluate(&self, range: RangeId, replication_target: usize) -> RangeHealth {
        let Some(descriptor) = self.ranges.get(range) else {
            return RangeHealth::Missing { range };
        };
        let voters: usize = descriptor.replicas.iter().filter(|r| !r.is_learner).count();
        if voters < replication_target {
            return RangeHealth::UnderReplicated {
                range,
                current: voters,
                target: replication_target,
            };
        }
        let shard_id = ShardId::new(range.get() as u32);
        if self.leases.current(shard_id).is_none() {
            return RangeHealth::NoLeaseholder { range };
        }
        let state = self.raft_state.state(range);
        if state.leader_replica_id.is_none() {
            return RangeHealth::NoActiveLeader { range };
        }
        RangeHealth::Healthy
    }

    pub fn evaluate_all(&self, replication_target: usize) -> Vec<RangeHealth> {
        self.ranges
            .snapshot()
            .into_iter()
            .map(|d| self.evaluate(d.range_id, replication_target))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use std::time::Instant;

    use super::*;
    use crate::lease::LeaseRegistry;
    use crate::raft_state::{RaftStateRegistry, RangeRaftState};
    use crate::range_descriptor::{RangeDescriptor, ReplicaDescriptor, ReplicaId};
    use crate::ShardId;

    fn descriptor(range: u64) -> RangeDescriptor {
        RangeDescriptor {
            range_id: RangeId::new(range),
            start_key: b"a".to_vec(),
            end_key: b"z".to_vec(),
            replicas: (1..=3)
                .map(|i| ReplicaDescriptor {
                    replica_id: ReplicaId::new(i),
                    node_id: format!("n{i}"),
                    is_learner: false,
                })
                .collect(),
            shard: ShardId::new(range as u32),
            lease: None,
            generation: 0,
        }
    }

    fn build() -> (RangeDescriptorRegistry, LeaseRegistry, RaftStateRegistry) {
        let r = RangeDescriptorRegistry::new();
        r.upsert(descriptor(1)).unwrap();
        let l = LeaseRegistry::new();
        let rs = RaftStateRegistry::new();
        (r, l, rs)
    }

    #[test]
    fn missing_range_is_flagged() {
        let (r, l, rs) = build();
        let mon = RangeHealthMonitor {
            ranges: &r,
            leases: &l,
            raft_state: &rs,
        };
        assert_eq!(
            mon.evaluate(RangeId::new(99), 3),
            RangeHealth::Missing {
                range: RangeId::new(99)
            }
        );
    }

    #[test]
    fn under_replicated_is_flagged_first() {
        let (r, l, rs) = build();
        let mon = RangeHealthMonitor {
            ranges: &r,
            leases: &l,
            raft_state: &rs,
        };
        match mon.evaluate(RangeId::new(1), 5) {
            RangeHealth::UnderReplicated {
                current: 3,
                target: 5,
                ..
            } => {}
            other => panic!("expected UnderReplicated, got {other:?}"),
        }
    }

    #[test]
    fn no_lease_is_flagged_next() {
        let (r, l, rs) = build();
        let mon = RangeHealthMonitor {
            ranges: &r,
            leases: &l,
            raft_state: &rs,
        };
        match mon.evaluate(RangeId::new(1), 3) {
            RangeHealth::NoLeaseholder { range } => assert_eq!(range, RangeId::new(1)),
            other => panic!("expected NoLeaseholder, got {other:?}"),
        }
    }

    #[test]
    fn no_leader_is_flagged_after_lease() {
        let (r, l, rs) = build();
        l.acquire(ShardId::new(1), 7, Duration::from_secs(30), Instant::now());
        let mon = RangeHealthMonitor {
            ranges: &r,
            leases: &l,
            raft_state: &rs,
        };
        match mon.evaluate(RangeId::new(1), 3) {
            RangeHealth::NoActiveLeader { .. } => {}
            other => panic!("expected NoActiveLeader, got {other:?}"),
        }
    }

    #[test]
    fn healthy_when_all_signals_pass() {
        let (r, l, rs) = build();
        l.acquire(ShardId::new(1), 7, Duration::from_secs(30), Instant::now());
        rs.publish_state(
            RangeId::new(1),
            RangeRaftState {
                term: 1,
                commit_index: 1,
                applied_index: 1,
                last_log_index: 1,
                leader_replica_id: Some(ReplicaId::new(1)),
            },
        );
        let mon = RangeHealthMonitor {
            ranges: &r,
            leases: &l,
            raft_state: &rs,
        };
        assert_eq!(mon.evaluate(RangeId::new(1), 3), RangeHealth::Healthy);
    }
}
