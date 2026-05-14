#![allow(clippy::doc_markdown, clippy::must_use_candidate)]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::protocol::{Epoch, HaMessage, NodeId};

/// Persisted slice of `LeaderElection` state - the minimum that must
/// survive a process restart to prevent double-voting in the same epoch.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistedElection {
    current_epoch: u64,
    voted_for: Option<(u64, u64)>, // (epoch, node_id)
}

const MAX_ELECTION_STATE_BYTES: u64 = 16 * 1024;

/// Outcome of a leader election round.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ElectionResult {
    /// This node won the election at the given epoch.
    Won { epoch: Epoch },
    /// This node lost -- another node may have won.
    Lost {
        epoch: Epoch,
        winner: Option<NodeId>,
    },
    /// The election timed out without reaching quorum.
    Timeout { epoch: Epoch },
    /// Not enough nodes in the cluster to form a quorum.
    InsufficientNodes,
}

struct VoteTally {
    votes_granted: HashSet<NodeId>,
    votes_denied: HashSet<NodeId>,
}

/// Epoch-based leader election where the candidate with the highest LSN wins.
pub struct LeaderElection {
    node_id: NodeId,
    current_epoch: AtomicU64,
    voted_for: Mutex<Option<(Epoch, NodeId)>>,
    vote_tally: Mutex<HashMap<u64, VoteTally>>,
    cluster_size: usize,
    /// Optional disk path. When set, every epoch advance / vote grant is
    /// flushed to disk so a process restart cannot reset epoch to 0 and
    /// re-vote in the same epoch (split-brain risk).
    persist_path: Option<PathBuf>,
}

impl LeaderElection {
    pub fn new(node_id: NodeId, cluster_size: usize) -> Self {
        Self {
            node_id,
            current_epoch: AtomicU64::new(0),
            voted_for: Mutex::new(None),
            vote_tally: Mutex::new(HashMap::new()),
            cluster_size,
            persist_path: None,
        }
    }

    /// Build a persistent `LeaderElection`. The on-disk state survives
    /// crash/restart so the node never replays a stale epoch with a
    /// fresh "voted_for=None" view.
    pub fn with_persistence(node_id: NodeId, cluster_size: usize, path: PathBuf) -> Self {
        let mut persisted = Self::load_persisted(&path).unwrap_or_default();
        if let Some((vote_epoch, _)) = persisted.voted_for {
            persisted.current_epoch = persisted.current_epoch.max(vote_epoch);
        }
        let voted_for = persisted
            .voted_for
            .map(|(epoch, nid)| (Epoch::new(epoch), NodeId::new(nid)));
        Self {
            node_id,
            current_epoch: AtomicU64::new(persisted.current_epoch),
            voted_for: Mutex::new(voted_for),
            vote_tally: Mutex::new(HashMap::new()),
            cluster_size,
            persist_path: Some(path),
        }
    }

    fn load_persisted(path: &Path) -> Option<PersistedElection> {
        let file = fs::File::open(path).ok()?;
        let file_len = file.metadata().ok()?.len();
        if file_len > MAX_ELECTION_STATE_BYTES {
            return None;
        }
        let capacity = usize::try_from(file_len).ok()?;
        let mut bytes = Vec::with_capacity(capacity);
        let mut limited = file.take(MAX_ELECTION_STATE_BYTES.saturating_add(1));
        limited.read_to_end(&mut bytes).ok()?;
        if u64::try_from(bytes.len()).ok()? > MAX_ELECTION_STATE_BYTES {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    }

    fn persist(&self) {
        let Some(path) = &self.persist_path else {
            return;
        };
        let voted = self
            .voted_for
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .map(|(epoch, nid)| (epoch.get(), nid.get()));
        let snapshot = PersistedElection {
            current_epoch: self.current_epoch.load(Ordering::Acquire),
            voted_for: voted,
        };
        let _ = persist_election_state(path, &snapshot);
    }

    /// Start a new election: increment epoch, vote for self, return the
    /// `VoteRequest` message to broadcast.
    pub fn start_election(&self, own_lsn: u64) -> (Epoch, HaMessage) {
        let new_epoch = self.next_election_epoch();

        {
            let mut voted = self
                .voted_for
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *voted = Some((new_epoch, self.node_id));
        }

        // Flush epoch + self-vote before broadcasting so a crash between
        // start_election and the first peer's response cannot replay the
        // same epoch with a "no vote" view post-restart.
        self.persist();

        {
            let mut tally = self
                .vote_tally
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut granted = HashSet::new();
            granted.insert(self.node_id);
            tally.insert(
                new_epoch.get(),
                VoteTally {
                    votes_granted: granted,
                    votes_denied: HashSet::new(),
                },
            );
        }

        let msg = HaMessage::VoteRequest {
            epoch: new_epoch,
            candidate_id: self.node_id,
            last_lsn: own_lsn,
        };

        (new_epoch, msg)
    }

    fn next_election_epoch(&self) -> Epoch {
        let mut current = self.current_epoch.load(Ordering::Acquire);
        loop {
            let next = current.saturating_add(1);
            if next == current {
                return Epoch::new(current);
            }
            match self.current_epoch.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Epoch::new(next),
                Err(actual) => current = actual,
            }
        }
    }

    /// Handle an incoming vote request from another candidate.
    ///
    /// Grant the vote if the candidate's epoch is higher than any epoch we have
    /// already voted in AND the candidate's LSN is at least as high as ours.
    pub fn handle_vote_request(
        &self,
        epoch: Epoch,
        candidate_id: NodeId,
        candidate_lsn: u64,
        own_lsn: u64,
    ) -> HaMessage {
        let mut voted = self
            .voted_for
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let local_epoch = self.current_epoch();

        // Always advance to a newer epoch, even if we end up denying this
        // request (Raft-style term monotonicity).
        if epoch > local_epoch {
            self.advance_epoch(epoch);
            if matches!(*voted, Some((voted_epoch, _)) if voted_epoch < epoch) {
                *voted = None;
            }
        }

        let granted = if epoch < self.current_epoch() {
            false
        } else {
            // Same epoch: deny if we already voted for a different candidate.
            let voted_for_other = matches!(
                *voted,
                Some((voted_epoch, voted_candidate))
                    if voted_epoch == epoch && voted_candidate != candidate_id
            );
            if voted_for_other {
                false
            } else if candidate_lsn == u64::MAX {
                // Reject the obvious sentinel that lets a single compromised
                // peer claim infinite catch-up (audit ha F4). Real LSNs grow
                // monotonically with WAL bytes; u64::MAX is unreachable in
                // any honest cluster.
                false
            } else if candidate_lsn >= own_lsn {
                // Idempotent: grant for same candidate in same epoch.
                *voted = Some((epoch, candidate_id));
                true
            } else {
                false
            }
        };
        // Drop the lock before the synchronous fsync so vote-handling
        // throughput is not bottlenecked on disk per request - the
        // persist closure re-acquires its own snapshot.
        drop(voted);
        self.persist();

        HaMessage::VoteResponse {
            epoch,
            voter_id: self.node_id,
            granted,
            voter_lsn: own_lsn,
        }
    }

    /// Record an incoming vote response and return a result if quorum is reached.
    pub fn record_vote(
        &self,
        epoch: Epoch,
        voter_id: NodeId,
        granted: bool,
    ) -> Option<ElectionResult> {
        if self.cluster_size == 0 {
            return Some(ElectionResult::InsufficientNodes);
        }

        let mut tally_map = self
            .vote_tally
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tally = tally_map.get_mut(&epoch.get())?;

        if granted {
            tally.votes_granted.insert(voter_id);
        } else {
            tally.votes_denied.insert(voter_id);
        }

        let quorum = self.quorum_size();

        if tally.votes_granted.len() >= quorum {
            Some(ElectionResult::Won { epoch })
        } else if tally.votes_denied.len() > self.cluster_size.saturating_sub(quorum) {
            Some(ElectionResult::Lost {
                epoch,
                winner: None,
            })
        } else {
            None
        }
    }

    /// Minimum number of votes needed to win an election.
    pub fn quorum_size(&self) -> usize {
        self.cluster_size / 2 + 1
    }

    /// Return the current epoch.
    pub fn current_epoch(&self) -> Epoch {
        Epoch::new(self.current_epoch.load(Ordering::Acquire))
    }

    /// Update the epoch if the provided value is higher than the current one.
    pub fn advance_epoch(&self, epoch: Epoch) {
        let mut current = self.current_epoch.load(Ordering::Acquire);
        let advanced = loop {
            if epoch.get() <= current {
                break false;
            }
            match self.current_epoch.compare_exchange_weak(
                current,
                epoch.get(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break true,
                Err(actual) => current = actual,
            }
        };
        // Drop tallies for epochs that can no longer accumulate votes -
        // long-lived processes otherwise leak one HashMap entry per
        // election round.
        if advanced {
            let mut tally = self
                .vote_tally
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            tally.retain(|round_epoch, _| *round_epoch >= epoch.get());
        }
    }
}

fn persist_election_state(path: &Path, snapshot: &PersistedElection) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_vec(snapshot).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("tmp");
    let mut tmp_file = create_tmp_file(&tmp)?;
    tmp_file.write_all(&json)?;
    tmp_file.sync_all()?;
    drop(tmp_file);
    fs::rename(&tmp, path)?;
    sync_parent_dir(path)
}

fn create_tmp_file(path: &Path) -> std::io::Result<fs::File> {
    for attempt in 0..2 {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempt == 0 => {
                fs::remove_file(path)?;
            }
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "failed to create election temp file",
    ))
}

use aiondb_core::bounded_io::sync_parent_dir;

#[cfg(test)]
mod tests {
    use super::*;

    /// POC: voted_for + current_epoch must survive a process restart so
    /// the node never replays an epoch with a fresh "no vote" view and
    /// double-votes (split-brain).
    #[test]
    fn persistent_election_state_survives_restart() {
        let tmp = std::env::temp_dir().join(format!(
            "aiondb_election_persist_{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);

        let node_a = LeaderElection::with_persistence(NodeId::new(1), 3, tmp.clone());
        let (epoch_a, _msg) = node_a.start_election(1234);
        assert_eq!(epoch_a, Epoch::new(1));
        // Process "crashes" - drop the in-memory state.
        drop(node_a);

        // Restart: re-load from disk. Epoch must be 1 (not 0), and we
        // must remember we already voted for self in epoch 1.
        let node_a2 = LeaderElection::with_persistence(NodeId::new(1), 3, tmp.clone());
        assert_eq!(
            node_a2.current_epoch(),
            Epoch::new(1),
            "epoch must persist across restart to prevent epoch reuse"
        );
        // Asking for a fresh vote in the same epoch from a DIFFERENT
        // candidate must be denied (we already voted for self).
        let resp = node_a2.handle_vote_request(Epoch::new(1), NodeId::new(2), 9999, 1234);
        match resp {
            HaMessage::VoteResponse { granted, .. } => {
                assert!(
                    !granted,
                    "post-restart node must refuse a second vote in epoch we already voted in"
                );
            }
            _ => panic!("expected VoteResponse"),
        }

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn persistent_election_state_ignores_oversized_file() {
        let tmp = std::env::temp_dir().join(format!(
            "aiondb_election_oversized_{}.json",
            std::process::id()
        ));
        std::fs::write(&tmp, vec![b' '; MAX_ELECTION_STATE_BYTES as usize + 1]).unwrap();

        let node = LeaderElection::with_persistence(NodeId::new(1), 3, tmp.clone());
        assert_eq!(node.current_epoch(), Epoch::new(0));

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn persistent_election_state_uses_voted_epoch_floor() {
        let tmp = std::env::temp_dir().join(format!(
            "aiondb_election_epoch_floor_{}.json",
            std::process::id()
        ));
        std::fs::write(&tmp, r#"{"current_epoch":1,"voted_for":[5,2]}"#).unwrap();

        let node = LeaderElection::with_persistence(NodeId::new(1), 3, tmp.clone());
        assert_eq!(node.current_epoch(), Epoch::new(5));

        let (epoch, _) = node.start_election(1000);
        assert_eq!(epoch, Epoch::new(6));

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn three_node_election_win() {
        let node1 = LeaderElection::new(NodeId::new(1), 3);

        let (epoch, vote_req) = node1.start_election(1000);
        assert_eq!(epoch, Epoch::new(1));
        match &vote_req {
            HaMessage::VoteRequest {
                epoch: e,
                candidate_id,
                last_lsn,
            } => {
                assert_eq!(*e, Epoch::new(1));
                assert_eq!(*candidate_id, NodeId::new(1));
                assert_eq!(*last_lsn, 1000);
            }
            _ => panic!("expected VoteRequest"),
        }

        // Node 1 already voted for itself (1 grant). Receive grant from node 2.
        let result = node1.record_vote(epoch, NodeId::new(2), true);
        assert_eq!(result, Some(ElectionResult::Won { epoch }));
    }

    #[test]
    fn start_election_saturates_at_max_epoch() {
        let node = LeaderElection::new(NodeId::new(1), 3);
        node.advance_epoch(Epoch::new(u64::MAX));

        let (epoch, _) = node.start_election(1000);

        assert_eq!(epoch, Epoch::new(u64::MAX));
        assert_eq!(node.current_epoch(), Epoch::new(u64::MAX));
    }

    #[test]
    fn three_node_election_lose() {
        let node1 = LeaderElection::new(NodeId::new(1), 3);
        let (epoch, _) = node1.start_election(1000);

        // Node 2 and node 3 both deny.
        let r1 = node1.record_vote(epoch, NodeId::new(2), false);
        assert_eq!(r1, None);
        let r2 = node1.record_vote(epoch, NodeId::new(3), false);
        assert_eq!(
            r2,
            Some(ElectionResult::Lost {
                epoch,
                winner: None,
            })
        );
    }

    #[test]
    fn vote_rejected_when_candidate_has_lower_lsn() {
        let node2 = LeaderElection::new(NodeId::new(2), 3);
        let response = node2.handle_vote_request(Epoch::new(5), NodeId::new(1), 500, 1000);
        match response {
            HaMessage::VoteResponse { granted, .. } => {
                assert!(!granted, "should deny vote when candidate LSN < own LSN");
            }
            _ => panic!("expected VoteResponse"),
        }
    }

    #[test]
    fn vote_granted_when_candidate_has_higher_lsn() {
        let node2 = LeaderElection::new(NodeId::new(2), 3);
        let response = node2.handle_vote_request(Epoch::new(5), NodeId::new(1), 2000, 1000);
        match response {
            HaMessage::VoteResponse { granted, .. } => {
                assert!(granted, "should grant vote when candidate LSN >= own LSN");
            }
            _ => panic!("expected VoteResponse"),
        }
    }

    #[test]
    fn vote_rejected_for_already_voted_epoch() {
        let node2 = LeaderElection::new(NodeId::new(2), 3);
        let r1 = node2.handle_vote_request(Epoch::new(5), NodeId::new(1), 2000, 1000);
        match &r1 {
            HaMessage::VoteResponse { granted, .. } => assert!(*granted),
            _ => panic!("expected VoteResponse"),
        }
        // Same epoch again from a different candidate.
        let r2 = node2.handle_vote_request(Epoch::new(5), NodeId::new(3), 3000, 1000);
        match r2 {
            HaMessage::VoteResponse { granted, .. } => {
                assert!(!granted, "should deny second vote in same epoch");
            }
            _ => panic!("expected VoteResponse"),
        }
    }

    #[test]
    fn vote_same_candidate_same_epoch_is_idempotent() {
        let node2 = LeaderElection::new(NodeId::new(2), 3);
        let r1 = node2.handle_vote_request(Epoch::new(5), NodeId::new(1), 2000, 1000);
        match r1 {
            HaMessage::VoteResponse { granted, .. } => assert!(granted),
            _ => panic!("expected VoteResponse"),
        }
        let r2 = node2.handle_vote_request(Epoch::new(5), NodeId::new(1), 2000, 1000);
        match r2 {
            HaMessage::VoteResponse { granted, .. } => {
                assert!(
                    granted,
                    "same candidate should remain granted in same epoch"
                );
            }
            _ => panic!("expected VoteResponse"),
        }
    }

    #[test]
    fn higher_epoch_rejection_still_advances_epoch() {
        let node2 = LeaderElection::new(NodeId::new(2), 3);
        assert_eq!(node2.current_epoch(), Epoch::new(0));
        let response = node2.handle_vote_request(Epoch::new(7), NodeId::new(1), 10, 100);
        match response {
            HaMessage::VoteResponse { granted, .. } => {
                assert!(!granted, "candidate with stale LSN must be denied");
            }
            _ => panic!("expected VoteResponse"),
        }
        assert_eq!(
            node2.current_epoch(),
            Epoch::new(7),
            "epoch must advance even on denied vote"
        );
    }

    #[test]
    fn quorum_sizes() {
        assert_eq!(LeaderElection::new(NodeId::new(1), 0).quorum_size(), 1);
        assert_eq!(LeaderElection::new(NodeId::new(1), 1).quorum_size(), 1);
        assert_eq!(LeaderElection::new(NodeId::new(1), 2).quorum_size(), 2);
        assert_eq!(LeaderElection::new(NodeId::new(1), 3).quorum_size(), 2);
        assert_eq!(LeaderElection::new(NodeId::new(1), 5).quorum_size(), 3);
    }

    #[test]
    fn zero_node_cluster_reports_insufficient_nodes() {
        let node = LeaderElection::new(NodeId::new(1), 0);
        let (epoch, _) = node.start_election(1000);

        assert_eq!(
            node.record_vote(epoch, NodeId::new(2), false),
            Some(ElectionResult::InsufficientNodes)
        );
    }

    #[test]
    fn advance_epoch_only_increases() {
        let e = LeaderElection::new(NodeId::new(1), 3);
        e.advance_epoch(Epoch::new(10));
        assert_eq!(e.current_epoch(), Epoch::new(10));
        e.advance_epoch(Epoch::new(5));
        assert_eq!(e.current_epoch(), Epoch::new(10));
    }
}
