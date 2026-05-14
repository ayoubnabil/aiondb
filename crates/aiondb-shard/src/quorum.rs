//! Quorum predicate.
//!
//! Computes write / read quorum sizes for a given replica count and
//! checks whether a given ack set meets quorum.
//!
//! Asymmetric quorums : write quorum can be `replicas/2 + 1`
//! (majority) while read quorum can be 1 (any replica) when
//! `read_consistency = relaxed`.

use std::collections::HashSet;
use std::hash::{BuildHasher, Hash};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuorumPolicy {
    Majority,
    All,
    Custom(usize),
}

pub fn required(replica_count: usize, policy: QuorumPolicy) -> usize {
    match policy {
        QuorumPolicy::Majority => replica_count / 2 + 1,
        QuorumPolicy::All => replica_count,
        QuorumPolicy::Custom(n) => n.min(replica_count).max(1),
    }
}

pub fn met<T: Eq + Hash, S: BuildHasher>(
    replicas: &[T],
    acks: &HashSet<T, S>,
    policy: QuorumPolicy,
) -> bool {
    let need = required(replicas.len(), policy);
    let have = replicas.iter().filter(|r| acks.contains(r)).count();
    have >= need
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn majority_of_3_is_2() {
        assert_eq!(required(3, QuorumPolicy::Majority), 2);
    }

    #[test]
    fn majority_of_5_is_3() {
        assert_eq!(required(5, QuorumPolicy::Majority), 3);
    }

    #[test]
    fn all_returns_full_count() {
        assert_eq!(required(7, QuorumPolicy::All), 7);
    }

    #[test]
    fn custom_capped_at_replica_count() {
        assert_eq!(required(3, QuorumPolicy::Custom(99)), 3);
        assert_eq!(required(3, QuorumPolicy::Custom(2)), 2);
    }

    #[test]
    fn met_succeeds_when_majority_acks() {
        let replicas = vec![1u64, 2, 3];
        let mut acks = HashSet::new();
        acks.insert(1);
        acks.insert(2);
        assert!(met(&replicas, &acks, QuorumPolicy::Majority));
    }

    #[test]
    fn met_fails_when_below_majority() {
        let replicas = vec![1u64, 2, 3];
        let mut acks = HashSet::new();
        acks.insert(1);
        assert!(!met(&replicas, &acks, QuorumPolicy::Majority));
    }

    #[test]
    fn met_with_all_policy_requires_every_replica() {
        let replicas = vec![1u64, 2, 3];
        let mut acks = HashSet::new();
        acks.insert(1);
        acks.insert(2);
        assert!(!met(&replicas, &acks, QuorumPolicy::All));
        acks.insert(3);
        assert!(met(&replicas, &acks, QuorumPolicy::All));
    }
}
