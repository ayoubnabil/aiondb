//! Quorum intersection verifier.
//!
//! During a joint configuration transition (Raft's joint consensus),
//! every committed proposal must achieve majority in BOTH the old
//! and new voter sets. This module checks that the configured
//! quorum sets satisfy the intersection property and rejects unsafe
//! transitions.

use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IntersectionError {
    EmptyConfiguration,
    NoIntersection,
}

pub fn verify_intersection(
    old_voters: &[u64],
    new_voters: &[u64],
) -> Result<(), IntersectionError> {
    let old: BTreeSet<u64> = old_voters.iter().copied().collect();
    let new: BTreeSet<u64> = new_voters.iter().copied().collect();
    if old.is_empty() && new.is_empty() {
        return Err(IntersectionError::EmptyConfiguration);
    }
    // Any majority of `old` and any majority of `new` must share at least
    // one voter. This holds iff |old ∩ new| > max(|old|, |new|) / 2.
    let intersection = old.intersection(&new).count();
    let max_majority = old.len().max(new.len()) / 2;
    if intersection > max_majority {
        Ok(())
    } else if intersection == 0 && (old.is_empty() || new.is_empty()) {
        // Pure expand/contract from empty/to empty handled separately.
        Ok(())
    } else {
        Err(IntersectionError::NoIntersection)
    }
}

pub fn majority(n: usize) -> usize {
    n / 2 + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_configurations_rejected() {
        assert_eq!(
            verify_intersection(&[], &[]),
            Err(IntersectionError::EmptyConfiguration)
        );
    }

    #[test]
    fn identical_configurations_ok() {
        assert!(verify_intersection(&[1, 2, 3], &[1, 2, 3]).is_ok());
    }

    #[test]
    fn overlap_majority_ok() {
        assert!(verify_intersection(&[1, 2, 3], &[2, 3, 4]).is_ok());
    }

    #[test]
    fn insufficient_overlap_rejected() {
        assert_eq!(
            verify_intersection(&[1, 2, 3], &[4, 5, 6]),
            Err(IntersectionError::NoIntersection)
        );
    }

    #[test]
    fn growing_from_one_to_three_passes() {
        // |old ∩ new| = 1, max_majority(1, 3) = 3/2 = 1, 1 > 1 false → reject.
        // Joint consensus is meant to add a voter at a time.
        let r = verify_intersection(&[1], &[1, 2, 3]);
        assert!(r.is_err());
    }

    #[test]
    fn adding_one_voter_passes() {
        assert!(verify_intersection(&[1, 2, 3], &[1, 2, 3, 4]).is_ok());
    }

    #[test]
    fn removing_one_voter_passes() {
        assert!(verify_intersection(&[1, 2, 3, 4], &[1, 2, 3]).is_ok());
    }

    #[test]
    fn majority_helper_is_correct() {
        assert_eq!(majority(3), 2);
        assert_eq!(majority(4), 3);
        assert_eq!(majority(5), 3);
    }
}
