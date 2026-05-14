mod algorithm;
mod raft;
mod registry;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aiondb_config::{pgwire::split_listen_addr, ReplicationRole, RuntimeConfig};
use aiondb_engine::engine::streaming::ReplicationManager;
use aiondb_engine::{DatabaseId, DbError, DbResult, Engine, QueryEngine};
use aiondb_ha::{decode_authenticated, encode_authenticated, HaMessage, NodeId, NodeRole};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{interval, timeout};
use tracing::{info, warn};

use self::algorithm::{AlgorithmContext, HaAlgorithm, OutboundMessage};

const HA_NETWORK_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_HA_FRAME_BYTES: usize = 10 * 1024 * 1024;
const MIN_HA_AUTH_TOKEN_BYTES: usize = 32;

struct HaRuntime {
    engine: Arc<Engine>,
    node_id: NodeId,
    peer_addrs: HashMap<NodeId, String>,
    auth_secret: Vec<u8>,
    replication_manager: Arc<ReplicationManager>,
    health_check_interval: Duration,
    distributed_replication_maintenance_enabled: bool,
    algorithm: Box<dyn HaAlgorithm>,
}

pub async fn init_ha_runtime(
    engine: Arc<Engine>,
    config: &RuntimeConfig,
    shutdown_rx: watch::Receiver<bool>,
) -> DbResult<Option<JoinHandle<()>>> {
    if !config.ha.enabled {
        return Ok(None);
    }

    let runtime = HaRuntime::new(engine, config)?;
    let listen_addr = ha_listen_addr(config);
    preflight_ha_bind(&listen_addr).await?;
    let listener = TcpListener::bind(&listen_addr).await.map_err(|error| {
        DbError::internal(format!(
            "failed to bind HA listener on {listen_addr}: {error}"
        ))
    })?;

    info!(addr = %listen_addr, algorithm = runtime.algorithm.name(), "starting HA runtime listener");
    Ok(Some(tokio::spawn(async move {
        runtime.run(listener, shutdown_rx).await;
    })))
}

fn ha_listen_addr(config: &RuntimeConfig) -> String {
    let (bind_host, _) = split_listen_addr(&config.pgwire.listen_addr);
    if bind_host.contains(':') && !bind_host.starts_with('[') {
        format!("[{bind_host}]:{}", config.ha.ha_port)
    } else {
        format!("{bind_host}:{}", config.ha.ha_port)
    }
}

async fn preflight_ha_bind(listen_addr: &str) -> DbResult<()> {
    let listener = tokio::net::TcpListener::bind(listen_addr)
        .await
        .map_err(|error| {
            DbError::internal(format!(
                "failed to initialize HA listener on {listen_addr}: {error}"
            ))
        })?;
    drop(listener);
    Ok(())
}

impl HaRuntime {
    fn new(engine: Arc<Engine>, config: &RuntimeConfig) -> DbResult<Self> {
        let cluster_size = config.ha.cluster_nodes.len();
        if cluster_size == 0 {
            return Err(DbError::internal(
                "HA requires at least 1 entry in AIONDB_HA_CLUSTER_NODES",
            ));
        }

        let peer_addrs = resolve_peer_addrs(&config.ha.cluster_nodes)?;
        let node_id = NodeId::new(config.ha.node_id);
        let self_addr = peer_addrs
            .get(&node_id)
            .cloned()
            .ok_or_else(|| {
                DbError::internal(format!(
                    "AIONDB_HA_NODE_ID={} is out of range for {} configured cluster nodes (index is 1-based)",
                    config.ha.node_id,
                    cluster_size
                ))
            })?;

        let replication_manager = engine.replication_manager().ok_or_else(|| {
            DbError::feature_not_supported(
                "HA is enabled but replication manager is unavailable for this engine",
            )
        })?;
        let replication_state = replication_manager.state().clone();
        let current_role = replication_state.role();
        if current_role == ReplicationRole::Standalone {
            return Err(DbError::feature_not_supported(
                "HA requires replication.role to be primary or replica (standalone is not valid)",
            ));
        }

        let raft_peer_ids = peer_addrs
            .keys()
            .filter(|id| **id != node_id)
            .map(|id| id.get())
            .collect::<Vec<_>>();

        let algorithm_name = registry::selected_algorithm_name();
        let registration = registry::resolve_algorithm(&algorithm_name)?;
        let mut algorithm = (registration.build)(AlgorithmContext {
            replication_state,
            config,
            node_id,
            self_addr,
            cluster_size,
            peer_ids: raft_peer_ids,
        })?;
        algorithm.bootstrap(current_role)?;

        Ok(Self {
            engine,
            node_id,
            peer_addrs,
            auth_secret: ha_auth_secret(config)?,
            replication_manager,
            health_check_interval: config.ha.health_check_interval,
            distributed_replication_maintenance_enabled: config.distributed.sharding.enabled
                && config.distributed.sharding.auto_rebalance,
            algorithm,
        })
    }

    async fn run(mut self, listener: TcpListener, mut shutdown_rx: watch::Receiver<bool>) {
        let mut ticker = interval(self.health_check_interval);

        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        info!(algorithm = self.algorithm.name(), "HA runtime shutdown requested");
                        break;
                    }
                }
                _ = ticker.tick() => {
                    if let Err(error) = self.on_tick().await {
                        warn!(algorithm = self.algorithm.name(), %error, "HA runtime tick failed");
                    }
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, peer_addr)) => {
                            if let Err(error) = self.on_connection(stream).await {
                                warn!(peer = %peer_addr, algorithm = self.algorithm.name(), %error, "HA inbound message handling failed");
                            }
                        }
                        Err(error) => {
                            warn!(algorithm = self.algorithm.name(), %error, "HA listener accept failed");
                        }
                    }
                }
            }
        }
    }

    async fn on_connection(&mut self, mut stream: TcpStream) -> DbResult<()> {
        let mut bytes = Vec::new();
        let mut limited_stream = (&mut stream).take((MAX_HA_FRAME_BYTES + 1) as u64);
        timeout(HA_NETWORK_TIMEOUT, limited_stream.read_to_end(&mut bytes))
            .await
            .map_err(|_| DbError::internal("HA inbound read timed out"))
            .and_then(|result| {
                result
                    .map_err(|error| DbError::internal(format!("HA inbound read failed: {error}")))
            })?;

        if bytes.is_empty() {
            return Ok(());
        }
        if bytes.len() > MAX_HA_FRAME_BYTES {
            return Err(DbError::internal(format!(
                "HA inbound payload too large ({} bytes)",
                bytes.len()
            )));
        }

        let mut cursor = 0usize;
        while cursor < bytes.len() {
            let (message, consumed) = self.decode_message(&bytes[cursor..])?;
            if consumed == 0 {
                return Err(DbError::internal(
                    "HA decoder consumed zero bytes; refusing to loop forever",
                ));
            }
            cursor += consumed;
            let own_lsn = self.current_lsn();
            let own_role = self.current_node_role();
            let outgoing = self.algorithm.on_message(message, own_lsn, own_role)?;
            self.dispatch_all(outgoing).await;
        }

        Ok(())
    }

    fn decode_message(&self, data: &[u8]) -> DbResult<(HaMessage, usize)> {
        decode_authenticated(data, &self.auth_secret)
    }

    fn encode_message(&self, message: &HaMessage) -> DbResult<Vec<u8>> {
        encode_authenticated(message, &self.auth_secret)
    }

    async fn on_tick(&mut self) -> DbResult<()> {
        let own_lsn = self.current_lsn();
        let own_role = self.current_node_role();
        let outgoing = self.algorithm.on_tick(own_lsn, own_role)?;
        self.dispatch_all(outgoing).await;
        self.maintain_distributed_replication_from_primary_progress(own_lsn, own_role)?;
        Ok(())
    }

    fn maintain_distributed_replication_from_primary_progress(
        &self,
        own_lsn: u64,
        own_role: NodeRole,
    ) -> DbResult<()> {
        if !self.distributed_replication_maintenance_enabled
            || !matches!(own_role, NodeRole::Primary)
        {
            return Ok(());
        }

        let outcome = self
            .engine
            .maintain_distributed_replication_from_config_with_primary_progress(
                DatabaseId::DEFAULT,
                own_lsn,
            )?;
        if !outcome.replica_repairs.is_empty() || !outcome.leadership_transfers.is_empty() {
            info!(
                replica_repairs = outcome.replica_repairs.len(),
                leadership_transfers = outcome.leadership_transfers.len(),
                target_apply_lsn = own_lsn,
                "HA tick applied distributed replication maintenance from primary progress"
            );
        }
        Ok(())
    }

    fn current_node_role(&self) -> NodeRole {
        match self.replication_manager.state().role() {
            ReplicationRole::Primary => NodeRole::Primary,
            ReplicationRole::Replica => NodeRole::Replica,
            ReplicationRole::Standalone => NodeRole::Standalone,
        }
    }

    fn current_lsn(&self) -> u64 {
        let state = self.replication_manager.state();
        match state.role() {
            ReplicationRole::Primary => state.wal_notifier().current_lsn().get(),
            ReplicationRole::Replica => state
                .wal_receiver()
                .map(|receiver| receiver.flush_lsn().get())
                .unwrap_or(0),
            ReplicationRole::Standalone => 0,
        }
    }

    async fn dispatch_all(&mut self, messages: Vec<OutboundMessage>) {
        for message in messages {
            self.dispatch(message).await;
        }
    }

    async fn dispatch(&mut self, outbound: OutboundMessage) {
        match outbound {
            OutboundMessage::Broadcast(message) => {
                let bytes = match self.encode_message(&message) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        warn!(%error, "HA broadcast encode failed");
                        return;
                    }
                };
                for (peer_id, addr) in &self.peer_addrs {
                    if *peer_id == self.node_id {
                        continue;
                    }
                    if let Err(error) = Self::send_bytes(addr, &bytes).await {
                        warn!(target_id = peer_id.get(), addr = %addr, %error, "HA send failed");
                    }
                }
            }
            OutboundMessage::Target(target_id, message) => {
                if target_id == self.node_id {
                    return;
                }
                let Some(addr) = self.peer_addrs.get(&target_id) else {
                    warn!(
                        target_id = target_id.get(),
                        "HA message target not present in configured cluster map"
                    );
                    return;
                };
                let bytes = match self.encode_message(&message) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        warn!(target_id = target_id.get(), %error, "HA targeted encode failed");
                        return;
                    }
                };
                if let Err(error) = Self::send_bytes(addr, &bytes).await {
                    warn!(target_id = target_id.get(), addr = %addr, %error, "HA targeted send failed");
                }
            }
        }
    }

    async fn send_bytes(addr: &str, bytes: &[u8]) -> DbResult<()> {
        let mut stream = timeout(HA_NETWORK_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| DbError::internal(format!("HA connect timeout to {addr}")))
            .and_then(|result| {
                result.map_err(|error| {
                    DbError::internal(format!("HA connect to {addr} failed: {error}"))
                })
            })?;

        timeout(HA_NETWORK_TIMEOUT, stream.write_all(bytes))
            .await
            .map_err(|_| DbError::internal(format!("HA write timeout to {addr}")))
            .and_then(|result| {
                result.map_err(|error| {
                    DbError::internal(format!("HA write to {addr} failed: {error}"))
                })
            })?;

        let _ = stream.shutdown().await;
        Ok(())
    }
}

fn resolve_peer_addrs(cluster_nodes: &[String]) -> DbResult<HashMap<NodeId, String>> {
    let mut map = HashMap::with_capacity(cluster_nodes.len());
    for (idx, raw_addr) in cluster_nodes.iter().enumerate() {
        let addr = raw_addr.trim();
        if addr.is_empty() {
            return Err(DbError::internal(
                "AIONDB_HA_CLUSTER_NODES contains an empty address entry",
            ));
        }
        let id = u64::try_from(idx + 1)
            .map_err(|error| DbError::internal(format!("cluster node index overflow: {error}")))?;
        map.insert(NodeId::new(id), addr.to_owned());
    }
    Ok(map)
}

fn ha_auth_secret(config: &RuntimeConfig) -> DbResult<Vec<u8>> {
    let token = config
        .ha
        .inter_node_auth_token
        .as_deref()
        .map(str::trim)
        .ok_or_else(|| DbError::invalid_authorization("HA auth token must be configured"))?;
    if token.is_empty() {
        return Err(DbError::invalid_authorization(
            "HA auth token must not be empty",
        ));
    }
    if token.len() < MIN_HA_AUTH_TOKEN_BYTES {
        return Err(DbError::invalid_authorization(format!(
            "HA auth token must be at least {MIN_HA_AUTH_TOKEN_BYTES} bytes"
        )));
    }
    Ok(token.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_peer_addrs_assigns_one_based_node_ids() {
        let map = resolve_peer_addrs(&["127.0.0.1:6001".to_owned(), "127.0.0.1:6002".to_owned()])
            .expect("peer map");

        assert_eq!(
            map.get(&NodeId::new(1)).map(String::as_str),
            Some("127.0.0.1:6001")
        );
        assert_eq!(
            map.get(&NodeId::new(2)).map(String::as_str),
            Some("127.0.0.1:6002")
        );
    }

    #[test]
    fn ha_auth_secret_requires_configured_token() {
        let config = RuntimeConfig::default();
        assert!(ha_auth_secret(&config).is_err());
    }

    #[test]
    fn ha_auth_secret_rejects_short_token() {
        let mut config = RuntimeConfig::default();
        config.ha.inter_node_auth_token = Some("too-short".to_owned());
        assert!(ha_auth_secret(&config).is_err());
    }

    #[test]
    fn ha_auth_secret_accepts_minimum_length_token() {
        let mut config = RuntimeConfig::default();
        config.ha.inter_node_auth_token = Some("  0123456789abcdef0123456789abcdef  ".to_owned());
        assert_eq!(
            ha_auth_secret(&config).expect("valid token"),
            b"0123456789abcdef0123456789abcdef".to_vec()
        );
    }
}
