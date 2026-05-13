//! Quorum vote counter.
//!
//! Tracks per-voter weights and a configurable quorum threshold.
//! Returns `true` once enough yes-votes are in. Supports the simple
//! majority case (n/2 + 1) as well as flexible weighted quorums
//! used for cross-region leases.

use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Vote {
    Yes,
    No,
    Pending,
}

#[derive(Clone, Debug)]
pub struct QuorumVoter {
    inner: Arc<std::sync::Mutex<VoterState>>,
}

#[derive(Default, Debug)]
struct VoterState {
    weights: BTreeMap<u64, u32>,
    votes: BTreeMap<u64, Vote>,
    target: u32,
}

impl QuorumVoter {
    pub fn new(members: Vec<(u64, u32)>) -> Self {
        let total: u32 = members.iter().map(|(_, w)| *w).sum();
        let weights = members.into_iter().collect();
        Self {
            inner: Arc::new(std::sync::Mutex::new(VoterState {
                weights,
                votes: BTreeMap::new(),
                target: total / 2 + 1,
            })),
        }
    }

    pub fn with_threshold(members: Vec<(u64, u32)>, threshold: u32) -> Self {
        let weights = members.into_iter().collect();
        Self {
            inner: Arc::new(std::sync::Mutex::new(VoterState {
                weights,
                votes: BTreeMap::new(),
                target: threshold,
            })),
        }
    }

    pub fn record(&self, voter: u64, vote: Vote) {
        let mut g = self.inner.lock().unwrap();
        if g.weights.contains_key(&voter) {
            g.votes.insert(voter, vote);
        }
    }

    pub fn yes_weight(&self) -> u32 {
        let g = self.inner.lock().unwrap();
        g.votes
            .iter()
            .filter(|(_, v)| **v == Vote::Yes)
            .filter_map(|(k, _)| g.weights.get(k))
            .copied()
            .sum()
    }

    pub fn no_weight(&self) -> u32 {
        let g = self.inner.lock().unwrap();
        g.votes
            .iter()
            .filter(|(_, v)| **v == Vote::No)
            .filter_map(|(k, _)| g.weights.get(k))
            .copied()
            .sum()
    }

    pub fn target(&self) -> u32 {
        self.inner.lock().unwrap().target
    }

    pub fn is_reached(&self) -> bool {
        self.yes_weight() >= self.target()
    }

    pub fn is_failed(&self) -> bool {
        let total: u32 = self.inner.lock().unwrap().weights.values().sum();
        let max_possible_yes = total - self.no_weight();
        max_possible_yes < self.target()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_majority_reached() {
        let q = QuorumVoter::new(vec![(1, 1), (2, 1), (3, 1)]);
        q.record(1, Vote::Yes);
        q.record(2, Vote::Yes);
        assert!(q.is_reached());
    }

    #[test]
    fn weighted_threshold() {
        let q = QuorumVoter::with_threshold(vec![(1, 3), (2, 1), (3, 1)], 4);
        q.record(1, Vote::Yes);
        assert!(!q.is_reached());
        q.record(2, Vote::Yes);
        assert!(q.is_reached());
    }

    #[test]
    fn unknown_voter_ignored() {
        let q = QuorumVoter::new(vec![(1, 1), (2, 1)]);
        q.record(99, Vote::Yes);
        assert_eq!(q.yes_weight(), 0);
    }

    #[test]
    fn no_votes_count_against() {
        let q = QuorumVoter::new(vec![(1, 1), (2, 1), (3, 1)]);
        q.record(1, Vote::Yes);
        q.record(2, Vote::No);
        q.record(3, Vote::No);
        assert!(q.is_failed());
    }

    #[test]
    fn not_reached_and_not_failed_means_pending() {
        let q = QuorumVoter::new(vec![(1, 1), (2, 1), (3, 1)]);
        q.record(1, Vote::Yes);
        assert!(!q.is_reached());
        assert!(!q.is_failed());
    }

    #[test]
    fn target_default_is_majority() {
        let q = QuorumVoter::new(vec![(1, 1), (2, 1), (3, 1), (4, 1), (5, 1)]);
        assert_eq!(q.target(), 3);
    }

    #[test]
    fn override_vote_uses_latest() {
        let q = QuorumVoter::new(vec![(1, 1), (2, 1)]);
        q.record(1, Vote::No);
        q.record(1, Vote::Yes);
        assert_eq!(q.yes_weight(), 1);
    }
}
