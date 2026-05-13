use std::time::Duration;

use aiondb_shard::ShardingConfig;

use crate::{
    ha::HaConfig, pgwire::PgWireConfig, replication::ReplicationConfig, security::SecurityConfig,
    storage::StorageConfig,
};

pub const DEFAULT_LIMITS_STATEMENT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_LIMITS_LOCK_TIMEOUT: Duration = Duration::from_secs(1);
pub const DEFAULT_LIMITS_MAX_RESULT_ROWS: u64 = 10_000;
pub const DEFAULT_LIMITS_MAX_RESULT_BYTES: u64 = 8 * 1024 * 1024;
pub const DEFAULT_LIMITS_MAX_MEMORY_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_LIMITS_MAX_TEMP_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_LIMITS_MAX_PARALLEL_WORKERS_PER_QUERY: usize = 1;
pub const DEFAULT_LIMITS_MAX_PORTALS: usize = 64;
pub const DEFAULT_LIMITS_MAX_PREPARED_STATEMENTS: usize = 128;
pub const DEFAULT_LIMITS_MAX_RECURSIVE_ITERATIONS: usize = 10_000;
pub const DEFAULT_LIMITS_MAX_RECURSIVE_ROWS: usize = 1_000_000;
pub const DEFAULT_ENGINE_POOL_WORKER_THREADS: usize = 4;

/// Computes the default number of pgwire engine pool worker slots.
///
/// Sized to match the host's logical CPU count so concurrent client
/// connections don't serialize behind a fixed-small pool. Falls back to
/// [`DEFAULT_ENGINE_POOL_WORKER_THREADS`] when the platform refuses to
/// report parallelism.
pub fn default_engine_pool_worker_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(DEFAULT_ENGINE_POOL_WORKER_THREADS)
        .max(DEFAULT_ENGINE_POOL_WORKER_THREADS)
}
pub const DEFAULT_ENGINE_POOL_QUEUE_DEPTH: usize = 1024;
pub const DEFAULT_DISTRIBUTED_ALLOW_UNREGISTERED_LOOPBACK_NODES: bool = false;
pub const DEFAULT_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_FAILURE_THRESHOLD: u32 = 5;
pub const DEFAULT_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_RESET_TIMEOUT: Duration =
    Duration::from_secs(30);
pub const DEFAULT_DISTRIBUTED_REMOTE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_DISTRIBUTED_REMOTE_MAX_RETRIES: u32 = 3;
pub const DEFAULT_DISTRIBUTED_REMOTE_RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// Configuration for a remote execution node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteNodeConfig {
    /// Unique node identifier used as fragment target.
    pub node_id: String,
    /// Network address in `host:port` format.
    pub addr: String,
}

/// Snapshot policy for remote fragment execution.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RemoteSnapshotMode {
    /// Execute remote fragments against each remote node's latest visible
    /// committed state. This is the default because TxnIds are not yet global
    /// across independent AionDB nodes.
    #[default]
    LatestVisible,
    /// Send the coordinator's MVCC snapshot to remote nodes. Use only when the
    /// cluster guarantees comparable TxnIds or a global transaction timeline.
    Coordinator,
}

impl RemoteSnapshotMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "latest_visible" | "latest-visible" | "latest" => Some(Self::LatestVisible),
            "coordinator" | "coordinator_snapshot" | "coordinator-snapshot" => {
                Some(Self::Coordinator)
            }
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LatestVisible => "latest_visible",
            Self::Coordinator => "coordinator",
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct DistributedConfig {
    pub loopback_remote_nodes: Vec<String>,
    pub allow_unregistered_loopback_nodes: bool,
    /// Remote nodes for actual distributed execution (host:port).
    pub remote_nodes: Vec<RemoteNodeConfig>,
    /// Shared secret for inter-node fragment transport authentication.
    pub inter_node_auth_token: Option<String>,
    /// Path to PEM-encoded TLS certificate for inter-node connections.
    pub tls_cert_path: Option<String>,
    /// Path to PEM-encoded TLS private key for inter-node connections.
    pub tls_key_path: Option<String>,
    /// Path to PEM-encoded CA certificate for verifying peer nodes.
    /// When set, enables mutual TLS (mTLS) for inter-node traffic.
    pub tls_ca_cert_path: Option<String>,
    /// Whether to require TLS for inter-node fragment transport.
    /// Defaults to `true` - plaintext is rejected when remote nodes are configured.
    pub require_tls: bool,
    /// Port on which this node listens for fragment execution requests.
    pub fragment_transport_port: u16,
    /// Snapshot policy used for remote fragment execution.
    pub remote_snapshot_mode: RemoteSnapshotMode,
    /// Consecutive remote dispatch failures before opening a node circuit.
    pub remote_circuit_breaker_failure_threshold: u32,
    /// Duration to keep a failed remote node circuit open before probing.
    pub remote_circuit_breaker_reset_timeout: Duration,
    /// Timeout for establishing a remote fragment transport connection.
    pub remote_connect_timeout: Duration,
    /// Number of retry attempts after the initial remote connection attempt.
    pub remote_max_retries: u32,
    /// Base retry backoff for remote fragment transport connection attempts.
    pub remote_retry_backoff: Duration,
    /// Sharding configuration (consistent hashing + custom shard keys).
    pub sharding: ShardingConfig,
}

impl std::fmt::Debug for DistributedConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DistributedConfig")
            .field("loopback_remote_nodes", &self.loopback_remote_nodes)
            .field(
                "allow_unregistered_loopback_nodes",
                &self.allow_unregistered_loopback_nodes,
            )
            .field("remote_nodes", &self.remote_nodes)
            .field(
                "inter_node_auth_token",
                &self.inter_node_auth_token.as_ref().map(|_| "<redacted>"),
            )
            .field("fragment_transport_port", &self.fragment_transport_port)
            .field("tls_cert_path", &self.tls_cert_path)
            .field("tls_key_path", &self.tls_key_path)
            .field("tls_ca_cert_path", &self.tls_ca_cert_path)
            .field("require_tls", &self.require_tls)
            .field("remote_snapshot_mode", &self.remote_snapshot_mode.as_str())
            .field(
                "remote_circuit_breaker_failure_threshold",
                &self.remote_circuit_breaker_failure_threshold,
            )
            .field(
                "remote_circuit_breaker_reset_timeout",
                &self.remote_circuit_breaker_reset_timeout,
            )
            .field("remote_connect_timeout", &self.remote_connect_timeout)
            .field("remote_max_retries", &self.remote_max_retries)
            .field("remote_retry_backoff", &self.remote_retry_backoff)
            .field("sharding", &self.sharding)
            .finish()
    }
}

impl Default for DistributedConfig {
    fn default() -> Self {
        Self {
            loopback_remote_nodes: Vec::new(),
            allow_unregistered_loopback_nodes:
                DEFAULT_DISTRIBUTED_ALLOW_UNREGISTERED_LOOPBACK_NODES,
            remote_nodes: Vec::new(),
            inter_node_auth_token: None,
            tls_cert_path: None,
            tls_key_path: None,
            tls_ca_cert_path: None,
            require_tls: true,
            fragment_transport_port: 5434,
            remote_snapshot_mode: RemoteSnapshotMode::default(),
            remote_circuit_breaker_failure_threshold:
                DEFAULT_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_FAILURE_THRESHOLD,
            remote_circuit_breaker_reset_timeout:
                DEFAULT_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_RESET_TIMEOUT,
            remote_connect_timeout: DEFAULT_DISTRIBUTED_REMOTE_CONNECT_TIMEOUT,
            remote_max_retries: DEFAULT_DISTRIBUTED_REMOTE_MAX_RETRIES,
            remote_retry_backoff: DEFAULT_DISTRIBUTED_REMOTE_RETRY_BACKOFF,
            sharding: ShardingConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LimitsConfig {
    pub statement_timeout: Duration,
    pub lock_timeout: Duration,
    pub max_result_rows: u64,
    pub max_result_bytes: u64,
    pub max_memory_bytes: u64,
    pub max_temp_bytes: u64,
    pub max_parallel_workers_per_query: usize,
    pub max_portals: usize,
    pub max_prepared_statements: usize,
    pub max_recursive_iterations: usize,
    pub max_recursive_rows: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            statement_timeout: DEFAULT_LIMITS_STATEMENT_TIMEOUT,
            lock_timeout: DEFAULT_LIMITS_LOCK_TIMEOUT,
            max_result_rows: DEFAULT_LIMITS_MAX_RESULT_ROWS,
            max_result_bytes: DEFAULT_LIMITS_MAX_RESULT_BYTES,
            max_memory_bytes: DEFAULT_LIMITS_MAX_MEMORY_BYTES,
            max_temp_bytes: DEFAULT_LIMITS_MAX_TEMP_BYTES,
            max_parallel_workers_per_query: DEFAULT_LIMITS_MAX_PARALLEL_WORKERS_PER_QUERY,
            max_portals: DEFAULT_LIMITS_MAX_PORTALS,
            max_prepared_statements: DEFAULT_LIMITS_MAX_PREPARED_STATEMENTS,
            max_recursive_iterations: DEFAULT_LIMITS_MAX_RECURSIVE_ITERATIONS,
            max_recursive_rows: DEFAULT_LIMITS_MAX_RECURSIVE_ROWS,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnginePoolConfig {
    pub worker_threads: usize,
    pub queue_depth: usize,
}

impl Default for EnginePoolConfig {
    fn default() -> Self {
        Self {
            worker_threads: default_engine_pool_worker_threads(),
            queue_depth: DEFAULT_ENGINE_POOL_QUEUE_DEPTH,
        }
    }
}

/// Default value for native Cypher execution mode.
pub const DEFAULT_NATIVE_CYPHER: bool = true;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    pub storage: StorageConfig,
    pub pgwire: PgWireConfig,
    pub security: SecurityConfig,
    pub limits: LimitsConfig,
    pub replication: ReplicationConfig,
    pub ha: HaConfig,
    pub distributed: DistributedConfig,
    /// When true (the default), Cypher statements are executed natively via
    /// the graph plan executor.  When false, they are translated to SQL via
    /// the `cypher_sql` translation layer.
    pub native_cypher: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            storage: StorageConfig::default(),
            pgwire: PgWireConfig::default(),
            security: SecurityConfig::default(),
            limits: LimitsConfig::default(),
            replication: ReplicationConfig::default(),
            ha: HaConfig::default(),
            distributed: DistributedConfig::default(),
            native_cypher: DEFAULT_NATIVE_CYPHER,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // -------------------------------------------------------------------
    // LimitsConfig::default() field-by-field
    // -------------------------------------------------------------------

    #[test]
    fn limits_default_statement_timeout_is_30s() {
        let cfg = LimitsConfig::default();
        assert_eq!(cfg.statement_timeout, DEFAULT_LIMITS_STATEMENT_TIMEOUT);
    }

    #[test]
    fn limits_default_max_result_rows_is_10000() {
        let cfg = LimitsConfig::default();
        assert_eq!(cfg.max_result_rows, DEFAULT_LIMITS_MAX_RESULT_ROWS);
    }

    #[test]
    fn limits_default_max_result_bytes_is_8mb() {
        let cfg = LimitsConfig::default();
        assert_eq!(cfg.max_result_bytes, DEFAULT_LIMITS_MAX_RESULT_BYTES);
    }

    #[test]
    fn limits_default_max_memory_bytes_is_64mb() {
        let cfg = LimitsConfig::default();
        assert_eq!(cfg.max_memory_bytes, DEFAULT_LIMITS_MAX_MEMORY_BYTES);
    }

    #[test]
    fn limits_default_max_temp_bytes_is_256mb() {
        let cfg = LimitsConfig::default();
        assert_eq!(cfg.max_temp_bytes, DEFAULT_LIMITS_MAX_TEMP_BYTES);
    }

    #[test]
    fn limits_default_max_parallel_workers_per_query_is_1() {
        let cfg = LimitsConfig::default();
        assert_eq!(
            cfg.max_parallel_workers_per_query,
            DEFAULT_LIMITS_MAX_PARALLEL_WORKERS_PER_QUERY
        );
    }

    #[test]
    fn limits_default_max_portals_is_64() {
        let cfg = LimitsConfig::default();
        assert_eq!(cfg.max_portals, DEFAULT_LIMITS_MAX_PORTALS);
    }

    #[test]
    fn limits_default_max_prepared_statements_is_128() {
        let cfg = LimitsConfig::default();
        assert_eq!(
            cfg.max_prepared_statements,
            DEFAULT_LIMITS_MAX_PREPARED_STATEMENTS
        );
    }

    // -------------------------------------------------------------------
    // EnginePoolConfig::default() field-by-field
    // -------------------------------------------------------------------

    #[test]
    fn engine_pool_default_worker_threads_is_4() {
        let cfg = EnginePoolConfig::default();
        assert!(cfg.worker_threads >= DEFAULT_ENGINE_POOL_WORKER_THREADS);
    }

    #[test]
    fn engine_pool_default_queue_depth_is_1024() {
        let cfg = EnginePoolConfig::default();
        assert_eq!(cfg.queue_depth, DEFAULT_ENGINE_POOL_QUEUE_DEPTH);
    }

    // -------------------------------------------------------------------
    // RuntimeConfig::default() includes all sub-config defaults
    // -------------------------------------------------------------------

    #[test]
    fn runtime_default_storage_matches_storage_default() {
        let cfg = RuntimeConfig::default();
        assert_eq!(cfg.storage, StorageConfig::default());
    }

    #[test]
    fn runtime_default_pgwire_matches_pgwire_default() {
        let cfg = RuntimeConfig::default();
        assert_eq!(cfg.pgwire, PgWireConfig::default());
    }

    #[test]
    fn runtime_default_security_matches_security_default() {
        let cfg = RuntimeConfig::default();
        assert_eq!(cfg.security, SecurityConfig::default());
    }

    #[test]
    fn runtime_default_limits_matches_limits_default() {
        let cfg = RuntimeConfig::default();
        assert_eq!(cfg.limits, LimitsConfig::default());
    }

    #[test]
    fn runtime_default_distributed_matches_distributed_default() {
        let cfg = RuntimeConfig::default();
        assert_eq!(cfg.distributed, DistributedConfig::default());
    }

    #[test]
    fn distributed_default_allow_unregistered_loopback_nodes_is_false() {
        let cfg = DistributedConfig::default();
        assert_eq!(
            cfg.allow_unregistered_loopback_nodes,
            DEFAULT_DISTRIBUTED_ALLOW_UNREGISTERED_LOOPBACK_NODES
        );
    }

    // -------------------------------------------------------------------
    // Clone, Debug, Eq, PartialEq for all config types
    // -------------------------------------------------------------------

    #[test]
    fn limits_config_clone_eq() {
        let a = LimitsConfig::default();
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn limits_config_debug_format_contains_field_names() {
        let cfg = LimitsConfig::default();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("statement_timeout"));
        assert!(dbg.contains("max_result_rows"));
    }

    #[test]
    fn limits_config_ne_when_field_differs() {
        let mut a = LimitsConfig::default();
        let b = LimitsConfig::default();
        a.max_portals = 999;
        assert_ne!(a, b);
    }

    #[test]
    fn engine_pool_config_clone_eq() {
        let a = EnginePoolConfig::default();
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn engine_pool_config_debug_format() {
        let cfg = EnginePoolConfig::default();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("worker_threads"));
        assert!(dbg.contains("queue_depth"));
    }

    #[test]
    fn engine_pool_config_ne_when_field_differs() {
        let mut a = EnginePoolConfig::default();
        let b = EnginePoolConfig::default();
        a.worker_threads = 16;
        assert_ne!(a, b);
    }

    #[test]
    fn runtime_config_clone_eq() {
        let a = RuntimeConfig::default();
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn runtime_config_debug_format() {
        let cfg = RuntimeConfig::default();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("storage"));
        assert!(dbg.contains("pgwire"));
        assert!(dbg.contains("security"));
        assert!(dbg.contains("limits"));
        assert!(dbg.contains("distributed"));
    }

    #[test]
    fn runtime_config_ne_when_limits_differ() {
        let mut a = RuntimeConfig::default();
        let b = RuntimeConfig::default();
        a.limits.max_portals = 1;
        assert_ne!(a, b);
    }

    #[test]
    fn runtime_config_ne_when_security_differs() {
        let mut a = RuntimeConfig::default();
        let b = RuntimeConfig::default();
        a.security.allow_anonymous_local = true;
        assert_ne!(a, b);
    }

    // ===================================================================
    // NEW EDGE CASE TESTS
    // ===================================================================

    // --- LimitsConfig: boundary values ---

    #[test]
    fn limits_config_zero_statement_timeout() {
        let mut cfg = LimitsConfig::default();
        cfg.statement_timeout = Duration::from_millis(0);
        assert_eq!(cfg.statement_timeout, Duration::ZERO);
    }

    #[test]
    fn limits_config_max_u64_result_rows() {
        let mut cfg = LimitsConfig::default();
        cfg.max_result_rows = u64::MAX;
        assert_eq!(cfg.max_result_rows, u64::MAX);
    }

    #[test]
    fn limits_config_max_u64_result_bytes() {
        let mut cfg = LimitsConfig::default();
        cfg.max_result_bytes = u64::MAX;
        assert_eq!(cfg.max_result_bytes, u64::MAX);
    }

    #[test]
    fn limits_config_max_u64_memory_bytes() {
        let mut cfg = LimitsConfig::default();
        cfg.max_memory_bytes = u64::MAX;
        assert_eq!(cfg.max_memory_bytes, u64::MAX);
    }

    #[test]
    fn limits_config_max_u64_temp_bytes() {
        let mut cfg = LimitsConfig::default();
        cfg.max_temp_bytes = u64::MAX;
        assert_eq!(cfg.max_temp_bytes, u64::MAX);
    }

    #[test]
    fn limits_config_zero_all_fields() {
        let cfg = LimitsConfig {
            statement_timeout: Duration::ZERO,
            lock_timeout: Duration::ZERO,
            max_result_rows: 0,
            max_result_bytes: 0,
            max_memory_bytes: 0,
            max_temp_bytes: 0,
            max_parallel_workers_per_query: 0,
            max_portals: 0,
            max_prepared_statements: 0,
            max_recursive_iterations: 0,
            max_recursive_rows: 0,
        };
        assert_eq!(cfg.max_portals, 0);
        assert_eq!(cfg.max_prepared_statements, 0);
    }

    #[test]
    fn limits_config_max_usize_portals() {
        let mut cfg = LimitsConfig::default();
        cfg.max_portals = usize::MAX;
        assert_eq!(cfg.max_portals, usize::MAX);
    }

    #[test]
    fn limits_config_max_usize_prepared_statements() {
        let mut cfg = LimitsConfig::default();
        cfg.max_prepared_statements = usize::MAX;
        assert_eq!(cfg.max_prepared_statements, usize::MAX);
    }

    #[test]
    fn limits_config_very_large_timeout() {
        let mut cfg = LimitsConfig::default();
        cfg.statement_timeout = Duration::from_secs(60 * 60 * 8760);
        assert_eq!(cfg.statement_timeout.as_secs(), 86400 * 365);
    }

    // --- EnginePoolConfig: boundary values ---

    #[test]
    fn engine_pool_zero_worker_threads() {
        let cfg = EnginePoolConfig {
            worker_threads: 0,
            queue_depth: 1024,
        };
        assert_eq!(cfg.worker_threads, 0);
    }

    #[test]
    fn engine_pool_zero_queue_depth() {
        let cfg = EnginePoolConfig {
            worker_threads: 4,
            queue_depth: 0,
        };
        assert_eq!(cfg.queue_depth, 0);
    }

    #[test]
    fn engine_pool_max_usize_values() {
        let cfg = EnginePoolConfig {
            worker_threads: usize::MAX,
            queue_depth: usize::MAX,
        };
        assert_eq!(cfg.worker_threads, usize::MAX);
        assert_eq!(cfg.queue_depth, usize::MAX);
    }

    // --- RuntimeConfig: mutating nested fields ---

    #[test]
    fn runtime_config_ne_when_pgwire_max_connections_differs() {
        let mut a = RuntimeConfig::default();
        let b = RuntimeConfig::default();
        a.pgwire.max_connections = 999;
        assert_ne!(a, b);
    }

    #[test]
    fn runtime_config_ne_when_storage_differs() {
        let mut a = RuntimeConfig::default();
        let b = RuntimeConfig::default();
        a.storage.page_size = 1;
        assert_ne!(a, b);
    }

    #[test]
    fn runtime_config_ne_when_engine_pool_differs() {
        let mut a = RuntimeConfig::default();
        let b = RuntimeConfig::default();
        a.pgwire.engine_pool.queue_depth = 1;
        assert_ne!(a, b);
    }

    // --- LimitsConfig: ne for all individual fields ---

    #[test]
    fn limits_ne_when_max_result_rows_differs() {
        let mut a = LimitsConfig::default();
        let b = LimitsConfig::default();
        a.max_result_rows = 1;
        assert_ne!(a, b);
    }

    #[test]
    fn limits_ne_when_max_result_bytes_differs() {
        let mut a = LimitsConfig::default();
        let b = LimitsConfig::default();
        a.max_result_bytes = 1;
        assert_ne!(a, b);
    }

    #[test]
    fn limits_ne_when_max_memory_bytes_differs() {
        let mut a = LimitsConfig::default();
        let b = LimitsConfig::default();
        a.max_memory_bytes = 1;
        assert_ne!(a, b);
    }

    #[test]
    fn limits_ne_when_max_temp_bytes_differs() {
        let mut a = LimitsConfig::default();
        let b = LimitsConfig::default();
        a.max_temp_bytes = 1;
        assert_ne!(a, b);
    }

    #[test]
    fn limits_ne_when_statement_timeout_differs() {
        let mut a = LimitsConfig::default();
        let b = LimitsConfig::default();
        a.statement_timeout = Duration::from_millis(1);
        assert_ne!(a, b);
    }

    #[test]
    fn limits_ne_when_max_prepared_statements_differs() {
        let mut a = LimitsConfig::default();
        let b = LimitsConfig::default();
        a.max_prepared_statements = 1;
        assert_ne!(a, b);
    }

    // --- EnginePoolConfig: ne for queue_depth ---

    #[test]
    fn engine_pool_ne_when_queue_depth_differs() {
        let mut a = EnginePoolConfig::default();
        let b = EnginePoolConfig::default();
        a.queue_depth = 1;
        assert_ne!(a, b);
    }

    // --- Debug output includes specific values ---

    #[test]
    fn limits_debug_contains_default_values() {
        let cfg = LimitsConfig::default();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("10000")); // max_result_rows
    }

    #[test]
    fn engine_pool_debug_contains_default_values() {
        let cfg = EnginePoolConfig::default();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains('4')); // worker_threads
        assert!(dbg.contains("1024")); // queue_depth
    }

    #[test]
    fn distributed_debug_does_not_expose_inter_node_auth_token() {
        let mut cfg = DistributedConfig::default();
        cfg.inter_node_auth_token = Some("super-secret-distributed-token".to_owned());
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("super-secret-distributed-token"));
        assert!(dbg.contains("redacted"));
    }

    #[test]
    fn distributed_debug_shows_none_when_no_token() {
        let cfg = DistributedConfig::default();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("inter_node_auth_token: None"));
    }

    #[test]
    fn distributed_remote_snapshot_mode_defaults_to_latest_visible() {
        let cfg = DistributedConfig::default();
        assert_eq!(cfg.remote_snapshot_mode, RemoteSnapshotMode::LatestVisible);
        assert_eq!(cfg.remote_snapshot_mode.as_str(), "latest_visible");
    }

    #[test]
    fn distributed_remote_circuit_breaker_defaults_are_stable() {
        let cfg = DistributedConfig::default();
        assert_eq!(
            cfg.remote_circuit_breaker_failure_threshold,
            DEFAULT_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_FAILURE_THRESHOLD
        );
        assert_eq!(
            cfg.remote_circuit_breaker_reset_timeout,
            DEFAULT_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_RESET_TIMEOUT
        );
    }

    #[test]
    fn distributed_remote_client_defaults_are_stable() {
        let cfg = DistributedConfig::default();
        assert_eq!(
            cfg.remote_connect_timeout,
            DEFAULT_DISTRIBUTED_REMOTE_CONNECT_TIMEOUT
        );
        assert_eq!(
            cfg.remote_max_retries,
            DEFAULT_DISTRIBUTED_REMOTE_MAX_RETRIES
        );
        assert_eq!(
            cfg.remote_retry_backoff,
            DEFAULT_DISTRIBUTED_REMOTE_RETRY_BACKOFF
        );
    }

    #[test]
    fn distributed_remote_snapshot_mode_parse_accepts_aliases() {
        assert_eq!(
            RemoteSnapshotMode::parse("latest"),
            Some(RemoteSnapshotMode::LatestVisible)
        );
        assert_eq!(
            RemoteSnapshotMode::parse("coordinator_snapshot"),
            Some(RemoteSnapshotMode::Coordinator)
        );
        assert_eq!(RemoteSnapshotMode::parse("invalid"), None);
    }
}
