//! Replication factor enforcement.
//!
//! Scans the [`RangeDescriptorRegistry`] and reports under-replicated
//! ranges so an operator / allocator can add replicas before quorum
//! is lost.

use crate::range_descriptor::{RangeDescriptor, RangeDescriptorRegistry, RangeId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplicationVerdict {
    Healthy,
    UnderReplicated { current: usize, target: usize },
    Empty,
}

pub fn classify(descriptor: &RangeDescriptor, target: usize) -> ReplicationVerdict {
    let live = descriptor.replicas.iter().filter(|r| !r.is_learner).count();
    if live == 0 {
        return ReplicationVerdict::Empty;
    }
    if live < target {
        return ReplicationVerdict::UnderReplicated {
            current: live,
            target,
        };
    }
    ReplicationVerdict::Healthy
}

pub fn under_replicated_ranges(
    registry: &RangeDescriptorRegistry,
    target: usize,
) -> Vec<(RangeId, ReplicationVerdict)> {
    registry
        .snapshot()
        .into_iter()
        .filter_map(|d| {
            let verdict = classify(&d, target);
            if matches!(verdict, ReplicationVerdict::Healthy) {
                None
            } else {
                Some((d.range_id, verdict))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range_descriptor::{RangeDescriptor, ReplicaDescriptor, ReplicaId};
    use crate::ShardId;

    fn descriptor(range: u64, replicas: usize, learners: usize) -> RangeDescriptor {
        let mut reps = Vec::new();
        for i in 0..replicas {
            reps.push(ReplicaDescriptor {
                replica_id: ReplicaId::new(i as u64),
                node_id: format!("n{i}"),
                is_learner: false,
            });
        }
        for i in 0..learners {
            reps.push(ReplicaDescriptor {
                replica_id: ReplicaId::new((replicas + i) as u64),
                node_id: format!("l{i}"),
                is_learner: true,
            });
        }
        RangeDescriptor {
            range_id: RangeId::new(range),
            start_key: format!("k{range}").into_bytes(),
            end_key: format!("k{}", range + 1).into_bytes(),
            replicas: reps,
            shard: ShardId::new(range as u32),
            lease: None,
            generation: 0,
        }
    }

    #[test]
    fn enough_replicas_is_healthy() {
        let d = descriptor(1, 3, 0);
        assert_eq!(classify(&d, 3), ReplicationVerdict::Healthy);
    }

    #[test]
    fn under_replicated_reports_current_and_target() {
        let d = descriptor(1, 2, 0);
        match classify(&d, 3) {
            ReplicationVerdict::UnderReplicated { current, target } => {
                assert_eq!(current, 2);
                assert_eq!(target, 3);
            }
            other => panic!("expected UnderReplicated, got {other:?}"),
        }
    }

    #[test]
    fn learners_dont_count_toward_target() {
        let d = descriptor(1, 2, 2);
        let verdict = classify(&d, 3);
        assert!(matches!(
            verdict,
            ReplicationVerdict::UnderReplicated {
                current: 2,
                target: 3
            }
        ));
    }

    #[test]
    fn no_voters_is_empty() {
        let d = descriptor(1, 0, 2);
        assert_eq!(classify(&d, 3), ReplicationVerdict::Empty);
    }

    #[test]
    fn under_replicated_ranges_filters_healthy() {
        let registry = RangeDescriptorRegistry::new();
        registry.upsert(descriptor(1, 3, 0)).unwrap();
        registry.upsert(descriptor(2, 1, 0)).unwrap();
        let out = under_replicated_ranges(&registry, 3);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, RangeId::new(2));
    }
}
