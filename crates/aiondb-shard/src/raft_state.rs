//! Per-range Raft state bookkeeping.
//!
//! AionDB's existing [`aiondb-ha`] crate runs one Raft group per node
//! for cluster membership and metadata. A Cockroach-style design
//! instead runs **one Raft group per range** so writes to range A do
//! not contend on range B's quorum, and ranges can split or merge
//! independently. This module provides the registry side of that
//! design: a shared, in-memory view of every range's current Raft
//! state, indexed by [`RangeId`].
//!
//! The registry is intentionally **not** a Raft implementation. It is
//! a bookkeeping table the higher layer writes into and other
//! components consult:
//!
//! - The [`super::leaseholder_loop`] reads `leader_replica_id` to
//!   decide if it still has the right to renew its lease.
//! - The [`super::split::ShardSplitPlanner`] consults
//!   `all_replicas_caught_up()` before promoting a split candidate.
//! - The replication coordinator queries each follower's
//!   [`ReplicaProgress`] to decide between WAL streaming and a
//!   snapshot send.
//!
//! [`aiondb-ha`]: ../../aiondb_ha
//!
//! # Invariants
//!
//! - `term` is monotonically non-decreasing per range.
//! - `commit_index <= leader.last_log_index`.
//! - For every follower, `match_index <= commit_index`.
//! - `applied_index <= commit_index`.
//!
//! Callers are expected to enforce these invariants; the registry only
//! exposes the data and a few helper queries.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::range_descriptor::{RangeId, ReplicaId};

/// Snapshot of one Raft group's leader-side state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RangeRaftState {
    pub term: u64,
    /// Highest log index known to be committed by quorum.
    pub commit_index: u64,
    /// Highest log index this node has applied to its state machine.
    pub applied_index: u64,
    /// Highest log index appended by the leader (may be > commit_index
    /// while quorum is in progress).
    pub last_log_index: u64,
    pub leader_replica_id: Option<ReplicaId>,
}

/// Follower progress as tracked by the leader.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReplicaProgress {
    /// Highest log index known to be replicated to this follower.
    pub match_index: u64,
    /// Next log index the leader will send to this follower.
    pub next_index: u64,
    /// `true` when the leader has heard from this follower recently.
    pub recent_active: bool,
    /// `true` when the leader has paused replication to this follower
    /// (snapshot in flight, follower marked dead, ...).
    pub paused: bool,
}

#[derive(Clone, Debug)]
struct GroupEntry {
    state: RangeRaftState,
    progress: HashMap<ReplicaId, ReplicaProgress>,
    last_heartbeat: Instant,
}

impl Default for GroupEntry {
    fn default() -> Self {
        Self {
            state: RangeRaftState::default(),
            progress: HashMap::new(),
            last_heartbeat: Instant::now(),
        }
    }
}

/// Per-range Raft state registry. Cheap to clone.
#[derive(Clone, Debug, Default)]
pub struct RaftStateRegistry {
    inner: Arc<std::sync::Mutex<HashMap<RangeId, GroupEntry>>>,
}

impl RaftStateRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the current leader-side state of a range. Returns
    /// `RangeRaftState::default()` when the range has no recorded
    /// state yet -- callers can interpret that as "uninitialised".
    pub fn state(&self, range: RangeId) -> RangeRaftState {
        self.lock()
            .get(&range)
            .map(|entry| entry.state)
            .unwrap_or_default()
    }

    /// Publish a fresh leader-side state. Enforces that `term` never
    /// decreases. Returns `false` (no update applied) when the caller
    /// supplies a stale term -- typical when two paths race to record
    /// the same heartbeat.
    pub fn publish_state(&self, range: RangeId, state: RangeRaftState) -> bool {
        let mut guard = self.lock();
        let entry = guard.entry(range).or_default();
        if state.term < entry.state.term {
            return false;
        }
        entry.state = state;
        entry.last_heartbeat = Instant::now();
        true
    }

    /// Record progress for a single follower. Enforces `match_index`
    /// monotonicity within the same term.
    pub fn record_progress(
        &self,
        range: RangeId,
        replica: ReplicaId,
        progress: ReplicaProgress,
    ) -> bool {
        let mut guard = self.lock();
        let entry = guard.entry(range).or_default();
        let slot = entry.progress.entry(replica).or_default();
        if progress.match_index < slot.match_index {
            return false;
        }
        *slot = progress;
        entry.last_heartbeat = Instant::now();
        true
    }

    /// Mark a follower paused (snapshot in progress, marked dead, ...).
    pub fn pause_replica(&self, range: RangeId, replica: ReplicaId) {
        let mut guard = self.lock();
        let entry = guard.entry(range).or_default();
        let slot = entry.progress.entry(replica).or_default();
        slot.paused = true;
        slot.recent_active = false;
    }

    /// Resume replication to a previously-paused follower.
    pub fn resume_replica(&self, range: RangeId, replica: ReplicaId) {
        let mut guard = self.lock();
        if let Some(entry) = guard.get_mut(&range) {
            if let Some(slot) = entry.progress.get_mut(&replica) {
                slot.paused = false;
                slot.recent_active = true;
            }
        }
    }

    /// Look up a follower's last-known progress.
    pub fn progress(&self, range: RangeId, replica: ReplicaId) -> Option<ReplicaProgress> {
        self.lock()
            .get(&range)
            .and_then(|entry| entry.progress.get(&replica).copied())
    }

    /// List every follower's progress for a range. Sorted by replica
    /// id for deterministic output.
    pub fn replica_progress(&self, range: RangeId) -> Vec<(ReplicaId, ReplicaProgress)> {
        let guard = self.lock();
        let entry = match guard.get(&range) {
            Some(e) => e,
            None => return Vec::new(),
        };
        let mut progress: Vec<_> = entry
            .progress
            .iter()
            .map(|(id, prog)| (*id, *prog))
            .collect();
        progress.sort_by_key(|(id, _)| *id);
        progress
    }

    /// `true` when every tracked follower's `match_index >=
    /// commit_index` -- i.e. nobody is behind. Used by the split
    /// planner to avoid splitting while replicas are still catching
    /// up.
    pub fn all_replicas_caught_up(&self, range: RangeId) -> bool {
        let guard = self.lock();
        let entry = match guard.get(&range) {
            Some(e) => e,
            None => return false,
        };
        let commit = entry.state.commit_index;
        if entry.progress.is_empty() {
            // Single-replica or pre-bootstrap: trivially caught up.
            return true;
        }
        entry
            .progress
            .values()
            .all(|p| !p.paused && p.match_index >= commit)
    }

    /// `true` when the leader has heard from a quorum of replicas
    /// within `liveness_window`. Used by lease eligibility checks to
    /// avoid granting a lease to a partitioned leader.
    pub fn has_quorum_contact(&self, range: RangeId, liveness_window: Duration) -> bool {
        let guard = self.lock();
        let entry = match guard.get(&range) {
            Some(e) => e,
            None => return false,
        };
        let now = Instant::now();
        let recent = entry
            .progress
            .values()
            .filter(|p| p.recent_active && !p.paused)
            .count();
        // The leader counts as one of the active members; quorum is
        // ceil((replicas + 1) / 2) over the leader + followers.
        let total = entry.progress.len() + 1;
        let quorum = total / 2 + 1;
        let leader_alive = now.saturating_duration_since(entry.last_heartbeat) < liveness_window;
        leader_alive && (recent + 1) >= quorum
    }

    /// Drop a range entirely. Used when a range merges away or
    /// decommissions.
    pub fn remove(&self, range: RangeId) -> Option<RangeRaftState> {
        self.lock().remove(&range).map(|entry| entry.state)
    }

    /// Snapshot every tracked range, ordered by id.
    pub fn snapshot(&self) -> Vec<(RangeId, RangeRaftState)> {
        let guard = self.lock();
        let mut entries: Vec<_> = guard
            .iter()
            .map(|(range, entry)| (*range, entry.state))
            .collect();
        entries.sort_by_key(|(range, _)| *range);
        entries
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<RangeId, GroupEntry>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(n: u64) -> RangeId {
        RangeId::new(n)
    }
    fn rep(n: u64) -> ReplicaId {
        ReplicaId::new(n)
    }

    #[test]
    fn fresh_registry_returns_default_state() {
        let r = RaftStateRegistry::new();
        let s = r.state(range(1));
        assert_eq!(s, RangeRaftState::default());
    }

    #[test]
    fn publish_state_records_term_and_indices() {
        let r = RaftStateRegistry::new();
        let s = RangeRaftState {
            term: 4,
            commit_index: 100,
            applied_index: 95,
            last_log_index: 110,
            leader_replica_id: Some(rep(1)),
        };
        assert!(r.publish_state(range(1), s));
        assert_eq!(r.state(range(1)), s);
    }

    #[test]
    fn publish_state_rejects_stale_term() {
        let r = RaftStateRegistry::new();
        r.publish_state(
            range(1),
            RangeRaftState {
                term: 5,
                ..Default::default()
            },
        );
        // Stale.
        assert!(!r.publish_state(
            range(1),
            RangeRaftState {
                term: 4,
                ..Default::default()
            }
        ));
        assert_eq!(r.state(range(1)).term, 5);
        // Same term is allowed (idempotent heartbeat).
        assert!(r.publish_state(
            range(1),
            RangeRaftState {
                term: 5,
                commit_index: 10,
                ..Default::default()
            }
        ));
        assert_eq!(r.state(range(1)).commit_index, 10);
    }

    #[test]
    fn record_progress_is_monotonic_within_match_index() {
        let r = RaftStateRegistry::new();
        let p1 = ReplicaProgress {
            match_index: 50,
            next_index: 51,
            recent_active: true,
            paused: false,
        };
        let p2 = ReplicaProgress {
            match_index: 80,
            next_index: 81,
            recent_active: true,
            paused: false,
        };
        let p3 = ReplicaProgress {
            match_index: 70, // regress
            next_index: 71,
            recent_active: true,
            paused: false,
        };
        assert!(r.record_progress(range(1), rep(1), p1));
        assert!(r.record_progress(range(1), rep(1), p2));
        assert!(!r.record_progress(range(1), rep(1), p3));
        assert_eq!(r.progress(range(1), rep(1)).unwrap().match_index, 80);
    }

    #[test]
    fn pause_and_resume_replica() {
        let r = RaftStateRegistry::new();
        r.record_progress(
            range(1),
            rep(2),
            ReplicaProgress {
                match_index: 5,
                next_index: 6,
                recent_active: true,
                paused: false,
            },
        );
        r.pause_replica(range(1), rep(2));
        let p = r.progress(range(1), rep(2)).unwrap();
        assert!(p.paused);
        assert!(!p.recent_active);
        r.resume_replica(range(1), rep(2));
        let p = r.progress(range(1), rep(2)).unwrap();
        assert!(!p.paused);
        assert!(p.recent_active);
    }

    #[test]
    fn all_replicas_caught_up_requires_match_at_or_above_commit() {
        let r = RaftStateRegistry::new();
        r.publish_state(
            range(1),
            RangeRaftState {
                term: 1,
                commit_index: 100,
                applied_index: 100,
                last_log_index: 100,
                leader_replica_id: Some(rep(1)),
            },
        );
        r.record_progress(
            range(1),
            rep(2),
            ReplicaProgress {
                match_index: 100,
                next_index: 101,
                recent_active: true,
                paused: false,
            },
        );
        r.record_progress(
            range(1),
            rep(3),
            ReplicaProgress {
                match_index: 90,
                next_index: 91,
                recent_active: true,
                paused: false,
            },
        );
        assert!(!r.all_replicas_caught_up(range(1)));
        // Bump replica 3 to commit.
        r.record_progress(
            range(1),
            rep(3),
            ReplicaProgress {
                match_index: 100,
                next_index: 101,
                recent_active: true,
                paused: false,
            },
        );
        assert!(r.all_replicas_caught_up(range(1)));
    }

    #[test]
    fn all_replicas_caught_up_treats_paused_as_behind() {
        let r = RaftStateRegistry::new();
        r.publish_state(
            range(1),
            RangeRaftState {
                term: 1,
                commit_index: 100,
                applied_index: 100,
                last_log_index: 100,
                leader_replica_id: Some(rep(1)),
            },
        );
        r.record_progress(
            range(1),
            rep(2),
            ReplicaProgress {
                match_index: 100,
                next_index: 101,
                recent_active: true,
                paused: true,
            },
        );
        assert!(!r.all_replicas_caught_up(range(1)));
    }

    #[test]
    fn has_quorum_contact_counts_leader_plus_active_followers() {
        let r = RaftStateRegistry::new();
        // 3-replica group: leader + 2 followers. Quorum is 2.
        r.publish_state(
            range(1),
            RangeRaftState {
                term: 1,
                commit_index: 50,
                applied_index: 50,
                last_log_index: 50,
                leader_replica_id: Some(rep(1)),
            },
        );
        // Both followers inactive -> no quorum.
        r.record_progress(
            range(1),
            rep(2),
            ReplicaProgress {
                match_index: 50,
                next_index: 51,
                recent_active: false,
                paused: false,
            },
        );
        r.record_progress(
            range(1),
            rep(3),
            ReplicaProgress {
                match_index: 50,
                next_index: 51,
                recent_active: false,
                paused: false,
            },
        );
        assert!(!r.has_quorum_contact(range(1), Duration::from_secs(60)));
        // One follower comes alive -> quorum reached (leader + 1).
        r.record_progress(
            range(1),
            rep(2),
            ReplicaProgress {
                match_index: 50,
                next_index: 51,
                recent_active: true,
                paused: false,
            },
        );
        assert!(r.has_quorum_contact(range(1), Duration::from_secs(60)));
    }

    #[test]
    fn remove_drops_range_and_returns_state() {
        let r = RaftStateRegistry::new();
        let s = RangeRaftState {
            term: 3,
            commit_index: 7,
            applied_index: 7,
            last_log_index: 7,
            leader_replica_id: Some(rep(1)),
        };
        r.publish_state(range(1), s);
        assert_eq!(r.remove(range(1)), Some(s));
        assert_eq!(r.state(range(1)), RangeRaftState::default());
    }

    #[test]
    fn snapshot_returns_sorted_by_range_id() {
        let r = RaftStateRegistry::new();
        for n in [3u64, 1, 2] {
            r.publish_state(
                range(n),
                RangeRaftState {
                    term: 1,
                    ..Default::default()
                },
            );
        }
        let snap = r.snapshot();
        assert_eq!(
            snap.iter().map(|(id, _)| id.get()).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn replica_progress_sorted_by_id() {
        let r = RaftStateRegistry::new();
        for n in [3u64, 1, 2] {
            r.record_progress(
                range(1),
                rep(n),
                ReplicaProgress {
                    match_index: 1,
                    next_index: 2,
                    recent_active: true,
                    paused: false,
                },
            );
        }
        let progress = r.replica_progress(range(1));
        let ids: Vec<u64> = progress.iter().map(|(id, _)| id.get()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }
}
