#![allow(
    clippy::items_after_statements,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::redundant_closure_for_method_calls,
    clippy::too_many_lines
)]

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use aiondb_core::{DbError, DbResult};
use tracing::{info, warn};

use crate::election::{ElectionResult, LeaderElection};
use crate::fencing::FencingGuard;
use crate::health::{HealthMonitor, PrimaryHealthStatus};
use crate::protocol::{Epoch, HaMessage, NodeId, NodeRole};
use crate::raft::{AppendEntriesRequest, AppendEntriesResponse, RaftNode};

/// Current state of the failover state machine.
#[derive(Clone, Debug)]
pub enum FailoverState {
    /// No failover activity. Normal operation.
    Idle,
    /// Monitoring primary health, ready to trigger election.
    MonitoringPrimary,
    /// An election is in progress.
    ElectionInProgress { epoch: Epoch, started_at: Instant },
    /// Election won, promotion pending.
    PromotionPending { epoch: Epoch },
    /// This node has been promoted to primary.
    Promoted { epoch: Epoch },
}

/// Events emitted by the failover orchestrator for the engine to act on.
#[derive(Clone, Debug)]
pub enum FailoverEvent {
    /// The primary node is no longer reachable.
    PrimaryLost,
    /// This node won the election and should promote itself.
    ElectionWon { epoch: Epoch },
    /// This node lost the election.
    ElectionLost { epoch: Epoch },
    /// Promotion is complete.
    PromotionComplete { epoch: Epoch },
    /// A new primary was detected (another node won).
    NewPrimaryDetected {
        node_id: NodeId,
        epoch: Epoch,
        addr: String,
    },
    /// Received a message with a higher epoch -- must step down.
    StaleEpochDetected { local: Epoch, remote: Epoch },
    /// Received a Raft `AppendEntries` RPC - caller must dispatch to `RaftNode`.
    RaftAppendEntries { payload: Vec<u8> },
    /// Received a Raft `AppendEntries` response - caller must dispatch to `RaftNode`.
    RaftAppendEntriesResponse { payload: Vec<u8> },
}

/// HA message with an explicit recipient.
///
/// Some Raft replication messages are follower-specific and cannot be broadcast.
#[derive(Clone, Debug)]
pub struct DirectedHaMessage {
    pub target_id: NodeId,
    pub message: HaMessage,
}

/// Orchestrates failover by tying together health monitoring, leader election,
/// and fencing.
pub struct FailoverOrchestrator {
    node_id: NodeId,
    health: Arc<HealthMonitor>,
    election: Arc<LeaderElection>,
    fencing: Arc<FencingGuard>,
    state: RwLock<FailoverState>,
    election_timeout: Duration,
}

impl FailoverOrchestrator {
    pub fn new(
        node_id: NodeId,
        health: Arc<HealthMonitor>,
        election: Arc<LeaderElection>,
        fencing: Arc<FencingGuard>,
        election_timeout: Duration,
    ) -> Self {
        Self {
            node_id,
            health,
            election,
            fencing,
            state: RwLock::new(FailoverState::Idle),
            election_timeout,
        }
    }

    /// Return a clone of the current failover state.
    pub fn state(&self) -> FailoverState {
        self.state.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Process an incoming HA message.
    ///
    /// Returns outgoing messages to send and events for the engine to act on.
    pub fn handle_message(
        &self,
        msg: HaMessage,
        own_lsn: u64,
        own_role: NodeRole,
    ) -> DbResult<(Vec<HaMessage>, Vec<FailoverEvent>)> {
        let mut out_msgs = Vec::new();
        let mut events = Vec::new();

        match msg {
            HaMessage::Heartbeat {
                epoch,
                node_id,
                wal_lsn,
                role,
                ..
            } => {
                self.health.record_heartbeat(node_id, epoch, wal_lsn, role);
                if role == NodeRole::Primary {
                    self.health.set_primary(node_id, epoch);
                }
                let local_epoch = self.health.current_epoch();
                if epoch > local_epoch {
                    self.health.advance_epoch(epoch);
                    self.election.advance_epoch(epoch);
                    events.push(FailoverEvent::StaleEpochDetected {
                        local: local_epoch,
                        remote: epoch,
                    });
                }
                out_msgs.push(HaMessage::HeartbeatAck {
                    epoch: self.health.current_epoch(),
                    node_id: self.node_id,
                    wal_lsn: own_lsn,
                });
            }
            HaMessage::HeartbeatAck {
                epoch,
                node_id,
                wal_lsn,
            } => {
                self.health
                    .record_heartbeat(node_id, epoch, wal_lsn, own_role);
            }
            HaMessage::VoteRequest {
                epoch,
                candidate_id,
                last_lsn,
            } => {
                let response =
                    self.election
                        .handle_vote_request(epoch, candidate_id, last_lsn, own_lsn);
                out_msgs.push(response);
            }
            HaMessage::VoteResponse {
                epoch,
                voter_id,
                granted,
                ..
            } => {
                if let Some(result) = self.election.record_vote(epoch, voter_id, granted) {
                    match result {
                        ElectionResult::Won { epoch } => {
                            info!(
                                node = %self.node_id,
                                epoch = epoch.get(),
                                "election won, acquiring fencing token"
                            );
                            self.fencing.acquire(epoch)?;
                            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
                            *state = FailoverState::PromotionPending { epoch };
                            events.push(FailoverEvent::ElectionWon { epoch });
                        }
                        ElectionResult::Lost { epoch, .. } => {
                            info!(
                                node = %self.node_id,
                                epoch = epoch.get(),
                                "election lost"
                            );
                            events.push(FailoverEvent::ElectionLost { epoch });
                        }
                        ElectionResult::Timeout { .. } | ElectionResult::InsufficientNodes => {}
                    }
                }
            }
            HaMessage::PromoteNotify {
                epoch,
                new_primary_id,
                new_primary_addr,
            } => {
                let local_epoch = self.health.current_epoch();
                if epoch < local_epoch {
                    return Ok((out_msgs, events));
                }
                if !self.health.set_primary(new_primary_id, epoch) {
                    return Ok((out_msgs, events));
                }
                self.health.advance_epoch(epoch);
                self.election.advance_epoch(epoch);
                events.push(FailoverEvent::NewPrimaryDetected {
                    node_id: new_primary_id,
                    epoch,
                    addr: new_primary_addr,
                });
            }
            HaMessage::DemoteRequest { epoch, target_id } => {
                if target_id == self.node_id {
                    let local_epoch = self.health.current_epoch();
                    if epoch < local_epoch {
                        return Ok((out_msgs, events));
                    }
                    if epoch > local_epoch {
                        self.health.advance_epoch(epoch);
                        self.election.advance_epoch(epoch);
                    }
                    warn!(
                        node = %self.node_id,
                        local_epoch = local_epoch.get(),
                        remote_epoch = epoch.get(),
                        "received demote request"
                    );
                    events.push(FailoverEvent::StaleEpochDetected {
                        local: local_epoch,
                        remote: epoch,
                    });
                }
            }
            // Raft AppendEntries messages are dispatched via events to the
            // caller, which owns the RaftNode.
            HaMessage::AppendEntries { payload } => {
                events.push(FailoverEvent::RaftAppendEntries { payload });
            }
            HaMessage::AppendEntriesResponse { payload } => {
                events.push(FailoverEvent::RaftAppendEntriesResponse { payload });
            }
        }

        Ok((out_msgs, events))
    }

    /// Process an incoming HA message and dispatch embedded Raft events
    /// directly into the provided `RaftNode`.
    ///
    /// This consumes `FailoverEvent::RaftAppendEntries*` internally and
    /// returns only non-Raft events to the caller.
    pub fn handle_message_with_raft(
        &self,
        msg: HaMessage,
        own_lsn: u64,
        own_role: NodeRole,
        raft_node: &Mutex<RaftNode>,
    ) -> DbResult<(Vec<HaMessage>, Vec<FailoverEvent>)> {
        let (mut out_msgs, events) = self.handle_message(msg, own_lsn, own_role)?;
        let mut passthrough_events = Vec::new();

        // Tighter inner cap on Raft payloads. The transport already rejects
        // anything > 8 MiB, but a deeply-nested JSON document inside that
        // budget can still drive serde_json into pathological CPU/heap
        // (audit ha F6). 1 MiB is far above any legitimate Raft message.
        const MAX_RAFT_INNER_PAYLOAD_BYTES: usize = 1024 * 1024;
        for event in events {
            match event {
                FailoverEvent::RaftAppendEntries { payload } => {
                    if payload.len() > MAX_RAFT_INNER_PAYLOAD_BYTES {
                        return Err(DbError::internal(format!(
                            "Raft AppendEntries payload {} bytes exceeds {MAX_RAFT_INNER_PAYLOAD_BYTES}",
                            payload.len()
                        )));
                    }
                    let req: AppendEntriesRequest =
                        serde_json::from_slice(&payload).map_err(|e| {
                            DbError::internal(format!("invalid Raft AppendEntries payload: {e}"))
                        })?;
                    let mut node = raft_node
                        .lock()
                        .map_err(|e| DbError::internal(format!("Raft node lock poisoned: {e}")))?;
                    let resp = node.handle_append_entries(&req)?;
                    let payload = serde_json::to_vec(&resp).map_err(|e| {
                        DbError::internal(format!(
                            "failed to serialize Raft AppendEntriesResponse: {e}"
                        ))
                    })?;
                    out_msgs.push(HaMessage::AppendEntriesResponse { payload });
                }
                FailoverEvent::RaftAppendEntriesResponse { payload } => {
                    if payload.len() > MAX_RAFT_INNER_PAYLOAD_BYTES {
                        return Err(DbError::internal(format!(
                            "Raft AppendEntriesResponse payload {} bytes exceeds {MAX_RAFT_INNER_PAYLOAD_BYTES}",
                            payload.len()
                        )));
                    }
                    let resp: AppendEntriesResponse =
                        serde_json::from_slice(&payload).map_err(|e| {
                            DbError::internal(format!(
                                "invalid Raft AppendEntriesResponse payload: {e}"
                            ))
                        })?;
                    let mut node = raft_node
                        .lock()
                        .map_err(|e| DbError::internal(format!("Raft node lock poisoned: {e}")))?;
                    node.handle_append_entries_response(&resp)?;
                }
                other => passthrough_events.push(other),
            }
        }

        Ok((out_msgs, passthrough_events))
    }

    /// Build follower-targeted Raft replication messages from the current
    /// leader state.
    pub fn build_raft_append_entries_messages(
        &self,
        raft_node: &Mutex<RaftNode>,
    ) -> DbResult<Vec<DirectedHaMessage>> {
        let node = raft_node
            .lock()
            .map_err(|e| DbError::internal(format!("Raft node lock poisoned: {e}")))?;
        let requests = node.build_append_entries_requests();
        drop(node);

        requests
            .into_iter()
            .map(|(peer_id, req)| {
                let payload = serde_json::to_vec(&req).map_err(|e| {
                    DbError::internal(format!(
                        "failed to serialize Raft AppendEntries request: {e}"
                    ))
                })?;
                Ok(DirectedHaMessage {
                    target_id: NodeId::new(peer_id),
                    message: HaMessage::AppendEntries { payload },
                })
            })
            .collect()
    }

    /// Periodic tick called by the runtime.
    ///
    /// Returns outgoing messages to send and events for the engine.
    pub fn tick(
        &self,
        own_lsn: u64,
        own_role: NodeRole,
    ) -> DbResult<(Vec<HaMessage>, Vec<FailoverEvent>)> {
        let mut out_msgs = Vec::new();
        let mut events = Vec::new();

        let current_state = self.state();

        match current_state {
            FailoverState::MonitoringPrimary => {
                if own_role == NodeRole::Replica {
                    match self.health.check_primary_health() {
                        PrimaryHealthStatus::Unreachable { .. } => {
                            info!(
                                node = %self.node_id,
                                "primary unreachable, starting election"
                            );
                            let (epoch, vote_req) = self.election.start_election(own_lsn);
                            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
                            *state = FailoverState::ElectionInProgress {
                                epoch,
                                started_at: Instant::now(),
                            };
                            out_msgs.push(vote_req);
                            events.push(FailoverEvent::PrimaryLost);
                        }
                        PrimaryHealthStatus::Healthy | PrimaryHealthStatus::Unknown => {}
                    }
                }
            }
            FailoverState::ElectionInProgress {
                epoch, started_at, ..
            } => {
                if started_at.elapsed() > self.election_timeout {
                    warn!(
                        node = %self.node_id,
                        epoch = epoch.get(),
                        "election timed out"
                    );
                    let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
                    *state = FailoverState::MonitoringPrimary;
                }
            }
            FailoverState::Idle
            | FailoverState::PromotionPending { .. }
            | FailoverState::Promoted { .. } => {}
        }

        out_msgs.push(self.health.create_heartbeat(own_lsn, own_role));

        Ok((out_msgs, events))
    }

    /// Confirm that promotion is complete after the engine has finished
    /// transitioning to primary.
    pub fn confirm_promotion(&self, epoch: Epoch) -> DbResult<()> {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        match *state {
            FailoverState::PromotionPending {
                epoch: pending_epoch,
            } if pending_epoch == epoch => {
                info!(
                    node = %self.node_id,
                    epoch = epoch.get(),
                    "promotion confirmed"
                );
                *state = FailoverState::Promoted { epoch };
                Ok(())
            }
            _ => Err(DbError::internal(format!(
                "cannot confirm promotion: unexpected state for epoch {}",
                epoch.get()
            ))),
        }
    }

    /// Reset to monitoring state (used when this node is a replica).
    pub fn reset_to_monitoring(&self) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        *state = FailoverState::MonitoringPrimary;
    }

    /// Set state to monitoring primary health.
    pub fn set_monitoring(&self) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        *state = FailoverState::MonitoringPrimary;
    }

    /// Set state to idle (used when this node is primary or standalone).
    pub fn set_idle(&self) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        *state = FailoverState::Idle;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::raft::{AppendEntriesRequest, AppendEntriesResponse, RaftCommand, RaftNode};

    fn make_raft_node(node_id: u64, cluster_size: usize) -> RaftNode {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_owned();
        std::mem::forget(dir);
        RaftNode::open(NodeId::new(node_id), cluster_size, path).unwrap()
    }

    fn make_orchestrator() -> FailoverOrchestrator {
        let node_id = NodeId::new(1);
        let health = Arc::new(HealthMonitor::new(node_id, Duration::from_millis(50)));
        let election = Arc::new(LeaderElection::new(node_id, 3));
        let fencing = Arc::new(FencingGuard::new(node_id, None));
        FailoverOrchestrator::new(node_id, health, election, fencing, Duration::from_secs(5))
    }

    #[test]
    fn tick_detects_primary_loss() {
        let orch = make_orchestrator();
        orch.set_monitoring();

        // Register a primary with a heartbeat, then let it expire.
        orch.health.set_primary(NodeId::new(2), Epoch::new(1));
        orch.health
            .record_heartbeat(NodeId::new(2), Epoch::new(1), 500, NodeRole::Primary);

        // Force the primary's heartbeat to appear stale by using a very short
        // timeout (the monitor was created with 50ms).
        std::thread::sleep(Duration::from_millis(100));

        let (msgs, events) = orch.tick(400, NodeRole::Replica).unwrap();

        let has_primary_lost = events
            .iter()
            .any(|e| matches!(e, FailoverEvent::PrimaryLost));
        assert!(has_primary_lost, "expected PrimaryLost event");

        let has_vote_req = msgs
            .iter()
            .any(|m| matches!(m, HaMessage::VoteRequest { .. }));
        assert!(has_vote_req, "expected VoteRequest message");
    }

    #[test]
    fn handle_vote_response_triggers_election_won() {
        let orch = make_orchestrator();
        orch.set_monitoring();

        // Start an election (votes for self).
        let (epoch, _) = orch.election.start_election(1000);

        // Set state to ElectionInProgress.
        {
            let mut state = orch.state.write().unwrap();
            *state = FailoverState::ElectionInProgress {
                epoch,
                started_at: Instant::now(),
            };
        }

        // Receive a granting vote from node 2 (quorum = 2 for cluster of 3).
        let msg = HaMessage::VoteResponse {
            epoch,
            voter_id: NodeId::new(2),
            granted: true,
            voter_lsn: 900,
        };
        let (_msgs, events) = orch.handle_message(msg, 1000, NodeRole::Replica).unwrap();

        let has_won = events
            .iter()
            .any(|e| matches!(e, FailoverEvent::ElectionWon { .. }));
        assert!(has_won, "expected ElectionWon event");

        match orch.state() {
            FailoverState::PromotionPending { epoch: e } => {
                assert_eq!(e, epoch);
            }
            other => panic!("expected PromotionPending, got {other:?}"),
        }
    }

    #[test]
    fn handle_heartbeat_records_primary() {
        let orch = make_orchestrator();

        let msg = HaMessage::Heartbeat {
            epoch: Epoch::new(3),
            node_id: NodeId::new(5),
            wal_lsn: 2000,
            role: NodeRole::Primary,
            timestamp_us: 123_456,
        };
        let (out_msgs, _events) = orch.handle_message(msg, 100, NodeRole::Replica).unwrap();

        let has_ack = out_msgs
            .iter()
            .any(|m| matches!(m, HaMessage::HeartbeatAck { .. }));
        assert!(has_ack, "expected HeartbeatAck response");
    }

    #[test]
    fn handle_demote_request_for_self() {
        let orch = make_orchestrator();
        let msg = HaMessage::DemoteRequest {
            epoch: Epoch::new(10),
            target_id: NodeId::new(1),
        };
        let (_msgs, events) = orch.handle_message(msg, 500, NodeRole::Primary).unwrap();
        let has_stale = events
            .iter()
            .any(|e| matches!(e, FailoverEvent::StaleEpochDetected { .. }));
        assert!(has_stale, "expected StaleEpochDetected event");
        assert_eq!(orch.health.current_epoch(), Epoch::new(10));
    }

    #[test]
    fn handle_stale_demote_request_is_ignored() {
        let orch = make_orchestrator();
        orch.health.advance_epoch(Epoch::new(5));

        let msg = HaMessage::DemoteRequest {
            epoch: Epoch::new(4),
            target_id: NodeId::new(1),
        };
        let (_msgs, events) = orch.handle_message(msg, 500, NodeRole::Primary).unwrap();

        assert!(events.is_empty());
        assert_eq!(orch.health.current_epoch(), Epoch::new(5));
    }

    #[test]
    fn handle_promote_notify() {
        let orch = make_orchestrator();
        let msg = HaMessage::PromoteNotify {
            epoch: Epoch::new(8),
            new_primary_id: NodeId::new(3),
            new_primary_addr: "10.0.0.3:5433".to_string(),
        };
        let (_msgs, events) = orch.handle_message(msg, 100, NodeRole::Replica).unwrap();
        let has_new_primary = events.iter().any(|e| {
            matches!(
                e,
                FailoverEvent::NewPrimaryDetected {
                    node_id,
                    epoch,
                    ..
                } if *node_id == NodeId::new(3) && *epoch == Epoch::new(8)
            )
        });
        assert!(has_new_primary, "expected NewPrimaryDetected event");
    }

    #[test]
    fn handle_stale_promote_notify_is_ignored() {
        let orch = make_orchestrator();
        orch.health.set_primary(NodeId::new(2), Epoch::new(5));
        orch.health
            .record_heartbeat(NodeId::new(2), Epoch::new(5), 100, NodeRole::Primary);

        let msg = HaMessage::PromoteNotify {
            epoch: Epoch::new(4),
            new_primary_id: NodeId::new(3),
            new_primary_addr: "10.0.0.3:5433".to_string(),
        };
        let (_msgs, events) = orch.handle_message(msg, 100, NodeRole::Replica).unwrap();

        assert!(events.is_empty());
        assert!(matches!(
            orch.health.check_primary_health(),
            PrimaryHealthStatus::Healthy
        ));
        assert_eq!(orch.health.current_epoch(), Epoch::new(5));
    }

    #[test]
    fn handle_conflicting_same_epoch_promote_notify_is_ignored() {
        let orch = make_orchestrator();
        orch.health.set_primary(NodeId::new(2), Epoch::new(5));
        orch.health
            .record_heartbeat(NodeId::new(2), Epoch::new(5), 100, NodeRole::Primary);

        let msg = HaMessage::PromoteNotify {
            epoch: Epoch::new(5),
            new_primary_id: NodeId::new(3),
            new_primary_addr: "10.0.0.3:5433".to_string(),
        };
        let (_msgs, events) = orch.handle_message(msg, 100, NodeRole::Replica).unwrap();

        assert!(events.is_empty());
        assert!(matches!(
            orch.health.check_primary_health(),
            PrimaryHealthStatus::Healthy
        ));
    }

    #[test]
    fn confirm_promotion_transitions_state() {
        let orch = make_orchestrator();
        {
            let mut state = orch.state.write().unwrap();
            *state = FailoverState::PromotionPending {
                epoch: Epoch::new(5),
            };
        }
        orch.confirm_promotion(Epoch::new(5)).unwrap();
        match orch.state() {
            FailoverState::Promoted { epoch } => assert_eq!(epoch, Epoch::new(5)),
            other => panic!("expected Promoted, got {other:?}"),
        }
    }

    #[test]
    fn confirm_promotion_wrong_epoch_fails() {
        let orch = make_orchestrator();
        {
            let mut state = orch.state.write().unwrap();
            *state = FailoverState::PromotionPending {
                epoch: Epoch::new(5),
            };
        }
        assert!(orch.confirm_promotion(Epoch::new(99)).is_err());
    }

    #[test]
    fn set_idle_and_monitoring() {
        let orch = make_orchestrator();
        orch.set_monitoring();
        assert!(matches!(orch.state(), FailoverState::MonitoringPrimary));
        orch.set_idle();
        assert!(matches!(orch.state(), FailoverState::Idle));
    }

    #[test]
    fn handle_message_with_raft_dispatches_append_entries() {
        let orch = make_orchestrator();
        let raft = Mutex::new(make_raft_node(1, 3));
        let req = AppendEntriesRequest {
            term: 1,
            leader_id: 2,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: Vec::new(),
            leader_commit: 0,
        };
        let payload = serde_json::to_vec(&req).unwrap();

        let (out_msgs, events) = orch
            .handle_message_with_raft(
                HaMessage::AppendEntries { payload },
                0,
                NodeRole::Replica,
                &raft,
            )
            .unwrap();

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FailoverEvent::RaftAppendEntries { .. })),
            "Raft append event should be consumed by handle_message_with_raft"
        );

        let response_payload = out_msgs
            .into_iter()
            .find_map(|m| match m {
                HaMessage::AppendEntriesResponse { payload } => Some(payload),
                _ => None,
            })
            .expect("expected AppendEntriesResponse output");
        let response: AppendEntriesResponse = serde_json::from_slice(&response_payload).unwrap();
        assert!(response.success);
        assert_eq!(response.node_id, 1);
    }

    #[test]
    fn handle_message_with_raft_dispatches_append_entries_response() {
        let orch = make_orchestrator();
        let raft = Mutex::new(make_raft_node(1, 3));
        {
            let mut node = raft.lock().unwrap();
            node.become_candidate().unwrap();
            node.become_leader(&[2, 3]).unwrap();
            node.propose(RaftCommand::Noop).unwrap();
            assert_eq!(node.commit_index(), 0);
        }

        let response = AppendEntriesResponse {
            term: 1,
            node_id: 2,
            success: true,
            match_index: 1,
        };
        let payload = serde_json::to_vec(&response).unwrap();

        let (_out_msgs, events) = orch
            .handle_message_with_raft(
                HaMessage::AppendEntriesResponse { payload },
                0,
                NodeRole::Replica,
                &raft,
            )
            .unwrap();

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FailoverEvent::RaftAppendEntriesResponse { .. })),
            "Raft append response event should be consumed by handle_message_with_raft"
        );

        let node = raft.lock().unwrap();
        assert_eq!(node.commit_index(), 1);
    }

    #[test]
    fn build_raft_append_entries_messages_returns_targeted_messages() {
        let orch = make_orchestrator();
        let raft = Mutex::new(make_raft_node(1, 3));
        {
            let mut node = raft.lock().unwrap();
            node.become_candidate().unwrap();
            node.become_leader(&[2, 3]).unwrap();
            node.propose(RaftCommand::Noop).unwrap();
        }

        let msgs = orch.build_raft_append_entries_messages(&raft).unwrap();
        assert_eq!(msgs.len(), 2);

        let targets: HashSet<u64> = msgs.iter().map(|m| m.target_id.get()).collect();
        assert!(targets.contains(&2));
        assert!(targets.contains(&3));
        for msg in msgs {
            assert!(matches!(msg.message, HaMessage::AppendEntries { .. }));
        }
    }
}
