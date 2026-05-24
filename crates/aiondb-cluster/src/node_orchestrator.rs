//! Single-process node orchestrator.
//!
//! Bundles the distributed stack on one node : gossip, multi-raft,
//! KV engine, distrib metrics, and the HTTP metrics endpoint. Tests
//! and the binary `aiondb-server` both consume the orchestrator to
//! spin up a fully-wired node with one constructor call.
//!
//! Responsibilities :
//!
//! 1. Bind gossip + raft + metrics listeners on configured addresses.
//! 2. Hold the `MultiRaftRegistry` rooted at a per-node state dir.
//! 3. Expose handles so higher layers (SQL, ingest, query) can plug
//!    into the same KV engine + control plane.
//! 4. Provide a graceful `shutdown()` that drains every listener and
//!    persists in-flight state.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aiondb_core::DbResult;
use aiondb_ha::distrib_metrics::DistribMetrics;
use aiondb_ha::kv_engine::KvEngine;
use aiondb_ha::metrics_server::MetricsServer;
use aiondb_ha::multi_raft::MultiRaftRegistry;
use aiondb_ha::protocol::NodeId as RaftNodeId;
use aiondb_ha::raft_auth::RaftSharedSecret;
use aiondb_ha::raft_control_plane::{RaftControlPlane, DEFAULT_METADATA_GROUP_ID};
use aiondb_ha::raft_tcp::RaftTcpServer;

use crate::distributed::NodeId as ClusterNodeId;
use crate::gossip::{GossipConfig, GossipNode};
use crate::gossip_transport::GossipServer;

/// Configuration for a single orchestrator node.
#[derive(Clone)]
pub struct NodeOrchestratorConfig {
    pub raft_node_id: u64,
    pub cluster_node_id: String,
    pub state_dir: PathBuf,
    pub gossip_bind: SocketAddr,
    pub gossip_tick: Duration,
    pub raft_bind: SocketAddr,
    pub metrics_bind: Option<SocketAddr>,
    pub metadata_voters: usize,
    pub metadata_peer_ids: Vec<u64>,
    /// When `true`, make the local node the metadata group leader
    /// after start. Set on the bootstrap node only.
    pub bootstrap_metadata_leader: bool,
    /// HMAC-SHA256 shared secret authenticating every Raft TCP frame.
    /// Must match across every node in the cluster ; must be at least
    /// 32 bytes. Sourced from cluster bootstrap material — never from
    /// SQL or wire input.
    pub raft_shared_secret: Vec<u8>,
}

impl std::fmt::Debug for NodeOrchestratorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeOrchestratorConfig")
            .field("raft_node_id", &self.raft_node_id)
            .field("cluster_node_id", &self.cluster_node_id)
            .field("state_dir", &self.state_dir)
            .field("gossip_bind", &self.gossip_bind)
            .field("gossip_tick", &self.gossip_tick)
            .field("raft_bind", &self.raft_bind)
            .field("metrics_bind", &self.metrics_bind)
            .field("metadata_voters", &self.metadata_voters)
            .field("metadata_peer_ids", &self.metadata_peer_ids)
            .field("bootstrap_metadata_leader", &self.bootstrap_metadata_leader)
            .field(
                "raft_shared_secret",
                &(!self.raft_shared_secret.is_empty()).then_some("<redacted>"),
            )
            .finish()
    }
}

impl NodeOrchestratorConfig {
    pub fn single_node_local(raft_id: u64) -> Self {
        Self {
            raft_node_id: raft_id,
            cluster_node_id: format!("n{raft_id}"),
            state_dir: PathBuf::from("./aiondb-state"),
            gossip_bind: "127.0.0.1:0".parse().unwrap(),
            gossip_tick: Duration::from_millis(500),
            raft_bind: "127.0.0.1:0".parse().unwrap(),
            metrics_bind: Some("127.0.0.1:0".parse().unwrap()),
            metadata_voters: 1,
            metadata_peer_ids: Vec::new(),
            bootstrap_metadata_leader: true,
            // Deterministic 32-byte test secret for single-node setups.
            // Production NodeOrchestratorConfig instances must override
            // this with secret bytes from the cluster bootstrap.
            raft_shared_secret: vec![0u8; 32],
        }
    }
}

/// Live node orchestrator.
pub struct NodeOrchestrator {
    pub raft_id: RaftNodeId,
    pub cluster_id: ClusterNodeId,
    pub gossip: Arc<GossipNode>,
    pub gossip_server: GossipServer,
    pub raft_registry: Arc<MultiRaftRegistry>,
    pub raft_server: RaftTcpServer,
    pub control_plane: RaftControlPlane,
    pub kv: KvEngine,
    pub metrics: Arc<DistribMetrics>,
    pub metrics_server: Option<MetricsServer>,
}

impl NodeOrchestrator {
    /// Boot every subsystem in dependency order.
    pub async fn start(config: NodeOrchestratorConfig) -> DbResult<Self> {
        std::fs::create_dir_all(&config.state_dir)
            .map_err(|e| aiondb_core::DbError::internal(format!("state_dir create: {e}")))?;
        let raft_id = RaftNodeId::new(config.raft_node_id);
        let cluster_id = ClusterNodeId::new(config.cluster_node_id.clone());

        // Gossip
        let gossip = Arc::new(GossipNode::new(cluster_id.clone(), gossip_config(&config)));
        let cluster_secret = RaftSharedSecret::new(config.raft_shared_secret.clone());
        let gossip_server = GossipServer::start(
            Arc::clone(&gossip),
            config.gossip_bind,
            config.gossip_tick,
            cluster_secret,
        )
        .await
        .map_err(|e| aiondb_core::DbError::internal(format!("gossip bind: {e}")))?;

        // Multi-Raft registry rooted at <state_dir>/raft/.
        let raft_root = config.state_dir.join("raft");
        std::fs::create_dir_all(&raft_root)
            .map_err(|e| aiondb_core::DbError::internal(format!("raft dir: {e}")))?;
        let raft_registry = Arc::new(MultiRaftRegistry::new(raft_id, &raft_root)?);
        let raft_secret = RaftSharedSecret::new(config.raft_shared_secret.clone());
        let raft_server =
            RaftTcpServer::start(Arc::clone(&raft_registry), config.raft_bind, raft_secret)
                .await
                .map_err(|e| aiondb_core::DbError::internal(format!("raft bind: {e}")))?;

        // Control plane (metadata) + KV engine on the same registry.
        let control_plane = RaftControlPlane::new(Arc::clone(&raft_registry));
        let kv = KvEngine::new(Arc::clone(&raft_registry));
        if config.bootstrap_metadata_leader {
            control_plane.bootstrap_leader(config.metadata_voters, &config.metadata_peer_ids)?;
        } else {
            // Open the metadata group as a follower so apply_committed
            // works once Raft delivers entries.
            match raft_registry.open_group(
                aiondb_ha::multi_raft::MultiRaftGroupId::new(DEFAULT_METADATA_GROUP_ID),
                config.metadata_voters,
            ) {
                Ok(_) => {}
                Err(err) if err.to_string().contains("no on-disk state") => {
                    raft_registry.create_group(
                        aiondb_ha::multi_raft::MultiRaftGroupId::new(DEFAULT_METADATA_GROUP_ID),
                        config.metadata_voters,
                    )?;
                }
                Err(other) => return Err(other),
            }
        }

        // Metrics
        let metrics = Arc::new(DistribMetrics::new(Arc::clone(&raft_registry), kv.clone()));
        let metrics_server = match config.metrics_bind {
            Some(bind) => Some(
                MetricsServer::start(Arc::clone(&metrics), bind)
                    .await
                    .map_err(|e| aiondb_core::DbError::internal(format!("metrics bind: {e}")))?,
            ),
            None => None,
        };

        Ok(Self {
            raft_id,
            cluster_id,
            gossip,
            gossip_server,
            raft_registry,
            raft_server,
            control_plane,
            kv,
            metrics,
            metrics_server,
        })
    }

    pub fn gossip_addr(&self) -> SocketAddr {
        self.gossip_server.local_addr()
    }

    pub fn raft_addr(&self) -> SocketAddr {
        self.raft_server.local_addr()
    }

    pub fn metrics_addr(&self) -> Option<SocketAddr> {
        self.metrics_server.as_ref().map(|s| s.local_addr())
    }

    /// Register a peer in both gossip and raft.
    pub async fn register_peer(
        &self,
        raft_id: u64,
        cluster_id: ClusterNodeId,
        gossip_addr: SocketAddr,
        raft_addr: SocketAddr,
    ) {
        self.gossip_server
            .register_peer(cluster_id.clone(), gossip_addr)
            .await;
        self.gossip.join(cluster_id, BTreeMap::new());
        self.raft_server.register_peer(raft_id, raft_addr).await;
    }

    pub async fn shutdown(self) {
        self.gossip_server.shutdown().await;
        self.raft_server.shutdown().await;
        if let Some(server) = self.metrics_server {
            server.shutdown().await;
        }
    }
}

fn gossip_config(_cfg: &NodeOrchestratorConfig) -> GossipConfig {
    GossipConfig {
        protocol_period: Duration::from_millis(500),
        ack_timeout: Duration::from_millis(200),
        suspect_timeout: Duration::from_secs(5),
        indirect_probes: 2,
        piggyback_size: 8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_orchestrator_config_debug_redacts_shared_secret() {
        let mut cfg = NodeOrchestratorConfig::single_node_local(1);
        cfg.raft_shared_secret = b"super-secret-cluster-hmac-key-32b".to_vec();

        let debug = format!("{cfg:?}");

        assert!(!debug.contains("super-secret-cluster-hmac-key-32b"));
        assert!(debug.contains("redacted"));
    }

    #[tokio::test]
    async fn single_node_starts_and_shuts_down() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = NodeOrchestratorConfig::single_node_local(1);
        cfg.state_dir = tmp.path().to_path_buf();
        let node = NodeOrchestrator::start(cfg).await.unwrap();
        assert!(node.metrics_addr().is_some());
        assert!(node.gossip_addr().port() > 0);
        assert!(node.raft_addr().port() > 0);
        // KV should be ready to accept writes.
        node.kv
            .put(
                aiondb_ha::multi_raft::MultiRaftGroupId::new(7),
                b"k".to_vec(),
                b"v".to_vec(),
            )
            .err(); // group 7 not created yet -> err expected
        node.shutdown().await;
    }

    #[tokio::test]
    async fn metrics_endpoint_reachable_after_start() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = NodeOrchestratorConfig::single_node_local(1);
        cfg.state_dir = tmp.path().to_path_buf();
        let node = NodeOrchestrator::start(cfg).await.unwrap();
        let addr = node.metrics_addr().unwrap();
        // Open the endpoint over TCP to confirm it accepts connections.
        let _ = tokio::net::TcpStream::connect(addr).await.unwrap();
        node.shutdown().await;
    }

    #[tokio::test]
    async fn control_plane_accepts_writes_after_bootstrap() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = NodeOrchestratorConfig::single_node_local(1);
        cfg.state_dir = tmp.path().to_path_buf();
        let node = NodeOrchestrator::start(cfg).await.unwrap();
        node.control_plane.add_node(99, "host-99").unwrap();
        let snap = node.control_plane.snapshot();
        assert_eq!(snap.members.len(), 1);
        assert_eq!(snap.members[0].node_id, 99);
        node.shutdown().await;
    }
}
