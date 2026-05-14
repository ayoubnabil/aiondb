//! Raft RPC message types for log replication.

use serde::{Deserialize, Serialize};

use super::log::RaftEntry;

/// `AppendEntries` RPC sent by the leader to replicate log entries
/// and serve as heartbeats.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppendEntriesRequest {
    /// Leader's current term.
    pub term: u64,
    /// Leader's node ID.
    pub leader_id: u64,
    /// Index of the log entry immediately preceding the new entries.
    pub prev_log_index: u64,
    /// Term of the entry at `prev_log_index`.
    pub prev_log_term: u64,
    /// Log entries to replicate (empty for heartbeats).
    pub entries: Vec<RaftEntry>,
    /// Leader's commit index.
    pub leader_commit: u64,
}

/// Response to an `AppendEntries` RPC.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppendEntriesResponse {
    /// Responder's current term (for leader to update itself).
    pub term: u64,
    /// Responder's node ID.
    pub node_id: u64,
    /// True if the follower successfully appended the entries.
    pub success: bool,
    /// The index of the last entry the follower has after this RPC.
    /// Used by the leader to update `next_index` and `match_index`.
    pub match_index: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raft::state::RaftCommand;

    #[test]
    fn heartbeat_request() {
        let req = AppendEntriesRequest {
            term: 3,
            leader_id: 1,
            prev_log_index: 5,
            prev_log_term: 2,
            entries: vec![],
            leader_commit: 4,
        };
        assert!(req.entries.is_empty());
    }

    #[test]
    fn serde_roundtrip() {
        let req = AppendEntriesRequest {
            term: 1,
            leader_id: 2,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![RaftEntry {
                index: 1,
                term: 1,
                command: RaftCommand::AddNode {
                    node_id: 3,
                    address: "10.0.0.1:5433".to_owned(),
                },
            }],
            leader_commit: 0,
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: AppendEntriesRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn response_serde() {
        let resp = AppendEntriesResponse {
            term: 2,
            node_id: 5,
            success: true,
            match_index: 10,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: AppendEntriesResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, decoded);
    }
}
