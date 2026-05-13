use std::path::PathBuf;
use std::sync::Arc;

use aiondb_config::{ReplicationRole, RuntimeConfig};
use aiondb_engine::engine::ha::HaIntegration;
use aiondb_engine::engine::streaming::StreamingReplicationState;
use aiondb_engine::{DbError, DbResult};
use aiondb_ha::{
    AppendEntriesRequest, AppendEntriesResponse, DirectedHaMessage, FailoverEvent,
    FailoverOrchestrator, FencingGuard, HaMessage, HealthMonitor, LeaderElection, NodeId, NodeRole,
    RaftRole,
};
use tracing::debug;

use super::algorithm::{AlgorithmContext, HaAlgorithm, OutboundMessage};

const MAX_RAFT_INNER_PAYLOAD_BYTES: usize = 1024 * 1024;

pub(crate) struct RaftHaAlgorithm {
    node_id: NodeId,
    self_addr: String,
    orchestrator: FailoverOrchestrator,
    ha_integration: HaIntegration,
    raft_peer_ids: Vec<u64>,
}

impl RaftHaAlgorithm {
    pub(crate) fn new(
        replication_state: Arc<StreamingReplicationState>,
        config: &RuntimeConfig,
        node_id: NodeId,
        self_addr: String,
        cluster_size: usize,
        raft_peer_ids: Vec<u64>,
    ) -> DbResult<Self> {
        let health = Arc::new(HealthMonitor::new(node_id, config.ha.health_check_timeout));
        let election = Arc::new(LeaderElection::new(node_id, cluster_size));
        let fencing = Arc::new(FencingGuard::new(
            node_id,
            config.ha.fencing_token_path.as_ref().map(PathBuf::from),
        ));
        let orchestrator = FailoverOrchestrator::new(
            node_id,
            health,
            election,
            fencing,
            config.ha.election_timeout,
        );

        let raft_state_dir = config
            .storage
            .data_dir
            .join("ha")
            .join(format!("node-{}", config.ha.node_id));
        std::fs::create_dir_all(&raft_state_dir).map_err(|error| {
            DbError::internal(format!(
                "failed to create HA state directory {}: {error}",
                raft_state_dir.display()
            ))
        })?;

        let ha_integration =
            HaIntegration::with_raft(replication_state, node_id, cluster_size, raft_state_dir)?;

        Ok(Self {
            node_id,
            self_addr,
            orchestrator,
            ha_integration,
            raft_peer_ids,
        })
    }

    fn bootstrap_raft_role(&mut self, current_role: ReplicationRole) -> DbResult<()> {
        let Some(raft) = self.ha_integration.raft_node() else {
            return Ok(());
        };

        let mut node = raft
            .lock()
            .map_err(|error| DbError::internal(format!("Raft lock poisoned: {error}")))?;
        match (current_role, node.role()) {
            (ReplicationRole::Primary, RaftRole::Leader) => {}
            (ReplicationRole::Primary, _) => {
                node.become_candidate()?;
                node.become_leader(&self.raft_peer_ids)?;
            }
            (_, RaftRole::Leader) => {
                let term = node.current_term();
                node.become_follower(term)?;
            }
            _ => {}
        }

        Ok(())
    }

    fn apply_event(&mut self, event: FailoverEvent) -> DbResult<Vec<OutboundMessage>> {
        let mut out = Vec::new();
        match event {
            FailoverEvent::PrimaryLost => {
                debug!(node_id = self.node_id.get(), "HA detected primary loss");
            }
            FailoverEvent::ElectionWon { epoch } => {
                if self.ha_integration.current_role() != ReplicationRole::Primary {
                    self.ha_integration.promote()?;
                }
                if let Err(error) = self.orchestrator.confirm_promotion(epoch) {
                    debug!(%error, epoch = epoch.get(), "failed to confirm promotion state");
                }
                self.orchestrator.set_idle();
                self.bootstrap_raft_role(ReplicationRole::Primary)?;
                out.push(OutboundMessage::Broadcast(HaMessage::PromoteNotify {
                    epoch,
                    new_primary_id: self.node_id,
                    new_primary_addr: self.self_addr.clone(),
                }));
            }
            FailoverEvent::ElectionLost { epoch } => {
                debug!(epoch = epoch.get(), "HA election lost");
                self.orchestrator.set_monitoring();
            }
            FailoverEvent::PromotionComplete { epoch } => {
                debug!(epoch = epoch.get(), "HA promotion complete event received");
            }
            FailoverEvent::NewPrimaryDetected { node_id, epoch, .. } => {
                if node_id != self.node_id
                    && self.ha_integration.current_role() == ReplicationRole::Primary
                {
                    self.ha_integration.demote()?;
                }
                debug!(
                    new_primary = node_id.get(),
                    epoch = epoch.get(),
                    "HA observed new primary"
                );
                self.orchestrator.set_monitoring();
                self.bootstrap_raft_role(ReplicationRole::Replica)?;
            }
            FailoverEvent::StaleEpochDetected { local, remote } => {
                if remote > local && self.ha_integration.current_role() == ReplicationRole::Primary
                {
                    self.ha_integration.demote()?;
                    self.orchestrator.set_monitoring();
                    self.bootstrap_raft_role(ReplicationRole::Replica)?;
                }
            }
            FailoverEvent::RaftAppendEntries { payload } => {
                ensure_raft_payload_len("Raft AppendEntries", &payload)?;
                let request: AppendEntriesRequest =
                    serde_json::from_slice(&payload).map_err(|error| {
                        DbError::internal(format!("invalid Raft AppendEntries payload: {error}"))
                    })?;
                let response = self.ha_integration.raft_handle_append_entries(&request)?;
                let response_payload = serde_json::to_vec(&response).map_err(|error| {
                    DbError::internal(format!(
                        "failed to encode Raft AppendEntries response: {error}"
                    ))
                })?;
                out.push(OutboundMessage::Target(
                    NodeId::new(request.leader_id),
                    HaMessage::AppendEntriesResponse {
                        payload: response_payload,
                    },
                ));
            }
            FailoverEvent::RaftAppendEntriesResponse { payload } => {
                ensure_raft_payload_len("Raft AppendEntriesResponse", &payload)?;
                let response: AppendEntriesResponse =
                    serde_json::from_slice(&payload).map_err(|error| {
                        DbError::internal(format!(
                            "invalid Raft AppendEntriesResponse payload: {error}"
                        ))
                    })?;
                self.ha_integration
                    .raft_handle_append_entries_response(&response)?;
            }
        }
        Ok(out)
    }
}

pub(crate) fn build(context: AlgorithmContext<'_>) -> DbResult<Box<dyn HaAlgorithm>> {
    Ok(Box::new(RaftHaAlgorithm::new(
        context.replication_state,
        context.config,
        context.node_id,
        context.self_addr,
        context.cluster_size,
        context.peer_ids,
    )?))
}

impl HaAlgorithm for RaftHaAlgorithm {
    fn name(&self) -> &'static str {
        "raft"
    }

    fn bootstrap(&mut self, current_role: ReplicationRole) -> DbResult<()> {
        match current_role {
            ReplicationRole::Replica => self.orchestrator.set_monitoring(),
            ReplicationRole::Primary | ReplicationRole::Standalone => self.orchestrator.set_idle(),
        }
        self.bootstrap_raft_role(current_role)
    }

    fn on_message(
        &mut self,
        message: HaMessage,
        own_lsn: u64,
        own_role: NodeRole,
    ) -> DbResult<Vec<OutboundMessage>> {
        let reply_target = sender_hint(&message)?;
        let (outgoing, events) = if let Some(raft) = self.ha_integration.raft_node() {
            self.orchestrator
                .handle_message_with_raft(message, own_lsn, own_role, raft)?
        } else {
            self.orchestrator
                .handle_message(message, own_lsn, own_role)?
        };

        let mut out = Vec::new();
        for message in outgoing {
            out.push(route_outbound(message, reply_target));
        }
        for event in events {
            out.extend(self.apply_event(event)?);
        }
        Ok(out)
    }

    fn on_tick(&mut self, own_lsn: u64, own_role: NodeRole) -> DbResult<Vec<OutboundMessage>> {
        let (outgoing, events) = self.orchestrator.tick(own_lsn, own_role)?;

        let mut out = outgoing
            .into_iter()
            .map(OutboundMessage::Broadcast)
            .collect::<Vec<_>>();

        for event in events {
            out.extend(self.apply_event(event)?);
        }

        if let Some(raft) = self.ha_integration.raft_node() {
            let directed = self.orchestrator.build_raft_append_entries_messages(raft)?;
            for DirectedHaMessage { target_id, message } in directed {
                out.push(OutboundMessage::Target(target_id, message));
            }
        }

        Ok(out)
    }
}

fn sender_hint(message: &HaMessage) -> DbResult<Option<NodeId>> {
    let hint = match message {
        HaMessage::Heartbeat { node_id, .. } => Some(*node_id),
        HaMessage::HeartbeatAck { node_id, .. } => Some(*node_id),
        HaMessage::VoteRequest { candidate_id, .. } => Some(*candidate_id),
        HaMessage::VoteResponse { voter_id, .. } => Some(*voter_id),
        HaMessage::PromoteNotify { new_primary_id, .. } => Some(*new_primary_id),
        HaMessage::DemoteRequest { target_id, .. } => Some(*target_id),
        HaMessage::AppendEntries { payload } => {
            ensure_raft_payload_len("Raft AppendEntries", payload)?;
            let request: AppendEntriesRequest =
                serde_json::from_slice(payload).map_err(|error| {
                    DbError::internal(format!(
                        "invalid Raft AppendEntries payload for sender hint: {error}"
                    ))
                })?;
            Some(NodeId::new(request.leader_id))
        }
        HaMessage::AppendEntriesResponse { payload } => {
            ensure_raft_payload_len("Raft AppendEntriesResponse", payload)?;
            let response: AppendEntriesResponse =
                serde_json::from_slice(payload).map_err(|error| {
                    DbError::internal(format!(
                        "invalid Raft AppendEntriesResponse payload for sender hint: {error}"
                    ))
                })?;
            Some(NodeId::new(response.node_id))
        }
    };
    Ok(hint)
}

fn ensure_raft_payload_len(kind: &str, payload: &[u8]) -> DbResult<()> {
    if payload.len() > MAX_RAFT_INNER_PAYLOAD_BYTES {
        return Err(DbError::internal(format!(
            "{kind} payload {} bytes exceeds {MAX_RAFT_INNER_PAYLOAD_BYTES}",
            payload.len()
        )));
    }
    Ok(())
}

fn route_outbound(message: HaMessage, reply_target: Option<NodeId>) -> OutboundMessage {
    let targeted_reply = matches!(
        message,
        HaMessage::HeartbeatAck { .. }
            | HaMessage::VoteResponse { .. }
            | HaMessage::AppendEntriesResponse { .. }
    );

    if targeted_reply {
        if let Some(target) = reply_target {
            OutboundMessage::Target(target, message)
        } else {
            OutboundMessage::Broadcast(message)
        }
    } else {
        OutboundMessage::Broadcast(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_ha::Epoch;

    #[test]
    fn route_outbound_targets_ack_to_sender_hint() {
        let outbound = route_outbound(
            HaMessage::HeartbeatAck {
                epoch: Epoch::new(1),
                node_id: NodeId::new(1),
                wal_lsn: 42,
            },
            Some(NodeId::new(9)),
        );

        match outbound {
            OutboundMessage::Target(id, HaMessage::HeartbeatAck { .. }) => {
                assert_eq!(id, NodeId::new(9));
            }
            other => panic!("expected targeted HeartbeatAck, got {other:?}"),
        }
    }

    #[test]
    fn route_outbound_broadcasts_when_reply_target_missing() {
        let outbound = route_outbound(
            HaMessage::VoteResponse {
                epoch: Epoch::new(2),
                voter_id: NodeId::new(2),
                granted: true,
                voter_lsn: 11,
            },
            None,
        );

        assert!(matches!(
            outbound,
            OutboundMessage::Broadcast(HaMessage::VoteResponse { .. })
        ));
    }

    #[test]
    fn sender_hint_rejects_oversized_raft_payload() {
        let message = HaMessage::AppendEntries {
            payload: vec![b' '; MAX_RAFT_INNER_PAYLOAD_BYTES + 1],
        };

        let error = sender_hint(&message).expect_err("oversized Raft payload must be rejected");
        assert!(error.to_string().contains("exceeds"));
    }
}
