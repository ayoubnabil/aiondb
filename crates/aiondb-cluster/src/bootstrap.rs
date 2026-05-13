//! Cluster bootstrap helper.
//!
//! Coordinates the steps needed to spin up a fresh cluster :
//!
//! 1. Validate the seed node list (>= 1, distinct ids).
//! 2. Compute the initial Raft voter set.
//! 3. Configure the metadata Raft's initial peer list.
//! 4. Emit a deterministic cluster id derived from the seed set.
//!
//! Used by `aiondb-server` at first start.

use std::collections::BTreeSet;

use aiondb_core::{DbError, DbResult};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootstrapPlan {
    pub cluster_id: String,
    pub voter_ids: Vec<u64>,
    pub initial_leader: u64,
}

pub fn plan(seed_node_ids: &[u64]) -> DbResult<BootstrapPlan> {
    if seed_node_ids.is_empty() {
        return Err(DbError::internal(
            "cluster bootstrap requires at least one seed node",
        ));
    }
    let set: BTreeSet<u64> = seed_node_ids.iter().copied().collect();
    if set.len() != seed_node_ids.len() {
        return Err(DbError::internal("duplicate node ids in seed set"));
    }
    let voter_ids: Vec<u64> = set.iter().copied().collect();
    let initial_leader = *voter_ids.first().expect("non-empty");
    let cluster_id = compute_cluster_id(&voter_ids);
    Ok(BootstrapPlan {
        cluster_id,
        voter_ids,
        initial_leader,
    })
}

fn compute_cluster_id(voters: &[u64]) -> String {
    // Stable hash of the sorted voter set.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for v in voters {
        for byte in v.to_le_bytes() {
            h ^= u64::from(byte);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    format!("cluster-{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_seed_set_is_rejected() {
        assert!(plan(&[]).is_err());
    }

    #[test]
    fn duplicate_node_ids_rejected() {
        assert!(plan(&[1, 2, 2, 3]).is_err());
    }

    #[test]
    fn voter_set_is_sorted() {
        let p = plan(&[3, 1, 2]).unwrap();
        assert_eq!(p.voter_ids, vec![1, 2, 3]);
        assert_eq!(p.initial_leader, 1);
    }

    #[test]
    fn cluster_id_is_deterministic_under_permutations() {
        let p1 = plan(&[1, 2, 3]).unwrap();
        let p2 = plan(&[3, 1, 2]).unwrap();
        assert_eq!(p1.cluster_id, p2.cluster_id);
    }

    #[test]
    fn different_seed_sets_produce_different_cluster_ids() {
        let p1 = plan(&[1, 2, 3]).unwrap();
        let p2 = plan(&[1, 2, 4]).unwrap();
        assert_ne!(p1.cluster_id, p2.cluster_id);
    }

    #[test]
    fn single_seed_is_valid_and_leader() {
        let p = plan(&[42]).unwrap();
        assert_eq!(p.voter_ids, vec![42]);
        assert_eq!(p.initial_leader, 42);
    }
}
