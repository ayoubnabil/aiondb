use std::sync::Arc;

use aiondb_config::{ReplicationRole, RuntimeConfig};
use aiondb_engine::engine::streaming::StreamingReplicationState;
use aiondb_engine::DbResult;
use aiondb_ha::{HaMessage, NodeId, NodeRole};

/// Network action emitted by an HA algorithm implementation.
#[derive(Clone, Debug)]
pub(crate) enum OutboundMessage {
    Broadcast(HaMessage),
    Target(NodeId, HaMessage),
}

/// Extensible HA algorithm contract.
///
/// Runtime networking and framing stay outside the algorithm implementation,
/// so new algorithms can focus on state transitions and message semantics.
pub(crate) trait HaAlgorithm: Send {
    /// Stable algorithm identifier (for diagnostics).
    fn name(&self) -> &'static str;

    /// Called once during runtime initialization.
    fn bootstrap(&mut self, current_role: ReplicationRole) -> DbResult<()>;

    /// Process one inbound HA message.
    fn on_message(
        &mut self,
        message: HaMessage,
        own_lsn: u64,
        own_role: NodeRole,
    ) -> DbResult<Vec<OutboundMessage>>;

    /// Process one periodic timer tick.
    fn on_tick(&mut self, own_lsn: u64, own_role: NodeRole) -> DbResult<Vec<OutboundMessage>>;
}

/// Construction inputs shared by all HA algorithm implementations.
pub(crate) struct AlgorithmContext<'a> {
    pub(crate) replication_state: Arc<StreamingReplicationState>,
    pub(crate) config: &'a RuntimeConfig,
    pub(crate) node_id: NodeId,
    pub(crate) self_addr: String,
    pub(crate) cluster_size: usize,
    pub(crate) peer_ids: Vec<u64>,
}

pub(crate) type AlgorithmBuilder =
    for<'a> fn(AlgorithmContext<'a>) -> DbResult<Box<dyn HaAlgorithm>>;

/// Static algorithm registration entry used by the runtime registry.
#[derive(Debug)]
pub(crate) struct AlgorithmRegistration {
    pub(crate) name: &'static str,
    pub(crate) build: AlgorithmBuilder,
}
