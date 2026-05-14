//! Raft node: state machine managing Follower/Candidate/Leader roles.
//!
//! Integrates with the existing HA election subsystem and adds
//! log replication via `AppendEntries`.

#![allow(
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value
)]

use std::collections::HashMap;
use std::path::PathBuf;

use aiondb_core::{DbError, DbResult};
use tracing::{debug, info};

use crate::protocol::NodeId;

use super::log::{RaftEntry, RaftLog};
use super::rpc::{AppendEntriesRequest, AppendEntriesResponse};
use super::state::{PersistentStateHandle, RaftCommand};

/// Raft role for this node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RaftRole {
    Follower,
    Candidate,
    Leader,
}

/// Per-follower replication state tracked by the leader.
#[derive(Debug)]
struct FollowerProgress {
    /// Next log index to send to this follower.
    next_index: u64,
    /// Highest log index known to be replicated on this follower.
    match_index: u64,
}

/// Core Raft consensus node.
///
/// This does NOT manage network I/O - it produces messages that the
/// caller (the HA orchestrator) must send. Similarly, incoming messages
/// are fed via `handle_append_entries` and `handle_append_entries_response`.
#[derive(Debug)]
pub struct RaftNode {
    node_id: NodeId,
    role: RaftRole,
    state: PersistentStateHandle,
    log: RaftLog,
    /// Last log index applied to the state machine.
    last_applied: u64,
    /// Per-follower replication state (leader only).
    followers: HashMap<u64, FollowerProgress>,
    /// Total cluster size (including this node).
    cluster_size: usize,
}

impl RaftNode {
    /// Create or restore a Raft node from persistent state.
    pub fn open(node_id: NodeId, cluster_size: usize, state_dir: PathBuf) -> DbResult<Self> {
        let state_path = state_dir.join("raft_state.json");
        let log_path = state_dir.join("raft_log.jsonl");

        let state = PersistentStateHandle::open(state_path)?;
        let log = RaftLog::open(log_path)?;
        let commit_index = state.commit_index();
        let last_log_index = log.last_index();
        if commit_index > last_log_index {
            return Err(DbError::internal(format!(
                "Raft state commit index {} is beyond local log end {}",
                commit_index, last_log_index
            )));
        }

        info!(
            node_id = node_id.get(),
            term = state.current_term(),
            log_len = log.len(),
            commit_index,
            "Raft node initialized"
        );

        Ok(Self {
            node_id,
            role: RaftRole::Follower,
            state,
            log,
            last_applied: 0,
            followers: HashMap::new(),
            cluster_size,
        })
    }

    // ─── Getters ───────────────────────────────────────────────

    pub fn role(&self) -> RaftRole {
        self.role
    }

    pub fn current_term(&self) -> u64 {
        self.state.current_term()
    }

    pub fn commit_index(&self) -> u64 {
        self.state.commit_index()
    }

    pub fn last_log_index(&self) -> u64 {
        self.log.last_index()
    }

    pub fn last_log_term(&self) -> u64 {
        self.log.last_term()
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    pub fn log(&self) -> &RaftLog {
        &self.log
    }

    // ─── Role transitions ──────────────────────────────────────

    /// Transition to leader after winning an election.
    /// Initializes per-follower replication state and, for single-node
    /// clusters, immediately commits any pending entries.
    pub fn become_leader(&mut self, peer_ids: &[u64]) -> DbResult<()> {
        let next = next_log_index(self.log.last_index(), "Raft leader initialization")?;
        self.role = RaftRole::Leader;
        self.followers.clear();
        for &peer in peer_ids {
            self.followers.insert(
                peer,
                FollowerProgress {
                    next_index: next,
                    match_index: 0,
                },
            );
        }
        info!(
            node_id = self.node_id.get(),
            term = self.current_term(),
            "became Raft leader"
        );
        // [Fix #6] In a single-node cluster, the leader is the only voter.
        // Try to advance commit index immediately since there are no
        // followers to ack.
        self.try_advance_commit_index()
    }

    /// Transition to follower (e.g., on discovering a higher term).
    pub fn become_follower(&mut self, new_term: u64) -> DbResult<()> {
        self.state.advance_term(new_term)?;
        self.role = RaftRole::Follower;
        self.followers.clear();
        Ok(())
    }

    /// Transition to candidate and start an election.
    pub fn become_candidate(&mut self) -> DbResult<u64> {
        let new_term = next_term(self.current_term(), "Raft election")?;
        self.state.advance_term(new_term)?;
        self.state.vote_for(self.node_id)?;
        self.role = RaftRole::Candidate;
        Ok(new_term)
    }

    // ─── Vote handling ─────────────────────────────────────────

    /// Process a vote request. Handles term advancement internally.
    ///
    /// Returns `(should_grant, our_current_term)`.
    /// Raft §5.1: if `candidate_term > current_term`, step down first.
    /// Raft §5.4.1: grant vote only if candidate's log is at least as
    /// up-to-date as ours.
    pub fn handle_vote_request(
        &mut self,
        candidate_term: u64,
        candidate_id: NodeId,
        candidate_last_log_index: u64,
        candidate_last_log_term: u64,
    ) -> DbResult<(bool, u64)> {
        // [Fix #4] Step down to new term first if candidate_term is higher.
        // This clears voted_for, allowing us to vote in the new term.
        if candidate_term > self.current_term() {
            self.become_follower(candidate_term)?;
        }

        // Reject stale term.
        if candidate_term < self.current_term() {
            return Ok((false, self.current_term()));
        }

        // Check voted_for: must be None or same candidate.
        if let Some(voted) = self.state.voted_for() {
            if voted != candidate_id.get() {
                return Ok((false, self.current_term()));
            }
        }

        // Candidate's log must be at least as up-to-date (Raft §5.4.1).
        if !self.candidate_log_is_up_to_date(candidate_last_log_term, candidate_last_log_index) {
            return Ok((false, self.current_term()));
        }

        // Grant the vote and persist.
        self.state.vote_for(candidate_id)?;
        Ok((true, self.current_term()))
    }

    // ─── AppendEntries handling (follower side) ────────────────

    /// Handle an `AppendEntries` RPC from a leader.
    pub fn handle_append_entries(
        &mut self,
        req: &AppendEntriesRequest,
    ) -> DbResult<AppendEntriesResponse> {
        // [Fix #1] Step down on EQUAL term too - a valid AppendEntries
        // from a leader in our term means we are not the leader.
        // This ensures Candidates step down on receiving a legitimate
        // heartbeat from the elected leader.
        let current_term = self.current_term();
        if req.term > current_term || (req.term == current_term && self.role != RaftRole::Follower)
        {
            self.become_follower(req.term)?;
        }

        // Reject if the request's term is older.
        if req.term < self.current_term() {
            return Ok(AppendEntriesResponse {
                term: self.current_term(),
                node_id: self.node_id.get(),
                success: false,
                match_index: self.log.last_index(),
            });
        }

        // [Fix #5] Log consistency check.
        // If prev_log_index > 0, we must have that entry with the right term.
        // If prev_log_index > our last index, we're missing entries → reject.
        if req.prev_log_index > 0 {
            if req.prev_log_index > self.log.last_index() {
                return Ok(AppendEntriesResponse {
                    term: self.current_term(),
                    node_id: self.node_id.get(),
                    success: false,
                    match_index: self.log.last_index(),
                });
            }
            let local_term = self.log.term_at(req.prev_log_index);
            if local_term != req.prev_log_term {
                debug!(
                    prev_log_index = req.prev_log_index,
                    expected_term = req.prev_log_term,
                    actual_term = local_term,
                    "AppendEntries log consistency check failed"
                );
                return Ok(AppendEntriesResponse {
                    term: self.current_term(),
                    node_id: self.node_id.get(),
                    success: false,
                    match_index: self.log.last_index(),
                });
            }
        }

        // [Fix #5] Append new entries with contiguity validation.
        if !req.entries.is_empty() {
            self.log
                .append_entries_checked(req.prev_log_index, &req.entries)?;
        }

        // Advance commit index.
        if req.leader_commit > self.commit_index() {
            let new_commit = req.leader_commit.min(self.log.last_index());
            self.state.set_commit_index(new_commit)?;
        }

        Ok(AppendEntriesResponse {
            term: self.current_term(),
            node_id: self.node_id.get(),
            success: true,
            match_index: self.log.last_index(),
        })
    }

    // ─── AppendEntries handling (leader side) ──────────────────

    /// Handle an `AppendEntries` response from a follower.
    pub fn handle_append_entries_response(&mut self, resp: &AppendEntriesResponse) -> DbResult<()> {
        if resp.term > self.current_term() {
            self.become_follower(resp.term)?;
            return Ok(());
        }

        if self.role != RaftRole::Leader {
            return Ok(());
        }

        let Some(progress) = self.followers.get_mut(&resp.node_id) else {
            return Ok(());
        };

        if resp.success {
            // [Fix #7] Only advance match_index, never regress.
            if resp.match_index > progress.match_index {
                let next_index = next_log_index(resp.match_index, "Raft follower progress")?;
                progress.match_index = resp.match_index;
                progress.next_index = next_index;
            }
            self.try_advance_commit_index()?;
        } else {
            // Decrement next_index and retry (leader will resend).
            progress.next_index = progress.next_index.saturating_sub(1).max(1);
        }

        Ok(())
    }

    /// Build `AppendEntries` requests for all followers that need entries.
    pub fn build_append_entries_requests(&self) -> Vec<(u64, AppendEntriesRequest)> {
        if self.role != RaftRole::Leader {
            return Vec::new();
        }

        let mut requests = Vec::new();
        for (&peer_id, progress) in &self.followers {
            let prev_log_index = progress.next_index.saturating_sub(1);
            let prev_log_term = self.log.term_at(prev_log_index);
            let entries = self.log.entries_from(progress.next_index).to_vec();

            requests.push((
                peer_id,
                AppendEntriesRequest {
                    term: self.current_term(),
                    leader_id: self.node_id.get(),
                    prev_log_index,
                    prev_log_term,
                    entries,
                    leader_commit: self.commit_index(),
                },
            ));
        }
        requests
    }

    // ─── Log operations (leader) ───────────────────────────────

    /// Propose a new command to the Raft log (leader only).
    /// The entry is appended locally; replication happens on next tick.
    /// In single-node clusters, the entry is committed immediately.
    pub fn propose(&mut self, command: RaftCommand) -> DbResult<u64> {
        if self.role != RaftRole::Leader {
            return Err(DbError::internal(
                "only the Raft leader can propose commands",
            ));
        }
        let index = self.log.append(self.current_term(), command)?;
        // [Fix #6] For single-node clusters, commit immediately.
        self.try_advance_commit_index()?;
        Ok(index)
    }

    /// Return committed but not yet applied entries.
    pub fn unapplied_entries(&self) -> Vec<RaftEntry> {
        let Some(start) = self.last_applied.checked_add(1) else {
            return Vec::new();
        };
        let end = self.commit_index();
        if start > end {
            return Vec::new();
        }
        self.log
            .entries_from(start)
            .iter()
            .filter(|e| e.index <= end)
            .cloned()
            .collect()
    }

    /// Mark entries as applied up to the given index.
    pub fn mark_applied(&mut self, index: u64) {
        if index > self.last_applied {
            self.last_applied = index;
        }
    }

    // ─── Internal ──────────────────────────────────────────────

    /// Check if a candidate's log is at least as up-to-date as ours.
    fn candidate_log_is_up_to_date(
        &self,
        candidate_last_log_term: u64,
        candidate_last_log_index: u64,
    ) -> bool {
        let our_last_term = self.log.last_term();
        let our_last_index = self.log.last_index();
        if candidate_last_log_term != our_last_term {
            return candidate_last_log_term > our_last_term;
        }
        candidate_last_log_index >= our_last_index
    }

    /// Advance the commit index based on `match_index` quorum.
    /// Raft §5.3/§5.4: a log entry is committed when stored on a
    /// majority of servers AND has the current term.
    fn try_advance_commit_index(&mut self) -> DbResult<()> {
        let current_commit = self.commit_index();
        let last_index = self.log.last_index();

        if current_commit >= last_index {
            return Ok(());
        }

        let start = next_log_index(current_commit, "Raft commit advancement")?;
        for n in start..=last_index {
            // Only commit entries from the current term.
            if self.log.term_at(n) != self.current_term() {
                continue;
            }
            // Count how many followers have replicated this entry.
            let replicated = self
                .followers
                .values()
                .filter(|p| p.match_index >= n)
                .count();
            // +1 for the leader itself.
            let total_replicated = replicated.saturating_add(1);
            let quorum = self.cluster_size / 2 + 1;
            if total_replicated >= quorum {
                self.state.set_commit_index(n)?;
            }
        }
        Ok(())
    }
}

fn next_log_index(index: u64, operation: &str) -> DbResult<u64> {
    index
        .checked_add(1)
        .ok_or_else(|| DbError::internal(format!("{operation}: log index overflow")))
}

fn next_term(term: u64, operation: &str) -> DbResult<u64> {
    term.checked_add(1)
        .ok_or_else(|| DbError::internal(format!("{operation}: term overflow")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(node_id: u64, cluster_size: usize) -> RaftNode {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_owned();
        std::mem::forget(dir);
        RaftNode::open(NodeId::new(node_id), cluster_size, path).unwrap()
    }

    #[test]
    fn initial_state() {
        let node = make_node(1, 3);
        assert_eq!(node.role(), RaftRole::Follower);
        assert_eq!(node.current_term(), 0);
        assert_eq!(node.last_log_index(), 0);
    }

    #[test]
    fn open_rejects_commit_index_past_log_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("raft_state.json"),
            r#"{"current_term":3,"voted_for":null,"commit_index":1}"#,
        )
        .unwrap();

        let err = RaftNode::open(NodeId::new(1), 3, dir.path().to_owned()).unwrap_err();
        assert!(err.to_string().contains("beyond local log end"));
    }

    #[test]
    fn become_candidate_advances_term() {
        let mut node = make_node(1, 3);
        let term = node.become_candidate().unwrap();
        assert_eq!(term, 1);
        assert_eq!(node.role(), RaftRole::Candidate);
        assert_eq!(node.current_term(), 1);
    }

    #[test]
    fn become_candidate_rejects_term_overflow() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("raft_state.json");
        std::fs::write(
            &state_path,
            r#"{"current_term":18446744073709551615,"voted_for":null,"commit_index":0}"#,
        )
        .unwrap();
        let mut node = RaftNode::open(NodeId::new(1), 3, dir.path().to_owned()).unwrap();

        let err = node.become_candidate().unwrap_err();
        assert!(err.to_string().contains("overflow"));
    }

    #[test]
    fn become_leader_initializes_followers() {
        let mut node = make_node(1, 3);
        node.become_candidate().unwrap();
        node.become_leader(&[2, 3]).unwrap();
        assert_eq!(node.role(), RaftRole::Leader);
        assert_eq!(node.followers.len(), 2);
    }

    #[test]
    fn become_leader_rejects_log_index_overflow() {
        let dir = tempfile::tempdir().unwrap();
        let entry = RaftEntry {
            index: u64::MAX,
            term: 1,
            command: RaftCommand::Noop,
        };
        std::fs::write(
            dir.path().join("raft_log.jsonl"),
            serde_json::to_string(&entry).unwrap(),
        )
        .unwrap();
        let mut node = RaftNode::open(NodeId::new(1), 3, dir.path().to_owned()).unwrap();

        let err = node.become_leader(&[2, 3]).unwrap_err();
        assert!(err.to_string().contains("overflow"));
        assert_eq!(node.role(), RaftRole::Follower);
    }

    #[test]
    fn propose_appends_to_log() {
        let mut node = make_node(1, 3);
        node.become_candidate().unwrap();
        node.become_leader(&[2, 3]).unwrap();

        let idx = node.propose(RaftCommand::Noop).unwrap();
        assert_eq!(idx, 1);
        assert_eq!(node.last_log_index(), 1);
        assert_eq!(node.log.get(1).unwrap().term, 1);
    }

    #[test]
    fn propose_rejected_for_non_leader() {
        let mut node = make_node(1, 3);
        assert!(node.propose(RaftCommand::Noop).is_err());
    }

    // [Fix #6] Single-node cluster: propose commits immediately.
    #[test]
    fn single_node_propose_commits_immediately() {
        let mut node = make_node(1, 1);
        node.become_candidate().unwrap();
        node.become_leader(&[]).unwrap();

        let idx = node.propose(RaftCommand::Noop).unwrap();
        assert_eq!(idx, 1);
        // Quorum = 1, leader alone satisfies it.
        assert_eq!(node.commit_index(), 1);
    }

    #[test]
    fn append_entries_heartbeat() {
        let mut follower = make_node(2, 3);
        let req = AppendEntriesRequest {
            term: 1,
            leader_id: 1,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![],
            leader_commit: 0,
        };
        let resp = follower.handle_append_entries(&req).unwrap();
        assert!(resp.success);
        assert_eq!(resp.term, 1);
    }

    #[test]
    fn append_entries_rejects_stale_term() {
        let mut follower = make_node(2, 3);
        follower.become_candidate().unwrap();
        follower.become_follower(2).unwrap();

        let req = AppendEntriesRequest {
            term: 1,
            leader_id: 1,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![],
            leader_commit: 0,
        };
        let resp = follower.handle_append_entries(&req).unwrap();
        assert!(!resp.success);
        assert_eq!(resp.term, 2);
    }

    // [Fix #1] Candidate steps down on same-term AppendEntries.
    #[test]
    fn candidate_steps_down_on_same_term_heartbeat() {
        let mut node = make_node(2, 3);
        node.become_candidate().unwrap(); // term 1, role Candidate
        assert_eq!(node.role(), RaftRole::Candidate);

        let req = AppendEntriesRequest {
            term: 1, // same term
            leader_id: 1,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![],
            leader_commit: 0,
        };
        let resp = node.handle_append_entries(&req).unwrap();
        assert!(resp.success);
        assert_eq!(node.role(), RaftRole::Follower);
    }

    // [Fix #1] Leader steps down on same-term AppendEntries (shouldn't
    // happen in normal Raft but guards against split-brain).
    #[test]
    fn leader_steps_down_on_same_term_append_entries() {
        let mut node = make_node(1, 3);
        node.become_candidate().unwrap();
        node.become_leader(&[2, 3]).unwrap();
        assert_eq!(node.role(), RaftRole::Leader);

        // Another leader in the same term (shouldn't happen, but safety).
        let req = AppendEntriesRequest {
            term: 1,
            leader_id: 99,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![],
            leader_commit: 0,
        };
        let resp = node.handle_append_entries(&req).unwrap();
        assert!(resp.success);
        assert_eq!(node.role(), RaftRole::Follower);
    }

    #[test]
    fn append_entries_with_data() {
        let mut follower = make_node(2, 3);
        let entries = vec![
            RaftEntry {
                index: 1,
                term: 1,
                command: RaftCommand::Noop,
            },
            RaftEntry {
                index: 2,
                term: 1,
                command: RaftCommand::AddNode {
                    node_id: 3,
                    address: "10.0.0.3:5433".to_owned(),
                },
            },
        ];
        let req = AppendEntriesRequest {
            term: 1,
            leader_id: 1,
            prev_log_index: 0,
            prev_log_term: 0,
            entries,
            leader_commit: 1,
        };
        let resp = follower.handle_append_entries(&req).unwrap();
        assert!(resp.success);
        assert_eq!(resp.match_index, 2);
        assert_eq!(follower.commit_index(), 1);
    }

    // [Fix #5] Reject entries with gaps.
    #[test]
    fn append_entries_rejects_gap() {
        let mut follower = make_node(2, 3);
        // Follower has no entries. Leader sends entries starting at index 5.
        let req = AppendEntriesRequest {
            term: 1,
            leader_id: 1,
            prev_log_index: 4, // follower doesn't have index 4
            prev_log_term: 1,
            entries: vec![RaftEntry {
                index: 5,
                term: 1,
                command: RaftCommand::Noop,
            }],
            leader_commit: 0,
        };
        let resp = follower.handle_append_entries(&req).unwrap();
        assert!(!resp.success); // rejected due to missing prev entry
    }

    #[test]
    fn leader_advances_commit_on_quorum() {
        let mut leader = make_node(1, 3);
        leader.become_candidate().unwrap();
        leader.become_leader(&[2, 3]).unwrap();

        leader.propose(RaftCommand::Noop).unwrap();
        assert_eq!(leader.commit_index(), 0);

        leader
            .handle_append_entries_response(&AppendEntriesResponse {
                term: 1,
                node_id: 2,
                success: true,
                match_index: 1,
            })
            .unwrap();

        // Leader + follower 2 = 2/3 = quorum.
        assert_eq!(leader.commit_index(), 1);
    }

    #[test]
    fn leader_does_not_commit_without_quorum() {
        let mut leader = make_node(1, 5);
        leader.become_candidate().unwrap();
        leader.become_leader(&[2, 3, 4, 5]).unwrap();

        leader.propose(RaftCommand::Noop).unwrap();

        leader
            .handle_append_entries_response(&AppendEntriesResponse {
                term: 1,
                node_id: 2,
                success: true,
                match_index: 1,
            })
            .unwrap();

        assert_eq!(leader.commit_index(), 0);
    }

    // [Fix #7] Stale response doesn't regress match_index.
    #[test]
    fn stale_response_does_not_regress_match_index() {
        let mut leader = make_node(1, 3);
        leader.become_candidate().unwrap();
        leader.become_leader(&[2, 3]).unwrap();
        leader.propose(RaftCommand::Noop).unwrap();
        leader.propose(RaftCommand::Noop).unwrap();

        // Follower 2 acks up to index 2.
        leader
            .handle_append_entries_response(&AppendEntriesResponse {
                term: 1,
                node_id: 2,
                success: true,
                match_index: 2,
            })
            .unwrap();

        // Stale response with match_index 1 arrives later.
        leader
            .handle_append_entries_response(&AppendEntriesResponse {
                term: 1,
                node_id: 2,
                success: true,
                match_index: 1,
            })
            .unwrap();

        // match_index should still be 2, not regressed to 1.
        assert_eq!(leader.followers.get(&2).unwrap().match_index, 2);
    }

    #[test]
    fn append_entries_response_rejects_next_index_overflow() {
        let mut leader = make_node(1, 3);
        leader.role = RaftRole::Leader;
        leader.followers.insert(
            2,
            FollowerProgress {
                next_index: u64::MAX,
                match_index: u64::MAX - 1,
            },
        );

        let err = leader
            .handle_append_entries_response(&AppendEntriesResponse {
                term: 0,
                node_id: 2,
                success: true,
                match_index: u64::MAX,
            })
            .unwrap_err();

        assert!(err.to_string().contains("overflow"));
        assert_eq!(leader.followers.get(&2).unwrap().match_index, u64::MAX - 1);
    }

    #[test]
    fn build_append_entries_for_followers() {
        let mut leader = make_node(1, 3);
        leader.become_candidate().unwrap();
        leader.become_leader(&[2, 3]).unwrap();
        leader.propose(RaftCommand::Noop).unwrap();

        let requests = leader.build_append_entries_requests();
        assert_eq!(requests.len(), 2);
        for (_, req) in &requests {
            assert_eq!(req.term, 1);
            assert_eq!(req.entries.len(), 1);
        }
    }

    #[test]
    fn step_down_on_higher_term() {
        let mut leader = make_node(1, 3);
        leader.become_candidate().unwrap();
        leader.become_leader(&[2, 3]).unwrap();

        leader
            .handle_append_entries_response(&AppendEntriesResponse {
                term: 5,
                node_id: 2,
                success: false,
                match_index: 0,
            })
            .unwrap();

        assert_eq!(leader.role(), RaftRole::Follower);
        assert_eq!(leader.current_term(), 5);
    }

    #[test]
    fn unapplied_entries() {
        let mut follower = make_node(2, 3);
        let entries = vec![
            RaftEntry {
                index: 1,
                term: 1,
                command: RaftCommand::Noop,
            },
            RaftEntry {
                index: 2,
                term: 1,
                command: RaftCommand::AddNode {
                    node_id: 3,
                    address: "10.0.0.3:5433".to_owned(),
                },
            },
        ];
        let req = AppendEntriesRequest {
            term: 1,
            leader_id: 1,
            prev_log_index: 0,
            prev_log_term: 0,
            entries,
            leader_commit: 2,
        };
        follower.handle_append_entries(&req).unwrap();

        let unapplied = follower.unapplied_entries();
        assert_eq!(unapplied.len(), 2);

        follower.mark_applied(2);
        assert!(follower.unapplied_entries().is_empty());
    }

    #[test]
    fn unapplied_entries_returns_empty_at_max_last_applied() {
        let mut node = make_node(1, 3);
        node.last_applied = u64::MAX;

        assert!(node.unapplied_entries().is_empty());
    }

    // [Fix #4] Vote request with higher term clears voted_for, allowing vote.
    #[test]
    fn vote_request_higher_term_allows_vote() {
        let mut node = make_node(1, 3);
        // Vote for self in term 1.
        node.become_candidate().unwrap();
        node.become_follower(1).unwrap(); // back to follower at term 1
                                          // node already voted for self (node 1) in term 1.

        // Candidate 2 requests vote in term 2 - should succeed because
        // advance_term clears voted_for.
        let (granted, term) = node.handle_vote_request(2, NodeId::new(2), 0, 0).unwrap();
        assert!(granted);
        assert_eq!(term, 2);
    }

    // [Fix #4] Vote request in same term for different candidate is rejected.
    #[test]
    fn vote_request_same_term_different_candidate_rejected() {
        let mut node = make_node(1, 3);
        node.become_candidate().unwrap(); // votes for self at term 1

        let (granted, _) = node.handle_vote_request(1, NodeId::new(2), 0, 0).unwrap();
        assert!(!granted);
    }
}
