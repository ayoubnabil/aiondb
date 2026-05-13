//! Range transfer plan : lease vs replica.
//!
//! Used by the rebalance executor to decide whether to do a cheap
//! lease-only transfer (a single Raft proposal) or a costly replica
//! migration (add learner, snapshot, promote, remove).
//!
//! Heuristic :
//!
//! - If `target_node` already has a replica → **LeaseTransfer**.
//! - Otherwise → **ReplicaMigration**.
//!
//! Used by [`crate::rebalance_executor`] to construct correct plans.

use crate::range_descriptor::{RangeDescriptor, RangeId, ReplicaDescriptor};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransferKind {
    LeaseTransfer {
        range: RangeId,
        from_node: String,
        to_node: String,
    },
    ReplicaMigration {
        range: RangeId,
        from_node: String,
        to_node: String,
        add_replica: ReplicaDescriptor,
        remove_replica: ReplicaDescriptor,
    },
}

pub fn plan_transfer(
    descriptor: &RangeDescriptor,
    from_node: &str,
    to_node: &str,
    next_replica_id: u64,
) -> Option<TransferKind> {
    let from_replica = descriptor
        .replicas
        .iter()
        .find(|r| r.node_id == from_node)?
        .clone();
    let target_already_replica = descriptor.replicas.iter().any(|r| r.node_id == to_node);
    if target_already_replica {
        Some(TransferKind::LeaseTransfer {
            range: descriptor.range_id,
            from_node: from_node.to_owned(),
            to_node: to_node.to_owned(),
        })
    } else {
        let new_replica = ReplicaDescriptor {
            replica_id: crate::range_descriptor::ReplicaId::new(next_replica_id),
            node_id: to_node.to_owned(),
            is_learner: false,
        };
        Some(TransferKind::ReplicaMigration {
            range: descriptor.range_id,
            from_node: from_node.to_owned(),
            to_node: to_node.to_owned(),
            add_replica: new_replica,
            remove_replica: from_replica,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range_descriptor::{RangeDescriptor, ReplicaId};
    use crate::ShardId;

    fn descriptor() -> RangeDescriptor {
        RangeDescriptor {
            range_id: RangeId::new(1),
            start_key: b"a".to_vec(),
            end_key: b"z".to_vec(),
            replicas: vec![
                ReplicaDescriptor {
                    replica_id: ReplicaId::new(1),
                    node_id: "n1".into(),
                    is_learner: false,
                },
                ReplicaDescriptor {
                    replica_id: ReplicaId::new(2),
                    node_id: "n2".into(),
                    is_learner: false,
                },
                ReplicaDescriptor {
                    replica_id: ReplicaId::new(3),
                    node_id: "n3".into(),
                    is_learner: false,
                },
            ],
            shard: ShardId::new(1),
            lease: None,
            generation: 0,
        }
    }

    #[test]
    fn target_already_replica_picks_lease_transfer() {
        let d = descriptor();
        match plan_transfer(&d, "n1", "n2", 99).unwrap() {
            TransferKind::LeaseTransfer {
                range,
                from_node,
                to_node,
            } => {
                assert_eq!(range, RangeId::new(1));
                assert_eq!(from_node, "n1");
                assert_eq!(to_node, "n2");
            }
            other => panic!("expected LeaseTransfer, got {other:?}"),
        }
    }

    #[test]
    fn target_not_replica_picks_replica_migration() {
        let d = descriptor();
        match plan_transfer(&d, "n1", "n4", 99).unwrap() {
            TransferKind::ReplicaMigration {
                range,
                add_replica,
                remove_replica,
                ..
            } => {
                assert_eq!(range, RangeId::new(1));
                assert_eq!(add_replica.node_id, "n4");
                assert_eq!(add_replica.replica_id, ReplicaId::new(99));
                assert_eq!(remove_replica.node_id, "n1");
            }
            other => panic!("expected ReplicaMigration, got {other:?}"),
        }
    }

    #[test]
    fn from_node_must_be_a_replica() {
        let d = descriptor();
        assert!(plan_transfer(&d, "n99", "n4", 99).is_none());
    }
}
