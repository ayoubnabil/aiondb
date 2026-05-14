//! Raft-backed distributed control plane.
//!
//! Replaces the in-memory `BTreeMap` placeholder with a real
//! consensus-replicated metadata store. Every mutation (`AddNode`,
//! `AssignShard`, `TransferShard`, ...) is proposed through a single
//! dedicated [`MultiRaftRegistry`] group; once the group commits the
//! entry, the local snapshot applies it and the change becomes
//! observable to readers.
//!
//! # Why a dedicated group?
//!
//! Cluster metadata changes are rare relative to data traffic. Putting
//! them in their own Raft group:
//!
//! - Decouples the size + replication factor of metadata from any
//!   user table.
//! - Lets the metadata group live on a small, stable set of voters
//!   (typically 3-5) while user ranges scale independently.
//! - Matches CockroachDB's `liveness` + `meta1` design.
//!
//! # Semantics
//!
//! - Writes : `propose_*` returns once the local Raft node has assigned
//!   a log index. The change is visible to local reads after the next
//!   `apply_committed` pass (or immediately when single-node since
//!   single-node Raft auto-commits).
//! - Reads : observe the local snapshot, which is monotone-consistent
//!   with the leader's commit. Followers may lag by up to one
//!   round-trip; callers needing strict linearisability should route
//!   reads to the leader or use the closed-timestamp horizon.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use aiondb_core::DbResult;

use crate::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use crate::protocol::NodeId;
use crate::raft::{RaftCommand, RaftEntry, RaftRole};

/// Reserved group id for the metadata Raft. A real deployment can
/// override via [`RaftControlPlane::with_group_id`] but most clusters
/// stick with the default.
pub const DEFAULT_METADATA_GROUP_ID: u64 = 0;

/// One known cluster member.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClusterMember {
    pub node_id: u64,
    pub address: String,
}

/// One shard assignment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShardAssignment {
    pub table_id: u64,
    pub shard_id: u32,
    pub node_id: u64,
}

/// Committed cluster metadata snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClusterSnapshot {
    pub members: Vec<ClusterMember>,
    pub assignments: Vec<ShardAssignment>,
    pub config: BTreeMap<String, String>,
    /// Highest Raft log index reflected in this snapshot.
    pub applied_index: u64,
}

#[derive(Debug, Default)]
struct ControlState {
    members: BTreeMap<u64, ClusterMember>,
    assignments: BTreeMap<(u64, u32), ShardAssignment>,
    config: BTreeMap<String, String>,
    applied_index: u64,
}

impl ControlState {
    fn apply(&mut self, entry: &RaftEntry) {
        let new_index = entry.index;
        match &entry.command {
            RaftCommand::Noop => {}
            RaftCommand::AddNode { node_id, address } => {
                self.members.insert(
                    *node_id,
                    ClusterMember {
                        node_id: *node_id,
                        address: address.clone(),
                    },
                );
            }
            RaftCommand::RemoveNode { node_id } => {
                self.members.remove(node_id);
                self.assignments.retain(|_, a| a.node_id != *node_id);
            }
            RaftCommand::AssignShard {
                table_id,
                shard_id,
                node_id,
            } => {
                self.assignments.insert(
                    (*table_id, *shard_id),
                    ShardAssignment {
                        table_id: *table_id,
                        shard_id: *shard_id,
                        node_id: *node_id,
                    },
                );
            }
            RaftCommand::TransferShard {
                table_id,
                shard_id,
                to_node,
                ..
            } => {
                if let Some(slot) = self.assignments.get_mut(&(*table_id, *shard_id)) {
                    slot.node_id = *to_node;
                }
            }
            RaftCommand::UpdateConfig { key, value } => {
                self.config.insert(key.clone(), value.clone());
            }
            RaftCommand::KvWrite { .. } => {
                // The control plane snapshot does not store user KV
                // data -- that lives in [`crate::kv_engine::KvEngine`].
                // We simply advance applied_index below so multi-raft
                // does not redeliver the entry.
            }
        }
        if new_index > self.applied_index {
            self.applied_index = new_index;
        }
    }

    fn snapshot(&self) -> ClusterSnapshot {
        let mut members: Vec<_> = self.members.values().cloned().collect();
        members.sort_by_key(|m| m.node_id);
        let mut assignments: Vec<_> = self.assignments.values().copied().collect();
        assignments.sort_by_key(|a| (a.table_id, a.shard_id));
        ClusterSnapshot {
            members,
            assignments,
            config: self.config.clone(),
            applied_index: self.applied_index,
        }
    }
}

/// Raft-backed control plane handle.
#[derive(Clone, Debug)]
pub struct RaftControlPlane {
    registry: Arc<MultiRaftRegistry>,
    group: MultiRaftGroupId,
    state: Arc<RwLock<ControlState>>,
}

impl RaftControlPlane {
    pub fn new(registry: Arc<MultiRaftRegistry>) -> Self {
        Self::with_group_id(registry, DEFAULT_METADATA_GROUP_ID)
    }

    pub fn with_group_id(registry: Arc<MultiRaftRegistry>, group_id: u64) -> Self {
        Self {
            registry,
            group: MultiRaftGroupId::new(group_id),
            state: Arc::new(RwLock::new(ControlState::default())),
        }
    }

    pub fn group(&self) -> MultiRaftGroupId {
        self.group
    }

    /// Initialise the metadata group with `cluster_size` voters and
    /// promote the local node to leader so it can accept proposals.
    /// In a real multi-node cluster the leader is determined by
    /// election; this helper exists for bootstrap + single-node use.
    pub fn bootstrap_leader(&self, cluster_size: usize, peer_ids: &[u64]) -> DbResult<()> {
        match self.registry.create_group(self.group, cluster_size) {
            Ok(_) => {}
            Err(err) if err.to_string().contains("already exists") => {}
            Err(err) => return Err(err),
        }
        self.registry.become_leader(self.group, peer_ids)?;
        self.apply_committed()?;
        Ok(())
    }

    pub fn metadata_storage_dir(&self) -> std::path::PathBuf {
        self.registry.group_storage_dir(self.group)
    }

    pub fn storage_root(&self) -> &Path {
        self.registry.storage_root()
    }

    /// Propose `AddNode`. Returns the assigned log index.
    pub fn add_node(&self, node_id: u64, address: impl Into<String>) -> DbResult<u64> {
        self.propose(RaftCommand::AddNode {
            node_id,
            address: address.into(),
        })
    }

    pub fn remove_node(&self, node_id: u64) -> DbResult<u64> {
        self.propose(RaftCommand::RemoveNode { node_id })
    }

    pub fn assign_shard(&self, table_id: u64, shard_id: u32, node_id: u64) -> DbResult<u64> {
        self.propose(RaftCommand::AssignShard {
            table_id,
            shard_id,
            node_id,
        })
    }

    pub fn transfer_shard(
        &self,
        table_id: u64,
        shard_id: u32,
        from_node: u64,
        to_node: u64,
    ) -> DbResult<u64> {
        self.propose(RaftCommand::TransferShard {
            table_id,
            shard_id,
            from_node,
            to_node,
        })
    }

    pub fn set_config(&self, key: impl Into<String>, value: impl Into<String>) -> DbResult<u64> {
        self.propose(RaftCommand::UpdateConfig {
            key: key.into(),
            value: value.into(),
        })
    }

    /// Apply every committed-but-unapplied Raft entry to the local
    /// snapshot. Returns the new `applied_index`.
    pub fn apply_committed(&self) -> DbResult<u64> {
        let unapplied = self.registry.unapplied_entries(self.group)?;
        if unapplied.is_empty() {
            return Ok(self.state.read().unwrap().applied_index);
        }
        let mut state = self.state.write().unwrap();
        let mut last_index = state.applied_index;
        for entry in &unapplied {
            state.apply(entry);
            last_index = entry.index;
        }
        drop(state);
        self.registry.mark_applied(self.group, last_index)?;
        Ok(last_index)
    }

    /// Local consistent read.
    pub fn snapshot(&self) -> ClusterSnapshot {
        self.state.read().unwrap().snapshot()
    }

    pub fn members(&self) -> Vec<ClusterMember> {
        self.snapshot().members
    }

    pub fn assignments(&self) -> Vec<ShardAssignment> {
        self.snapshot().assignments
    }

    pub fn config(&self) -> BTreeMap<String, String> {
        self.snapshot().config
    }

    pub fn role(&self) -> RaftRole {
        self.registry
            .group_state(self.group)
            .map(|s| s.role)
            .unwrap_or(RaftRole::Follower)
    }

    pub fn local_node_id(&self) -> NodeId {
        self.registry.local_node_id()
    }

    fn propose(&self, command: RaftCommand) -> DbResult<u64> {
        let idx = self.registry.propose(self.group, command)?;
        // For single-node clusters the entry is already committed; for
        // multi-node clusters the apply loop runs after AppendEntries
        // replies converge.
        self.apply_committed()?;
        Ok(idx)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn fresh() -> (tempfile::TempDir, RaftControlPlane) {
        let tmp = tmpdir();
        let registry = MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap();
        let cp = RaftControlPlane::new(Arc::new(registry));
        cp.bootstrap_leader(1, &[]).unwrap();
        (tmp, cp)
    }

    #[test]
    fn add_then_read_member() {
        let (_g, cp) = fresh();
        cp.add_node(7, "127.0.0.1:7000").unwrap();
        let snap = cp.snapshot();
        assert_eq!(snap.members.len(), 1);
        assert_eq!(snap.members[0].node_id, 7);
        assert_eq!(snap.members[0].address, "127.0.0.1:7000");
    }

    #[test]
    fn remove_member_clears_their_assignments() {
        let (_g, cp) = fresh();
        cp.add_node(7, "127.0.0.1:7000").unwrap();
        cp.add_node(8, "127.0.0.1:7001").unwrap();
        cp.assign_shard(1, 0, 7).unwrap();
        cp.assign_shard(1, 1, 8).unwrap();
        cp.remove_node(7).unwrap();
        let snap = cp.snapshot();
        assert_eq!(snap.members.len(), 1);
        assert!(snap.members.iter().all(|m| m.node_id != 7));
        // Assignments for node 7 must be gone.
        assert!(snap.assignments.iter().all(|a| a.node_id != 7));
    }

    #[test]
    fn transfer_updates_assignment_target() {
        let (_g, cp) = fresh();
        cp.add_node(7, "a").unwrap();
        cp.add_node(8, "b").unwrap();
        cp.assign_shard(1, 0, 7).unwrap();
        cp.transfer_shard(1, 0, 7, 8).unwrap();
        let snap = cp.snapshot();
        let assignment = snap
            .assignments
            .iter()
            .find(|a| a.table_id == 1 && a.shard_id == 0)
            .unwrap();
        assert_eq!(assignment.node_id, 8);
    }

    #[test]
    fn config_is_persisted_to_snapshot() {
        let (_g, cp) = fresh();
        cp.set_config("replication_factor", "3").unwrap();
        cp.set_config("region", "eu").unwrap();
        let cfg = cp.config();
        assert_eq!(cfg.get("replication_factor"), Some(&"3".to_owned()));
        assert_eq!(cfg.get("region"), Some(&"eu".to_owned()));
    }

    #[test]
    fn applied_index_advances_with_proposals() {
        let (_g, cp) = fresh();
        let i1 = cp.add_node(1, "a").unwrap();
        let i2 = cp.add_node(2, "b").unwrap();
        assert!(i2 > i1);
        let snap = cp.snapshot();
        assert!(snap.applied_index >= i2);
    }

    #[test]
    fn role_is_leader_after_bootstrap() {
        let (_g, cp) = fresh();
        assert_eq!(cp.role(), RaftRole::Leader);
    }

    #[test]
    fn replays_state_from_persisted_log() {
        let tmp = tmpdir();
        // Phase 1: write some state via the first control plane.
        {
            let registry = MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap();
            let cp = RaftControlPlane::new(Arc::new(registry));
            cp.bootstrap_leader(1, &[]).unwrap();
            cp.add_node(7, "host-a").unwrap();
            cp.assign_shard(10, 0, 7).unwrap();
        }
        // Phase 2: fresh control plane reads the same on-disk log and
        // catches up via apply_committed.
        let registry = MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap();
        registry
            .open_group(MultiRaftGroupId::new(DEFAULT_METADATA_GROUP_ID), 1)
            .unwrap();
        // Promote so the local node can apply.
        registry
            .become_leader(MultiRaftGroupId::new(DEFAULT_METADATA_GROUP_ID), &[])
            .unwrap();
        let cp = RaftControlPlane::new(Arc::new(registry));
        cp.apply_committed().unwrap();
        let snap = cp.snapshot();
        assert_eq!(snap.members.len(), 1);
        assert_eq!(snap.members[0].node_id, 7);
        assert_eq!(snap.assignments.len(), 1);
        assert_eq!(snap.assignments[0].node_id, 7);
    }

    #[test]
    fn propose_to_non_leader_fails() {
        let tmp = tmpdir();
        let registry = MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap();
        let cp = RaftControlPlane::new(Arc::new(registry));
        cp.registry
            .create_group(cp.group(), 3) // 3-voter cluster
            .unwrap();
        // We deliberately did NOT call become_leader -- local node is a
        // Follower. Proposing must fail.
        assert!(cp.add_node(1, "a").is_err());
    }
}
