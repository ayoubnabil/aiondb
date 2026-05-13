//! Engine-level high-availability integration.
//!
//! Bridges the [`FailoverOrchestrator`](aiondb_ha::FailoverOrchestrator) with
//! the engine's [`StreamingReplicationState`]
//! to enable runtime role transitions during automatic failover.

#![allow(clippy::doc_markdown, clippy::missing_errors_doc)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aiondb_config::ReplicationRole;
use aiondb_core::DbResult;
use aiondb_ha::raft::{
    AppendEntriesRequest, AppendEntriesResponse, RaftCommand, RaftNode, RaftRole,
};
use aiondb_ha::NodeId;
use tracing::info;

use super::streaming::StreamingReplicationState;

/// Handles failover events by driving role transitions on the engine's
/// streaming replication state, and manages the Raft consensus node for
/// distributed metadata coordination.
pub struct HaIntegration {
    replication_state: Arc<StreamingReplicationState>,
    /// Raft consensus node for cluster metadata (None when HA is disabled).
    raft_node: Option<Mutex<RaftNode>>,
}

impl HaIntegration {
    /// Create a new HA integration bound to the given replication state.
    pub fn new(replication_state: Arc<StreamingReplicationState>) -> Self {
        Self {
            replication_state,
            raft_node: None,
        }
    }

    /// Create HA integration with a Raft consensus node.
    pub fn with_raft(
        replication_state: Arc<StreamingReplicationState>,
        node_id: NodeId,
        cluster_size: usize,
        state_dir: PathBuf,
    ) -> DbResult<Self> {
        let raft = RaftNode::open(node_id, cluster_size, state_dir)?;
        info!(
            node_id = node_id.get(),
            cluster_size, "HA integration with Raft consensus initialized"
        );
        Ok(Self {
            replication_state,
            raft_node: Some(Mutex::new(raft)),
        })
    }

    /// Return a reference to the Raft node, if configured.
    pub fn raft_node(&self) -> Option<&Mutex<RaftNode>> {
        self.raft_node.as_ref()
    }

    /// Propose a Raft command (leader only).
    pub fn raft_propose(&self, command: RaftCommand) -> DbResult<u64> {
        let Some(raft) = &self.raft_node else {
            return Err(aiondb_core::DbError::feature_not_supported(
                "Raft consensus is not enabled",
            ));
        };
        let mut node = raft
            .lock()
            .map_err(|e| aiondb_core::DbError::internal(format!("Raft lock poisoned: {e}")))?;
        node.propose(command)
    }

    /// Handle an incoming Raft AppendEntries RPC.
    pub fn raft_handle_append_entries(
        &self,
        req: &AppendEntriesRequest,
    ) -> DbResult<AppendEntriesResponse> {
        let Some(raft) = &self.raft_node else {
            return Err(aiondb_core::DbError::feature_not_supported(
                "Raft consensus is not enabled",
            ));
        };
        let mut node = raft
            .lock()
            .map_err(|e| aiondb_core::DbError::internal(format!("Raft lock poisoned: {e}")))?;
        node.handle_append_entries(req)
    }

    /// Handle an incoming Raft AppendEntries response.
    pub fn raft_handle_append_entries_response(
        &self,
        resp: &AppendEntriesResponse,
    ) -> DbResult<()> {
        let Some(raft) = &self.raft_node else {
            return Ok(());
        };
        let mut node = raft
            .lock()
            .map_err(|e| aiondb_core::DbError::internal(format!("Raft lock poisoned: {e}")))?;
        node.handle_append_entries_response(resp)
    }

    /// Return the current Raft role, if configured.
    pub fn raft_role(&self) -> Option<RaftRole> {
        let raft = self.raft_node.as_ref()?;
        let node = raft.lock().ok()?;
        Some(node.role())
    }

    /// Promote this node from replica to primary.
    ///
    /// Called when this node wins a leader election. The caller is responsible
    /// for acquiring the fencing token before calling this method.
    pub fn promote(&self) -> DbResult<()> {
        info!("HA: initiating promotion to primary");
        self.replication_state.promote_to_primary()?;
        info!("HA: promotion to primary complete");
        Ok(())
    }

    /// Demote this node from primary to replica.
    ///
    /// Called when a higher-epoch primary is detected (split-brain resolution)
    /// or when the node is explicitly demoted.
    pub fn demote(&self) -> DbResult<()> {
        info!("HA: initiating demotion to replica");
        self.replication_state.demote_to_replica()?;
        info!("HA: demotion to replica complete");
        Ok(())
    }

    /// Transition this node to standalone mode.
    ///
    /// Used when HA is disabled or the node leaves the cluster.
    pub fn transition_to_standalone(&self) {
        info!("HA: transitioning to standalone mode");
        self.replication_state.transition_to_standalone();
    }

    /// Return the current replication role.
    pub fn current_role(&self) -> ReplicationRole {
        self.replication_state.role()
    }

    /// Return a reference to the underlying replication state.
    pub fn replication_state(&self) -> &Arc<StreamingReplicationState> {
        &self.replication_state
    }
}

impl std::fmt::Debug for HaIntegration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HaIntegration")
            .field("role", &self.current_role())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_config::ReplicationConfig;

    #[test]
    fn promote_replica_to_primary() {
        let config = ReplicationConfig::default();
        let state = Arc::new(StreamingReplicationState::new_primary(
            crate::test_support::unique_temp_path("engine-ha-tests", "promote-replica"),
            config,
        ));
        state.demote_to_replica().unwrap();
        assert_eq!(state.role(), ReplicationRole::Replica);
        assert!(state.is_read_only());

        let ha = HaIntegration::new(state.clone());
        ha.promote().unwrap();
        assert_eq!(state.role(), ReplicationRole::Primary);
        assert!(!state.is_read_only());
    }

    #[test]
    fn demote_primary_to_replica() {
        let config = ReplicationConfig::default();
        let state = Arc::new(StreamingReplicationState::new_primary(
            crate::test_support::unique_temp_path("engine-ha-tests", "demote-primary"),
            config,
        ));
        assert_eq!(state.role(), ReplicationRole::Primary);
        assert!(!state.is_read_only());

        let ha = HaIntegration::new(state.clone());
        ha.demote().unwrap();
        assert_eq!(state.role(), ReplicationRole::Replica);
        assert!(state.is_read_only());
    }

    #[test]
    fn promote_non_replica_fails() {
        let state = Arc::new(StreamingReplicationState::standalone());
        let ha = HaIntegration::new(state);
        assert!(ha.promote().is_err());
    }

    #[test]
    fn demote_non_primary_fails() {
        let state = Arc::new(StreamingReplicationState::standalone());
        let ha = HaIntegration::new(state);
        assert!(ha.demote().is_err());
    }

    #[test]
    fn transition_to_standalone_from_any_role() {
        let config = ReplicationConfig::default();
        let state = Arc::new(StreamingReplicationState::new_primary(
            crate::test_support::unique_temp_path("engine-ha-tests", "to-standalone"),
            config,
        ));
        let ha = HaIntegration::new(state.clone());
        ha.transition_to_standalone();
        assert_eq!(state.role(), ReplicationRole::Standalone);
        assert!(!state.is_read_only());
    }
}
