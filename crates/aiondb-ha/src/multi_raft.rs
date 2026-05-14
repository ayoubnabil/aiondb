//! Multi-Raft registry: one [`RaftNode`] per range.
//!
//! Cockroach-style distribution runs **independent Raft groups per
//! range** so writes against range A never contend with range B's
//! quorum. This module wraps [`RaftNode`] with a registry that:
//!
//! - Creates one persistent state directory per range under a shared
//!   root, so each Raft group's log and `voted_for` live in isolation.
//! - Routes AppendEntries / vote / propose calls to the right group.
//! - Provides batch tick + commit-index queries used by the lease
//!   loop and the snapshot-send coordinator.
//!
//! The registry is intentionally thin -- the per-group state machine
//! is the existing single-Raft implementation, just instantiated
//! N times. That gives us multi-range consensus without rewriting the
//! Raft core.
//!
//! # Storage layout
//!
//! ```text
//! <root>/
//!   range-1/
//!     raft_state.json
//!     raft_log.jsonl
//!   range-2/
//!     raft_state.json
//!     raft_log.jsonl
//!   ...
//! ```
//!
//! # Concurrency
//!
//! Each group lives behind its own `Mutex<RaftNode>` so concurrent
//! operations on distinct ranges proceed in parallel. The registry
//! itself uses a `RwLock` so adding / removing groups does not block
//! group-local progress.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use aiondb_core::{DbError, DbResult};
use tracing::{debug, warn};

use crate::protocol::NodeId;
use crate::raft::{
    AppendEntriesRequest, AppendEntriesResponse, RaftCommand, RaftEntry, RaftNode, RaftRole,
};

/// Opaque per-group identifier. Matches the conceptual
/// `aiondb_shard::range_descriptor::RangeId` -- duplicated here to
/// avoid a circular crate dependency.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MultiRaftGroupId(u64);

impl MultiRaftGroupId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for MultiRaftGroupId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "range-{}", self.0)
    }
}

/// Snapshot of one group's state for introspection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GroupState {
    pub group: MultiRaftGroupId,
    pub role: RaftRole,
    pub current_term: u64,
    pub commit_index: u64,
    pub last_log_index: u64,
}

#[derive(Debug)]
struct GroupEntry {
    node: Mutex<RaftNode>,
}

/// Multi-Raft registry. Cheap to clone.
#[derive(Clone, Debug)]
pub struct MultiRaftRegistry {
    inner: Arc<RwLock<MultiRaftInner>>,
    local_node_id: NodeId,
    storage_root: PathBuf,
}

#[derive(Debug)]
struct MultiRaftInner {
    groups: HashMap<MultiRaftGroupId, Arc<GroupEntry>>,
}

impl MultiRaftRegistry {
    /// Create a fresh registry rooted at `storage_root`. The directory
    /// must exist and be writable -- the registry creates one
    /// subdirectory per group on demand.
    pub fn new(local_node_id: NodeId, storage_root: impl Into<PathBuf>) -> DbResult<Self> {
        let storage_root = storage_root.into();
        if !storage_root.exists() {
            std::fs::create_dir_all(&storage_root).map_err(|e| {
                DbError::internal(format!(
                    "failed to create multi-raft storage root {}: {e}",
                    storage_root.display()
                ))
            })?;
        }
        Ok(Self {
            inner: Arc::new(RwLock::new(MultiRaftInner {
                groups: HashMap::new(),
            })),
            local_node_id,
            storage_root,
        })
    }

    pub fn local_node_id(&self) -> NodeId {
        self.local_node_id
    }

    /// Create a new Raft group for `group_id` with `cluster_size`
    /// total members (including the local node). Returns the initial
    /// state. If the group already exists, returns an error.
    pub fn create_group(
        &self,
        group_id: MultiRaftGroupId,
        cluster_size: usize,
    ) -> DbResult<GroupState> {
        let dir = self.group_dir(group_id);
        std::fs::create_dir_all(&dir).map_err(|e| {
            DbError::internal(format!("failed to create group dir {}: {e}", dir.display()))
        })?;
        let mut guard = self.lock_write();
        if guard.groups.contains_key(&group_id) {
            return Err(DbError::internal(format!(
                "multi-raft group {group_id} already exists",
            )));
        }
        let node = RaftNode::open(self.local_node_id, cluster_size, dir)?;
        let state = group_state_from(group_id, &node);
        guard.groups.insert(
            group_id,
            Arc::new(GroupEntry {
                node: Mutex::new(node),
            }),
        );
        debug!(?group_id, "multi-raft group created");
        Ok(state)
    }

    /// Open an existing group from on-disk state. Useful at startup
    /// when the catalog describes ranges that already have logs.
    pub fn open_group(
        &self,
        group_id: MultiRaftGroupId,
        cluster_size: usize,
    ) -> DbResult<GroupState> {
        let dir = self.group_dir(group_id);
        if !dir.exists() {
            return Err(DbError::internal(format!(
                "multi-raft group {group_id} has no on-disk state at {}",
                dir.display()
            )));
        }
        let mut guard = self.lock_write();
        if guard.groups.contains_key(&group_id) {
            return Err(DbError::internal(format!(
                "multi-raft group {group_id} is already open"
            )));
        }
        let node = RaftNode::open(self.local_node_id, cluster_size, dir)?;
        let state = group_state_from(group_id, &node);
        guard.groups.insert(
            group_id,
            Arc::new(GroupEntry {
                node: Mutex::new(node),
            }),
        );
        Ok(state)
    }

    /// Remove a group from the registry. The on-disk state is
    /// **left intact** by default so a misclick is recoverable;
    /// pass `delete_storage = true` for the destructive variant
    /// (range fully decommissioned).
    pub fn close_group(&self, group_id: MultiRaftGroupId, delete_storage: bool) -> DbResult<()> {
        let mut guard = self.lock_write();
        guard
            .groups
            .remove(&group_id)
            .ok_or_else(|| DbError::internal(format!("group {group_id} not open")))?;
        drop(guard);
        if delete_storage {
            let dir = self.group_dir(group_id);
            if let Err(err) = std::fs::remove_dir_all(&dir) {
                warn!(?group_id, error = %err, "could not remove group storage");
            }
        }
        Ok(())
    }

    /// Inspect a group's current state. Returns `None` when the group
    /// is not open.
    pub fn group_state(&self, group_id: MultiRaftGroupId) -> Option<GroupState> {
        let entry = self.lookup_entry(group_id)?;
        let node = entry.node.lock().unwrap();
        Some(group_state_from(group_id, &node))
    }

    /// Snapshot every open group, sorted by id.
    pub fn snapshot(&self) -> Vec<GroupState> {
        let guard = self.lock_read();
        let mut states: Vec<GroupState> = guard
            .groups
            .iter()
            .map(|(id, entry)| {
                let node = entry.node.lock().unwrap();
                group_state_from(*id, &node)
            })
            .collect();
        states.sort_by_key(|s| s.group);
        states
    }

    /// Number of open groups.
    pub fn len(&self) -> usize {
        self.lock_read().groups.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock_read().groups.is_empty()
    }

    /// Propose a command to a group's leader. Returns the assigned
    /// log index. Errors with `not_leader` when the local node is
    /// not currently the leader of the group.
    pub fn propose(&self, group_id: MultiRaftGroupId, command: RaftCommand) -> DbResult<u64> {
        let entry = self
            .lookup_entry(group_id)
            .ok_or_else(|| DbError::internal(format!("group {group_id} not open")))?;
        let mut node = entry.node.lock().unwrap();
        if !matches!(node.role(), RaftRole::Leader) {
            return Err(DbError::internal(format!(
                "group {group_id} is not led by this node (role = {:?})",
                node.role()
            )));
        }
        node.propose(command)
    }

    /// Route an inbound `AppendEntries` to the right group.
    pub fn handle_append_entries(
        &self,
        group_id: MultiRaftGroupId,
        req: &AppendEntriesRequest,
    ) -> DbResult<AppendEntriesResponse> {
        let entry = self
            .lookup_entry(group_id)
            .ok_or_else(|| DbError::internal(format!("group {group_id} not open")))?;
        let mut node = entry.node.lock().unwrap();
        node.handle_append_entries(req)
    }

    /// Route an inbound `AppendEntriesResponse` to the right group.
    pub fn handle_append_entries_response(
        &self,
        group_id: MultiRaftGroupId,
        resp: &AppendEntriesResponse,
    ) -> DbResult<()> {
        let entry = self
            .lookup_entry(group_id)
            .ok_or_else(|| DbError::internal(format!("group {group_id} not open")))?;
        let mut node = entry.node.lock().unwrap();
        node.handle_append_entries_response(resp)
    }

    /// Promote a group to leader. Equivalent to winning an election;
    /// the caller is responsible for making sure that's actually the
    /// outcome the network reported.
    pub fn become_leader(&self, group_id: MultiRaftGroupId, peer_ids: &[u64]) -> DbResult<()> {
        let entry = self
            .lookup_entry(group_id)
            .ok_or_else(|| DbError::internal(format!("group {group_id} not open")))?;
        let mut node = entry.node.lock().unwrap();
        node.become_leader(peer_ids)
    }

    /// Demote a group back to Follower at a higher term.
    pub fn become_follower(&self, group_id: MultiRaftGroupId, new_term: u64) -> DbResult<()> {
        let entry = self
            .lookup_entry(group_id)
            .ok_or_else(|| DbError::internal(format!("group {group_id} not open")))?;
        let mut node = entry.node.lock().unwrap();
        node.become_follower(new_term)
    }

    /// Drain every committed-but-unapplied entry from `group_id`.
    /// Caller applies them to its state machine, then calls
    /// [`Self::mark_applied`].
    pub fn unapplied_entries(&self, group_id: MultiRaftGroupId) -> DbResult<Vec<RaftEntry>> {
        let entry = self
            .lookup_entry(group_id)
            .ok_or_else(|| DbError::internal(format!("group {group_id} not open")))?;
        let node = entry.node.lock().unwrap();
        Ok(node.unapplied_entries())
    }

    /// Acknowledge that the state machine has applied `index` for
    /// `group_id`. Bounds `last_applied` for that group.
    pub fn mark_applied(&self, group_id: MultiRaftGroupId, index: u64) -> DbResult<()> {
        let entry = self
            .lookup_entry(group_id)
            .ok_or_else(|| DbError::internal(format!("group {group_id} not open")))?;
        let mut node = entry.node.lock().unwrap();
        node.mark_applied(index);
        Ok(())
    }

    /// Produce outbound `AppendEntries` requests this leader needs to
    /// send. Returns `(follower_id, request)` pairs.
    pub fn build_append_entries_requests(
        &self,
        group_id: MultiRaftGroupId,
    ) -> DbResult<Vec<(u64, AppendEntriesRequest)>> {
        let entry = self
            .lookup_entry(group_id)
            .ok_or_else(|| DbError::internal(format!("group {group_id} not open")))?;
        let node = entry.node.lock().unwrap();
        Ok(node.build_append_entries_requests())
    }

    fn group_dir(&self, group_id: MultiRaftGroupId) -> PathBuf {
        self.storage_root.join(format!("range-{}", group_id.get()))
    }

    fn lookup_entry(&self, group_id: MultiRaftGroupId) -> Option<Arc<GroupEntry>> {
        self.lock_read().groups.get(&group_id).cloned()
    }

    fn lock_read(&self) -> std::sync::RwLockReadGuard<'_, MultiRaftInner> {
        self.inner.read().unwrap()
    }

    fn lock_write(&self) -> std::sync::RwLockWriteGuard<'_, MultiRaftInner> {
        self.inner.write().unwrap()
    }

    /// Filesystem path the registry uses for `group_id`.
    pub fn group_storage_dir(&self, group_id: MultiRaftGroupId) -> PathBuf {
        self.group_dir(group_id)
    }

    /// Root storage directory of the registry.
    pub fn storage_root(&self) -> &Path {
        &self.storage_root
    }
}

fn group_state_from(group_id: MultiRaftGroupId, node: &RaftNode) -> GroupState {
    GroupState {
        group: group_id,
        role: node.role(),
        current_term: node.current_term(),
        commit_index: node.commit_index(),
        last_log_index: node.last_log_index(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn group(n: u64) -> MultiRaftGroupId {
        MultiRaftGroupId::new(n)
    }

    fn node_id(n: u64) -> NodeId {
        NodeId::new(n)
    }

    #[test]
    fn create_and_inspect_group() {
        let root = tmpdir();
        let reg = MultiRaftRegistry::new(node_id(1), root.path()).unwrap();
        let state = reg.create_group(group(1), 1).unwrap();
        assert_eq!(state.group, group(1));
        assert_eq!(state.role, RaftRole::Follower);
        assert_eq!(reg.len(), 1);
        assert!(reg.storage_root().join("range-1").exists());
    }

    #[test]
    fn duplicate_create_is_rejected() {
        let root = tmpdir();
        let reg = MultiRaftRegistry::new(node_id(1), root.path()).unwrap();
        reg.create_group(group(1), 1).unwrap();
        let err = reg.create_group(group(1), 1).unwrap_err();
        assert!(err.to_string().contains("already exists"), "err: {err}");
    }

    #[test]
    fn close_group_clears_registry_but_preserves_storage_by_default() {
        let root = tmpdir();
        let reg = MultiRaftRegistry::new(node_id(1), root.path()).unwrap();
        reg.create_group(group(1), 1).unwrap();
        reg.close_group(group(1), false).unwrap();
        assert!(reg.is_empty());
        assert!(reg.storage_root().join("range-1").exists());
        // Reopening must succeed because storage persists.
        reg.open_group(group(1), 1).unwrap();
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn close_group_with_delete_removes_storage() {
        let root = tmpdir();
        let reg = MultiRaftRegistry::new(node_id(1), root.path()).unwrap();
        reg.create_group(group(1), 1).unwrap();
        reg.close_group(group(1), true).unwrap();
        assert!(!reg.storage_root().join("range-1").exists());
    }

    #[test]
    fn single_node_group_can_propose_after_becoming_leader() {
        let root = tmpdir();
        let reg = MultiRaftRegistry::new(node_id(1), root.path()).unwrap();
        reg.create_group(group(1), 1).unwrap();
        reg.become_leader(group(1), &[]).unwrap();
        let idx = reg
            .propose(group(1), RaftCommand::Noop)
            .expect("propose to leader");
        assert!(idx >= 1, "log index assigned");
        let state = reg.group_state(group(1)).unwrap();
        assert_eq!(state.role, RaftRole::Leader);
        // Single-node cluster auto-commits.
        assert!(state.commit_index >= idx, "single-node auto-commits");
    }

    #[test]
    fn propose_to_non_leader_is_rejected() {
        let root = tmpdir();
        let reg = MultiRaftRegistry::new(node_id(1), root.path()).unwrap();
        reg.create_group(group(1), 3).unwrap();
        let err = reg.propose(group(1), RaftCommand::Noop).unwrap_err();
        assert!(err.to_string().contains("not led"), "err: {err}");
    }

    #[test]
    fn groups_are_independent() {
        let root = tmpdir();
        let reg = MultiRaftRegistry::new(node_id(1), root.path()).unwrap();
        reg.create_group(group(1), 1).unwrap();
        reg.create_group(group(2), 1).unwrap();
        reg.become_leader(group(1), &[]).unwrap();
        // Group 2 unchanged.
        let s1 = reg.group_state(group(1)).unwrap();
        let s2 = reg.group_state(group(2)).unwrap();
        assert_eq!(s1.role, RaftRole::Leader);
        assert_eq!(s2.role, RaftRole::Follower);
        // Each group has its own log.
        let idx1 = reg.propose(group(1), RaftCommand::Noop).unwrap();
        assert!(idx1 >= 1);
        let s2_after = reg.group_state(group(2)).unwrap();
        assert_eq!(s2_after.last_log_index, 0, "group 2 log untouched");
    }

    #[test]
    fn snapshot_returns_sorted_groups() {
        let root = tmpdir();
        let reg = MultiRaftRegistry::new(node_id(1), root.path()).unwrap();
        reg.create_group(group(3), 1).unwrap();
        reg.create_group(group(1), 1).unwrap();
        reg.create_group(group(2), 1).unwrap();
        let snap = reg.snapshot();
        let ids: Vec<u64> = snap.iter().map(|s| s.group.get()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn open_group_recovers_persisted_state() {
        let root = tmpdir();
        let reg = MultiRaftRegistry::new(node_id(1), root.path()).unwrap();
        reg.create_group(group(1), 1).unwrap();
        reg.become_leader(group(1), &[]).unwrap();
        let proposed = reg.propose(group(1), RaftCommand::Noop).unwrap();
        reg.close_group(group(1), false).unwrap();

        // Reopen via a fresh registry pointing at the same root.
        let reg2 = MultiRaftRegistry::new(node_id(1), root.path()).unwrap();
        reg2.open_group(group(1), 1).unwrap();
        let state = reg2.group_state(group(1)).unwrap();
        assert!(state.last_log_index >= proposed);
    }
}
