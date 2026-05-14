//! Raft read-index optimisation.
//!
//! Standard Raft requires a write to the log for every linearizable
//! read so the leader is sure it has not been deposed. The read-index
//! optimisation avoids the write by :
//!
//! 1. Recording the leader's current `commit_index` as the "read
//!    index".
//! 2. Sending heartbeats (or piggybacking on the next one) to confirm
//!    the leader still has quorum support.
//! 3. Waiting until the local state machine has applied up to the
//!    read index.
//! 4. Serving the read.
//!
//! This is the same algorithm used by etcd, TiKV and CockroachDB. It
//! adds zero log entries while preserving linearisability.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aiondb_core::{DbError, DbResult};

use crate::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use crate::raft::RaftRole;

/// A pending read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingRead {
    pub group: MultiRaftGroupId,
    pub read_index: u64,
    pub requested_at: Instant,
}

#[derive(Clone, Debug, Default)]
pub struct ReadIndexCoordinator {
    pending: Arc<std::sync::Mutex<HashMap<u64, PendingRead>>>,
    next_id: Arc<std::sync::atomic::AtomicU64>,
}

impl ReadIndexCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fresh read-index request for `group`. Returns the
    /// request id + the snapshot read index the leader observed.
    /// Errors when the local node is not the leader of `group`.
    pub fn request(
        &self,
        registry: &MultiRaftRegistry,
        group: MultiRaftGroupId,
    ) -> DbResult<(u64, u64)> {
        let state = registry
            .group_state(group)
            .ok_or_else(|| DbError::internal(format!("group {group} not open")))?;
        if !matches!(state.role, RaftRole::Leader) {
            return Err(DbError::internal(format!(
                "read-index requires leader; local node is {:?}",
                state.role
            )));
        }
        let read_index = state.commit_index;
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.pending.lock().unwrap().insert(
            id,
            PendingRead {
                group,
                read_index,
                requested_at: Instant::now(),
            },
        );
        Ok((id, read_index))
    }

    /// Resolve a request once the local apply index has caught up.
    /// Returns `true` when the request can be answered (caller can
    /// run the read), `false` otherwise.
    pub fn try_resolve(&self, request_id: u64, local_applied_index: u64) -> bool {
        let guard = self.pending.lock().unwrap();
        let Some(pending) = guard.get(&request_id) else {
            return false;
        };
        local_applied_index >= pending.read_index
    }

    /// Drop a request. Caller should call this after answering or
    /// timing out.
    pub fn complete(&self, request_id: u64) -> Option<PendingRead> {
        self.pending.lock().unwrap().remove(&request_id)
    }

    /// Wait (async) until `try_resolve` returns true. Polls every
    /// 5ms. Returns the read index on success.
    pub async fn wait_for(
        &self,
        request_id: u64,
        applied_index_fn: impl Fn() -> u64,
        timeout: Duration,
    ) -> DbResult<u64> {
        let start = tokio::time::Instant::now();
        loop {
            let applied = applied_index_fn();
            if self.try_resolve(request_id, applied) {
                let pending = self.complete(request_id);
                return Ok(pending.map(|p| p.read_index).unwrap_or(applied));
            }
            if start.elapsed() >= timeout {
                self.complete(request_id);
                return Err(DbError::internal(
                    "read-index wait timed out before applied index caught up",
                ));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Diagnostic snapshot of every pending read.
    pub fn snapshot(&self) -> Vec<(u64, PendingRead)> {
        let guard = self.pending.lock().unwrap();
        let mut out: Vec<_> = guard.iter().map(|(id, p)| (*id, *p)).collect();
        out.sort_by_key(|(id, _)| *id);
        out
    }

    pub fn len(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
    use crate::protocol::NodeId;
    use crate::raft::RaftCommand;

    fn fresh() -> (tempfile::TempDir, Arc<MultiRaftRegistry>) {
        let tmp = tempfile::tempdir().unwrap();
        let reg = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
        reg.create_group(MultiRaftGroupId::new(1), 1).unwrap();
        reg.become_leader(MultiRaftGroupId::new(1), &[]).unwrap();
        (tmp, reg)
    }

    #[test]
    fn request_on_non_leader_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap();
        reg.create_group(MultiRaftGroupId::new(1), 3).unwrap();
        let c = ReadIndexCoordinator::new();
        let err = c.request(&reg, MultiRaftGroupId::new(1)).unwrap_err();
        assert!(err.to_string().contains("read-index requires leader"));
    }

    #[test]
    fn request_captures_commit_index() {
        let (_t, reg) = fresh();
        // Propose a few entries to advance commit_index.
        for _ in 0..3 {
            reg.propose(MultiRaftGroupId::new(1), RaftCommand::Noop)
                .unwrap();
        }
        let c = ReadIndexCoordinator::new();
        let (id, ri) = c.request(&reg, MultiRaftGroupId::new(1)).unwrap();
        assert!(ri >= 3);
        assert_eq!(c.len(), 1);
        c.complete(id);
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn try_resolve_waits_until_applied_catches_up() {
        let (_t, reg) = fresh();
        reg.propose(MultiRaftGroupId::new(1), RaftCommand::Noop)
            .unwrap();
        let c = ReadIndexCoordinator::new();
        let (id, _) = c.request(&reg, MultiRaftGroupId::new(1)).unwrap();
        assert!(!c.try_resolve(id, 0), "applied below read_index");
        assert!(c.try_resolve(id, 100), "applied above read_index");
    }

    #[tokio::test]
    async fn wait_for_completes_when_applied_advances() {
        let (_t, reg) = fresh();
        reg.propose(MultiRaftGroupId::new(1), RaftCommand::Noop)
            .unwrap();
        let c = ReadIndexCoordinator::new();
        let (id, _) = c.request(&reg, MultiRaftGroupId::new(1)).unwrap();
        let applied = Arc::new(AtomicU64::new(0));
        let applied_clone = Arc::clone(&applied);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            applied_clone.store(100, Ordering::SeqCst);
        });
        let ri = c
            .wait_for(
                id,
                move || applied.load(Ordering::SeqCst),
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert!(ri >= 1);
    }

    #[tokio::test]
    async fn wait_for_times_out_when_applied_never_catches_up() {
        let (_t, reg) = fresh();
        reg.propose(MultiRaftGroupId::new(1), RaftCommand::Noop)
            .unwrap();
        let c = ReadIndexCoordinator::new();
        let (id, _) = c.request(&reg, MultiRaftGroupId::new(1)).unwrap();
        let err = c
            .wait_for(id, || 0, Duration::from_millis(20))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"));
    }
}
