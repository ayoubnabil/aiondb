//! Cluster invariant verifier.
//!
//! Cheap runtime self-checks ensuring global properties hold :
//!
//! - At most one leaseholder per shard.
//! - Range boundaries don't overlap.
//! - Every member listed in raft state appears in the gossip view.
//! - No range has fewer voters than the declared replication factor.
//!
//! Failures are returned as `InvariantViolation` so the operator can
//! react. The verifier itself is read-only.

use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvariantViolation {
    DuplicateLease {
        shard: u32,
        holders: Vec<u64>,
    },
    RangeOverlap {
        left: u64,
        right: u64,
    },
    MissingMember {
        node_id: u64,
    },
    UnderReplicated {
        range: u64,
        current: usize,
        target: usize,
    },
    NegativeApplied {
        node_id: u64,
    },
}

pub fn check_no_duplicate_leases(leases: &[(u32, u64)]) -> Vec<InvariantViolation> {
    let mut by_shard: std::collections::BTreeMap<u32, Vec<u64>> = std::collections::BTreeMap::new();
    for (shard, holder) in leases {
        by_shard.entry(*shard).or_default().push(*holder);
    }
    by_shard
        .into_iter()
        .filter(|(_, h)| h.len() > 1)
        .map(|(shard, holders)| InvariantViolation::DuplicateLease { shard, holders })
        .collect()
}

pub fn check_range_no_overlap(ranges: &[(u64, Vec<u8>, Vec<u8>)]) -> Vec<InvariantViolation> {
    let mut sorted: Vec<&(u64, Vec<u8>, Vec<u8>)> = ranges.iter().collect();
    sorted.sort_by(|a, b| a.1.cmp(&b.1));
    let mut out = Vec::new();
    for w in sorted.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if !a.2.is_empty() && b.1.as_slice() < a.2.as_slice() {
            out.push(InvariantViolation::RangeOverlap {
                left: a.0,
                right: b.0,
            });
        }
    }
    out
}

pub fn check_members_known(
    raft_member_ids: &[u64],
    gossip_member_ids: &[u64],
) -> Vec<InvariantViolation> {
    let gossip: BTreeSet<u64> = gossip_member_ids.iter().copied().collect();
    raft_member_ids
        .iter()
        .filter(|id| !gossip.contains(id))
        .map(|id| InvariantViolation::MissingMember { node_id: *id })
        .collect()
}

pub fn check_replication_factor(ranges: &[(u64, usize)], target: usize) -> Vec<InvariantViolation> {
    ranges
        .iter()
        .filter(|(_, voters)| *voters < target)
        .map(|(r, v)| InvariantViolation::UnderReplicated {
            range: *r,
            current: *v,
            target,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_lease_is_detected() {
        let v = check_no_duplicate_leases(&[(1, 100), (1, 200), (2, 100)]);
        assert_eq!(v.len(), 1);
        assert!(matches!(
            v[0],
            InvariantViolation::DuplicateLease { shard: 1, .. }
        ));
    }

    #[test]
    fn no_duplicates_returns_empty() {
        let v = check_no_duplicate_leases(&[(1, 100), (2, 200)]);
        assert!(v.is_empty());
    }

    #[test]
    fn overlapping_ranges_are_detected() {
        let v = check_range_no_overlap(&[
            (1, b"a".to_vec(), b"m".to_vec()),
            (2, b"h".to_vec(), b"z".to_vec()), // overlap with (1)
        ]);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn non_overlapping_ranges_pass() {
        let v = check_range_no_overlap(&[
            (1, b"a".to_vec(), b"m".to_vec()),
            (2, b"m".to_vec(), b"z".to_vec()),
        ]);
        assert!(v.is_empty());
    }

    #[test]
    fn members_must_be_in_gossip() {
        let v = check_members_known(&[1, 2, 3], &[1, 3]);
        assert_eq!(v.len(), 1);
        assert!(matches!(
            v[0],
            InvariantViolation::MissingMember { node_id: 2 }
        ));
    }

    #[test]
    fn under_replicated_ranges_are_flagged() {
        let v = check_replication_factor(&[(1, 2), (2, 3)], 3);
        assert_eq!(v.len(), 1);
        assert!(matches!(
            v[0],
            InvariantViolation::UnderReplicated {
                range: 1,
                current: 2,
                target: 3
            }
        ));
    }
}
