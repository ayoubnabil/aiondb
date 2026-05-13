#![allow(clippy::pedantic)]
#![allow(
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines
)]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use aiondb_catalog::{CatalogReader, CatalogTxnParticipant, CatalogWriter, SequenceManager};
use aiondb_catalog_store::{CatalogStore, CatalogWalHandle};
use aiondb_cluster::distributed::{InMemoryControlPlane, NodeDescriptor, NodeId, NodeMembership};
use aiondb_config::{
    replication::{WalCompression as ConfigWalCompression, WalLsnMode as ConfigWalLsnMode},
    runtime::RemoteSnapshotMode,
    storage::{
        DurableWalCommitPolicy as ConfigDurableWalCommitPolicy,
        DEFAULT_STORAGE_WAL_GROUP_COMMIT_DELAY_MICROS,
    },
    RuntimeConfig, SecurityConfig, SecurityProfile, StorageBackend,
};
use aiondb_core::{DbError, DbResult};
use aiondb_executor::node_registry::NodeRegistry;
use aiondb_executor::{FragmentDispatcher, RegisteredRemoteFragmentDispatcher};
use aiondb_security::{
    AllowAllAuthorizer, AuthRateLimiter, Authenticator, Authorizer, DenyAllAuthorizer,
    FileBackedAuthRateLimiter, InMemoryAuthRateLimiter,
};
use aiondb_shard::ShardedStorage;
use aiondb_storage_api::{StorageDDL, StorageDML, StorageTxnParticipant};
use aiondb_storage_engine::{
    DiskBackendConfig, LsmBackendConfig, PageEngineBackendConfig, StorageBackendHandle,
    StorageBackendSpec, StorageBufferPoolConfig, StorageOptions, WalCommitPolicy, WalConfig,
};
use aiondb_tx::{
    InMemoryTransactionManager, LockManager, NoopSerializableCoordinator, SerializableCoordinator,
    SnapshotOracle, TransactionLifecycle, WaitGraphLockManager,
};
use aiondb_wal::replication::{ReplicaRegistry, WalNotifier};
use tracing::{info, warn};

use crate::auth_audit::{default_auth_audit_sink, AuthAuditSink};
use crate::catalog_auth::CatalogAuthPolicy;
use crate::engine::replication::EngineReplicationHandle;
use crate::engine::streaming::{ReplicationManager, StreamingReplicationState};
use crate::engine::ReplicationIdentity;
use crate::{config::EngineConfig, Engine};

const MIN_INTER_NODE_AUTH_TOKEN_BYTES: usize = 32;
#[cfg(test)]
const TEST_INTER_NODE_AUTH_TOKEN: &str = "0123456789abcdef0123456789abcdef";

pub struct EngineBuilder {
    runtime_config: RuntimeConfig,
    allow_ephemeral_users_override: Option<bool>,
    authenticator: Option<Arc<dyn Authenticator>>,
    authorizer: Arc<dyn Authorizer>,
    rate_limiter: Option<Arc<dyn AuthRateLimiter>>,
    auth_audit_sink: Option<Arc<dyn AuthAuditSink>>,
    tx_manager: Arc<dyn TransactionLifecycle>,
    snapshot_oracle: Arc<dyn SnapshotOracle>,
    serializable_coordinator: Arc<dyn SerializableCoordinator>,
    lock_manager: Arc<dyn LockManager>,
    catalog_txn: Arc<dyn CatalogTxnParticipant>,
    catalog_reader: Arc<dyn CatalogReader>,
    catalog_writer: Arc<dyn CatalogWriter>,
    sequence_manager: Arc<dyn SequenceManager>,
    storage_ddl: Arc<dyn StorageDDL>,
    storage_dml: Arc<dyn StorageDML>,
    storage_txn: Arc<dyn StorageTxnParticipant>,
    fragment_dispatcher: Option<Arc<dyn FragmentDispatcher>>,
    node_registry: Option<Arc<NodeRegistry>>,
    replication_handle: Option<EngineReplicationHandle>,
    replication_manager: Option<Arc<ReplicationManager>>,
    replication_identity: Option<ReplicationIdentity>,
    /// Whether the storage layer is backed by WAL for durability.
    durable: bool,
}

impl EngineBuilder {
    fn invalidate_replication_state(&mut self) {
        self.replication_handle = None;
        self.replication_manager = None;
        self.replication_identity = None;
    }

    /// Create a builder with in-memory storage (no WAL, no persistence).
    ///
    /// Intended for **tests and development** only. Data is lost on restart.
    /// For production use, prefer [`new_durable`](Self::new_durable).
    #[must_use]
    pub fn new_in_memory() -> Self {
        let tx_runtime = Arc::new(InMemoryTransactionManager::default());
        let replication_export_barrier = Arc::new(RwLock::new(()));
        let replica_registry = Arc::new(ReplicaRegistry::new());
        let mut catalog_store = CatalogStore::default();
        catalog_store.set_replication_export_barrier(Arc::clone(&replication_export_barrier));
        let catalog = Arc::new(catalog_store);
        let mut storage_handle = StorageBackendHandle::open_in_memory(None);
        storage_handle.set_replication_export_barrier(Arc::clone(&replication_export_barrier));
        storage_handle.set_replica_registry(Arc::clone(&replica_registry));
        let storage = Arc::new(storage_handle);
        let replication_handle = Some(EngineReplicationHandle::new(
            storage.clone(),
            catalog.clone(),
            Arc::clone(&replication_export_barrier),
        ));
        let catalog_txn: Arc<dyn CatalogTxnParticipant> = catalog.clone();
        let catalog_reader: Arc<dyn CatalogReader> = catalog.clone();
        let catalog_writer: Arc<dyn CatalogWriter> = catalog.clone();
        let sequence_manager: Arc<dyn SequenceManager> = catalog;
        let storage_ddl: Arc<dyn StorageDDL> = storage.clone();
        let storage_dml: Arc<dyn StorageDML> = storage.clone();
        let storage_txn: Arc<dyn StorageTxnParticipant> = storage;
        let mut runtime_config = RuntimeConfig::default();
        runtime_config.security = SecurityConfig::from_profile(SecurityProfile::Development);

        Self {
            runtime_config,
            allow_ephemeral_users_override: None,
            authenticator: None,
            authorizer: Arc::new(DenyAllAuthorizer),
            rate_limiter: None,
            auth_audit_sink: None,
            tx_manager: tx_runtime.clone(),
            snapshot_oracle: tx_runtime.clone(),
            serializable_coordinator: tx_runtime,
            lock_manager: Arc::new(WaitGraphLockManager::default()),
            catalog_txn,
            catalog_reader,
            catalog_writer,
            sequence_manager,
            storage_ddl,
            storage_dml,
            storage_txn,
            fragment_dispatcher: None,
            node_registry: None,
            replication_handle,
            replication_manager: None,
            replication_identity: None,
            durable: false,
        }
    }

    /// Create a builder with durable storage backed by WAL.
    ///
    /// WAL segment files are written to `<data_dir>/wal/`. The storage engine
    /// replays the WAL on startup so committed data survives restarts.
    ///
    /// The catalog is recovered from `<data_dir>/catalog_wal/` (snapshot +
    /// WAL entries) and then backed by its own WAL writer for durability.
    ///
    /// # Errors
    ///
    /// Returns an error if the WAL directory cannot be created or opened.
    pub fn new_durable(data_dir: PathBuf) -> DbResult<Self> {
        Self::new_durable_with_config(data_dir, RuntimeConfig::default())
    }

    /// Create a builder with durable storage backed by WAL and an explicit
    /// runtime config for storage-level tunables such as buffer-pool sizing.
    ///
    /// # Errors
    ///
    /// Returns an error if the WAL directory cannot be created or opened.
    pub fn new_durable_with_config(
        data_dir: PathBuf,
        mut runtime_config: RuntimeConfig,
    ) -> DbResult<Self> {
        runtime_config.storage.backend = StorageBackend::Durable;
        Self::new_with_config(data_dir, runtime_config)
    }

    /// Create a builder using the storage backend declared in the runtime
    /// config.
    ///
    /// `runtime_config.storage.backend` selects the backend family while
    /// `data_dir` remains the root directory for persistent state and catalog
    /// WAL files.
    ///
    /// # Errors
    ///
    /// Returns an error if the selected storage backend cannot be opened.
    pub fn new_with_config(data_dir: PathBuf, mut runtime_config: RuntimeConfig) -> DbResult<Self> {
        let tx_runtime = Arc::new(InMemoryTransactionManager::default());
        let storage_backend = runtime_config.storage.backend;
        if storage_backend.is_persistent() {
            aiondb_storage_engine::ensure_storage_contract_for_open(
                &data_dir,
                storage_backend_kind(storage_backend),
            )?;
        }
        let replication_export_barrier = Arc::new(RwLock::new(()));
        let replica_registry = Arc::new(if storage_backend.is_persistent() {
            ReplicaRegistry::open(replication_slot_dir(&data_dir, &runtime_config))?
        } else {
            ReplicaRegistry::new()
        });
        let mut storage_handle =
            StorageBackendHandle::open(storage_backend_spec(&data_dir, &runtime_config))?;
        storage_handle.set_replication_export_barrier(Arc::clone(&replication_export_barrier));
        storage_handle.set_replica_registry(Arc::clone(&replica_registry));
        storage_handle.set_min_wal_keep_segments(runtime_config.replication.wal_keep_segments);
        storage_handle.set_write_concern(
            runtime_config.replication.write_concern.encode_level(),
            runtime_config.replication.sync_commit_timeout,
        );
        // Initialize GPU distance computer for HNSW index construction.
        let gpu_enabled = std::env::var("AIONDB_GPU_ENABLED")
            .ok()
            .is_some_and(|v| v == "true" || v == "1");
        let gpu_computer = aiondb_gpu::create_distance_computer(gpu_enabled);
        storage_handle.set_gpu_distance_computer(std::sync::Arc::from(gpu_computer));
        let (replication_manager, replication_identity) =
            if let Some(initial_wal_lsn) = storage_handle.current_wal_end_lsn()? {
                let wal_notifier = Arc::new(WalNotifier::new(initial_wal_lsn));
                storage_handle.set_wal_notifier(Arc::clone(&wal_notifier))?;
                build_replication_runtime(
                    &runtime_config,
                    &data_dir,
                    Arc::clone(&replica_registry),
                    wal_notifier,
                )?
            } else {
                (None, None)
            };
        let storage = Arc::new(storage_handle);

        // Recover catalog state from snapshot + WAL, then open a WAL
        // writer for ongoing catalog mutations.
        let catalog_wal_dir = data_dir.join("catalog_wal");
        let recovered_state =
            aiondb_catalog_store::recovery::recover_catalog_state(&catalog_wal_dir)?;
        let catalog_wal_config = WalConfig {
            dir: catalog_wal_dir,
            wal_compression: wal_compression_mode(&runtime_config),
            wal_lsn_mode: wal_lsn_mode(runtime_config.replication.wal_lsn_mode),
            ..WalConfig::default()
        };
        let catalog_wal_handle = Arc::new(CatalogWalHandle::open(catalog_wal_config)?);
        let mut catalog_store = CatalogStore::from_recovered(recovered_state, catalog_wal_handle);
        catalog_store.set_replication_export_barrier(Arc::clone(&replication_export_barrier));
        let catalog = Arc::new(catalog_store);

        info!("engine startup: storage and catalog recovery completed successfully");
        let replication_handle = Some(EngineReplicationHandle::new(
            storage.clone(),
            catalog.clone(),
            Arc::clone(&replication_export_barrier),
        ));

        let catalog_txn: Arc<dyn CatalogTxnParticipant> = catalog.clone();
        let catalog_reader: Arc<dyn CatalogReader> = catalog.clone();
        let catalog_writer: Arc<dyn CatalogWriter> = catalog.clone();
        let sequence_manager: Arc<dyn SequenceManager> = catalog;

        // Optionally wrap storage with shard-aware routing layer.
        let (storage_ddl, storage_dml, storage_txn): (
            Arc<dyn StorageDDL>,
            Arc<dyn StorageDML>,
            Arc<dyn StorageTxnParticipant>,
        ) = if runtime_config.distributed.sharding.enabled {
            // Use a high base ID range for physical shard tables to avoid
            // collisions with user-created table IDs.
            let sharded = Arc::new(ShardedStorage::new(storage, 1 << 40));
            (sharded.clone(), sharded.clone(), sharded)
        } else {
            (storage.clone(), storage.clone(), storage)
        };

        runtime_config.storage.data_dir = data_dir;

        Ok(Self {
            runtime_config,
            allow_ephemeral_users_override: None,
            authenticator: None,
            authorizer: Arc::new(DenyAllAuthorizer),
            rate_limiter: None,
            auth_audit_sink: None,
            tx_manager: tx_runtime.clone(),
            snapshot_oracle: tx_runtime.clone(),
            serializable_coordinator: tx_runtime,
            lock_manager: Arc::new(WaitGraphLockManager::default()),
            catalog_txn,
            catalog_reader,
            catalog_writer,
            sequence_manager,
            storage_ddl,
            storage_dml,
            storage_txn,
            fragment_dispatcher: None,
            node_registry: None,
            replication_handle,
            replication_manager,
            replication_identity,
            durable: storage_backend.is_persistent(),
        })
    }

    /// Convenience constructor for test code.
    ///
    /// Identical to [`Self::new_in_memory()`] except the authorizer is set to
    /// [`AllowAllAuthorizer`] and ephemeral startup is explicitly enabled so
    /// that tests do not need to seed catalog roles first.
    #[must_use]
    pub fn for_testing() -> Self {
        let mut builder = Self::new_in_memory().with_authorizer(Arc::new(AllowAllAuthorizer));
        builder.allow_ephemeral_users_override = Some(true);
        builder
    }

    #[must_use]
    pub fn with_runtime_config(mut self, runtime_config: RuntimeConfig) -> Self {
        self.runtime_config = runtime_config;
        self.invalidate_replication_state();
        self
    }

    #[must_use]
    pub fn with_allow_ephemeral_users(mut self, allow_ephemeral_users: bool) -> Self {
        self.allow_ephemeral_users_override = Some(allow_ephemeral_users);
        self
    }

    #[must_use]
    pub fn with_authenticator(mut self, authenticator: Arc<dyn Authenticator>) -> Self {
        self.authenticator = Some(authenticator);
        self
    }

    #[must_use]
    pub fn with_authorizer(mut self, authorizer: Arc<dyn Authorizer>) -> Self {
        self.authorizer = authorizer;
        self
    }

    #[must_use]
    pub fn with_rate_limiter(mut self, rate_limiter: Arc<dyn AuthRateLimiter>) -> Self {
        self.rate_limiter = Some(rate_limiter);
        self
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_auth_audit_sink(mut self, auth_audit_sink: Arc<dyn AuthAuditSink>) -> Self {
        self.auth_audit_sink = Some(auth_audit_sink);
        self
    }

    #[must_use]
    pub fn with_transaction_manager(mut self, tx_manager: Arc<dyn TransactionLifecycle>) -> Self {
        self.tx_manager = tx_manager;
        self.serializable_coordinator = Arc::new(NoopSerializableCoordinator);
        self
    }

    #[must_use]
    pub fn with_snapshot_oracle(mut self, snapshot_oracle: Arc<dyn SnapshotOracle>) -> Self {
        self.snapshot_oracle = snapshot_oracle;
        self
    }

    #[must_use]
    pub fn with_serializable_coordinator(
        mut self,
        serializable_coordinator: Arc<dyn SerializableCoordinator>,
    ) -> Self {
        self.serializable_coordinator = serializable_coordinator;
        self
    }

    #[must_use]
    pub fn with_lock_manager(mut self, lock_manager: Arc<dyn LockManager>) -> Self {
        self.lock_manager = lock_manager;
        self
    }

    #[must_use]
    pub fn with_catalog_reader(mut self, catalog_reader: Arc<dyn CatalogReader>) -> Self {
        self.catalog_reader = catalog_reader;
        self.invalidate_replication_state();
        self
    }

    #[must_use]
    pub fn with_catalog_txn(mut self, catalog_txn: Arc<dyn CatalogTxnParticipant>) -> Self {
        self.catalog_txn = catalog_txn;
        self.invalidate_replication_state();
        self
    }

    #[must_use]
    pub fn with_catalog_writer(mut self, catalog_writer: Arc<dyn CatalogWriter>) -> Self {
        self.catalog_writer = catalog_writer;
        self.invalidate_replication_state();
        self
    }

    #[must_use]
    pub fn with_sequence_manager(mut self, sequence_manager: Arc<dyn SequenceManager>) -> Self {
        self.sequence_manager = sequence_manager;
        self.invalidate_replication_state();
        self
    }

    #[must_use]
    pub fn with_storage_ddl(mut self, storage_ddl: Arc<dyn StorageDDL>) -> Self {
        self.storage_ddl = storage_ddl;
        self.invalidate_replication_state();
        self
    }

    #[must_use]
    pub fn with_storage_dml(mut self, storage_dml: Arc<dyn StorageDML>) -> Self {
        self.storage_dml = storage_dml;
        self.invalidate_replication_state();
        self
    }

    #[must_use]
    pub fn with_storage_txn(mut self, storage_txn: Arc<dyn StorageTxnParticipant>) -> Self {
        self.storage_txn = storage_txn;
        self.invalidate_replication_state();
        self
    }

    #[must_use]
    pub fn with_fragment_dispatcher(
        mut self,
        fragment_dispatcher: Arc<dyn FragmentDispatcher>,
    ) -> Self {
        self.fragment_dispatcher = Some(fragment_dispatcher);
        self
    }

    /// Attach a [`NodeRegistry`] for health-tracking and circuit-breaker
    /// aware distributed dispatch.
    ///
    /// When set, configured remote nodes are registered in the registry and
    /// remote fragment dispatch consults each node's circuit breaker before
    /// using the direct handler fallback.
    #[must_use]
    pub fn with_node_registry(mut self, registry: Arc<NodeRegistry>) -> Self {
        self.node_registry = Some(registry);
        self
    }

    pub fn build(mut self) -> DbResult<Engine> {
        if let Some(allow_ephemeral_users) = self.allow_ephemeral_users_override {
            self.runtime_config.security.allow_ephemeral_users = allow_ephemeral_users;
        }

        // Force RBAC-active when the security profile is non-Development so
        // a fresh catalog without real roles still routes DDL through the
        // superuser gate (audit engine_functions/backup F1 root cause).
        if !matches!(
            self.runtime_config.security.profile,
            SecurityProfile::Development
        ) {
            crate::catalog_authorizer::force_rbac_active();
        }

        if self.durable {
            info!(
                backend = self.runtime_config.storage.backend.as_str(),
                data_dir = %self.runtime_config.storage.data_dir.display(),
                "engine starting with persistent storage backend"
            );
        } else {
            warn!(
                "engine running without WAL — data will be lost on restart. \
                 Use EngineBuilder::new_durable() for production."
            );
        }

        let (startup_auth_policy, authenticator): (
            Option<Arc<CatalogAuthPolicy>>,
            Arc<dyn Authenticator>,
        ) = match self.authenticator {
            Some(authenticator) => (None, authenticator),
            None => {
                let policy = Arc::new(CatalogAuthPolicy::new(
                    self.catalog_reader.clone(),
                    self.runtime_config.security.clone(),
                )?);
                let authenticator: Arc<dyn Authenticator> = policy.clone();
                (Some(policy), authenticator)
            }
        };
        let rate_limiter: Arc<dyn AuthRateLimiter> = self
            .rate_limiter
            .unwrap_or_else(|| default_rate_limiter(&self.runtime_config));
        let auth_audit_sink = self
            .auth_audit_sink
            .unwrap_or_else(|| default_auth_audit_sink(&self.runtime_config));
        let dispatch_node_registry = self.node_registry.clone().or_else(|| {
            if self.fragment_dispatcher.is_none()
                && !self.runtime_config.distributed.remote_nodes.is_empty()
            {
                Some(Arc::new(NodeRegistry::with_circuit_breaker_config(
                    self.runtime_config
                        .distributed
                        .remote_circuit_breaker_failure_threshold,
                    self.runtime_config
                        .distributed
                        .remote_circuit_breaker_reset_timeout,
                )))
            } else {
                None
            }
        });
        let fragment_dispatcher = match self.fragment_dispatcher {
            Some(dispatcher) => Some(dispatcher),
            None => configured_fragment_dispatcher_from_config(
                &self.runtime_config,
                dispatch_node_registry.clone(),
            )?,
        };

        if let Some(ref registry) = dispatch_node_registry {
            info!(
                total_nodes = registry.node_count(),
                available_nodes = registry.available_count(),
                "node registry attached to engine builder"
            );
        }
        let distributed_control_plane =
            configured_distributed_control_plane_from_config(&self.runtime_config)?;
        let control_plane_snapshot = distributed_control_plane.snapshot()?;
        info!(
            total_nodes = control_plane_snapshot.total_nodes,
            live_nodes = control_plane_snapshot.live_nodes,
            "distributed control plane attached to engine builder"
        );

        let engine = Engine::new(
            EngineConfig::from(&self.runtime_config),
            self.runtime_config,
            authenticator,
            self.authorizer,
            rate_limiter,
            auth_audit_sink,
            self.tx_manager,
            self.snapshot_oracle,
            self.serializable_coordinator,
            self.lock_manager,
            self.catalog_txn,
            self.catalog_reader,
            self.catalog_writer,
            self.sequence_manager,
            self.storage_ddl,
            self.storage_dml,
            self.storage_txn,
            fragment_dispatcher,
            dispatch_node_registry,
            distributed_control_plane,
            startup_auth_policy,
            self.replication_handle,
            self.replication_manager,
            self.replication_identity,
        );
        engine.bootstrap_predefined_pg_roles()?;
        Ok(engine)
    }
}

fn build_replication_runtime(
    runtime_config: &RuntimeConfig,
    data_dir: &Path,
    replica_registry: Arc<ReplicaRegistry>,
    wal_notifier: Arc<WalNotifier>,
) -> DbResult<(Option<Arc<ReplicationManager>>, Option<ReplicationIdentity>)> {
    match runtime_config.replication.role {
        aiondb_config::ReplicationRole::Primary => {
            let wal_dir = storage_backend_wal_dir(data_dir, runtime_config);
            let timeline = load_or_create_timeline_id(data_dir, replication_promote_on_start())?;
            let state = StreamingReplicationState::new_primary_shared(
                wal_dir,
                runtime_config.replication.clone(),
                replica_registry,
                wal_notifier,
                u64::from(timeline),
            );
            let manager = Arc::new(ReplicationManager::new(Arc::new(state)));
            let identity = Some(ReplicationIdentity {
                system_identifier: load_or_create_system_identifier(data_dir)?,
                timeline,
            });
            Ok((Some(manager), identity))
        }
        aiondb_config::ReplicationRole::Replica => {
            let wal_dir = storage_backend_wal_dir(data_dir, runtime_config);
            let wal_config = aiondb_wal::WalConfig {
                dir: wal_dir.clone(),
                ..aiondb_wal::WalConfig::default()
            };
            let state = StreamingReplicationState::new_replica(
                wal_dir,
                wal_config,
                runtime_config.replication.clone(),
            )?;
            let manager = Arc::new(ReplicationManager::new(Arc::new(state)));
            let identity = Some(ReplicationIdentity {
                system_identifier: load_or_create_system_identifier(data_dir)?,
                timeline: 1,
            });
            Ok((Some(manager), identity))
        }
        aiondb_config::ReplicationRole::Standalone => Ok((None, None)),
    }
}

fn configured_distributed_control_plane_from_config(
    runtime_config: &RuntimeConfig,
) -> DbResult<Arc<InMemoryControlPlane>> {
    let control_plane = Arc::new(InMemoryControlPlane::new());
    control_plane.upsert_node(NodeDescriptor {
        node_id: NodeId::local(),
        rpc_endpoint: format!(
            "127.0.0.1:{}",
            runtime_config.distributed.fragment_transport_port
        ),
        is_live: true,
    })?;

    for remote in &runtime_config.distributed.remote_nodes {
        control_plane.upsert_node(NodeDescriptor {
            node_id: NodeId::new(remote.node_id.clone()),
            rpc_endpoint: remote.addr.clone(),
            is_live: true,
        })?;
    }

    Ok(control_plane)
}

fn replication_promote_on_start() -> bool {
    std::env::var("AIONDB_REPLICATION_PROMOTE_ON_START")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn storage_backend_wal_dir(data_dir: &Path, runtime_config: &RuntimeConfig) -> PathBuf {
    match runtime_config.storage.backend {
        StorageBackend::InMemory | StorageBackend::Durable => data_dir.join("wal"),
        StorageBackend::Disk => data_dir.join("disk").join("wal"),
        StorageBackend::PageEngine => data_dir.join("page_engine"),
        StorageBackend::Lsm => data_dir.join("lsm").join("wal"),
    }
}

fn configured_fragment_dispatcher_from_config(
    runtime_config: &RuntimeConfig,
    node_registry: Option<Arc<NodeRegistry>>,
) -> DbResult<Option<Arc<dyn FragmentDispatcher>>> {
    if runtime_config.distributed.loopback_remote_nodes.is_empty()
        && !runtime_config.distributed.allow_unregistered_loopback_nodes
        && runtime_config.distributed.remote_nodes.is_empty()
    {
        return Ok(None);
    }

    // Validate security when remote nodes are configured.
    if !runtime_config.distributed.remote_nodes.is_empty() {
        // Require non-empty auth token.
        let token = runtime_config
            .distributed
            .inter_node_auth_token
            .as_deref()
            .map(str::trim)
            .unwrap_or("");
        if token.is_empty() {
            return Err(DbError::invalid_authorization(
                "inter_node_auth_token must be set when remote_nodes are configured",
            ));
        }
        if token.len() < MIN_INTER_NODE_AUTH_TOKEN_BYTES {
            return Err(DbError::invalid_authorization(format!(
                "inter_node_auth_token must be at least {MIN_INTER_NODE_AUTH_TOKEN_BYTES} bytes when remote_nodes are configured"
            )));
        }
        // Require TLS if require_tls is true.
        if runtime_config.distributed.require_tls
            && runtime_config.distributed.tls_cert_path.is_none()
        {
            return Err(DbError::feature_not_supported(
                "TLS certificate must be configured for inter-node transport (set tls_cert_path, tls_key_path, tls_ca_cert_path or set require_tls=false)",
            ));
        }
    }

    // In production security profile, enforce TLS and mTLS for inter-node
    // fragment transport even if the caller configured require_tls=false.
    if runtime_config.security.profile == SecurityProfile::Production
        && !runtime_config.distributed.remote_nodes.is_empty()
    {
        if runtime_config.distributed.tls_cert_path.is_none() {
            return Err(DbError::feature_not_supported(
                "production security profile requires TLS for inter-node fragment transport \
                 (configure tls_cert_path, tls_key_path, tls_ca_cert_path)",
            ));
        }
        if runtime_config.distributed.tls_ca_cert_path.is_none() {
            return Err(DbError::feature_not_supported(
                "production security profile requires mutual TLS (mTLS) for inter-node transport \
                 (configure tls_ca_cert_path for client certificate verification)",
            ));
        }
    }

    let mut dispatcher =
        RegisteredRemoteFragmentDispatcher::new().with_loopback_remote_targets(false);
    if let Some(registry) = node_registry.as_ref() {
        dispatcher = dispatcher.with_node_registry(Arc::clone(registry));
    }
    for node_id in &runtime_config.distributed.loopback_remote_nodes {
        dispatcher.register_remote_handler(
            node_id.clone(),
            Arc::new(|fragment, executor, context| executor.execute(&fragment.plan, context)),
        );
    }
    // Register real remote fragment handlers for configured remote nodes.
    let remote_connection_pool =
        std::sync::Arc::new(aiondb_fragment_transport::client::ConnectionPool::with_defaults());
    for node_config in &runtime_config.distributed.remote_nodes {
        let client_config = remote_fragment_client_config_from_runtime(runtime_config, node_config);
        let client = std::sync::Arc::new(
            aiondb_fragment_transport::FragmentClient::new(client_config)
                .with_connection_pool(std::sync::Arc::clone(&remote_connection_pool)),
        );
        let node_id = node_config.node_id.clone();
        let remote_snapshot_mode = runtime_config.distributed.remote_snapshot_mode;
        let handler: Arc<aiondb_executor::RemoteFragmentHandler> =
            Arc::new(move |fragment, _executor, context| {
                let frag_context = aiondb_fragment_transport::client::FragmentContext {
                    txn_id: context.txn_id.get(),
                    isolation: format!("{:?}", context.isolation),
                    max_result_rows: context.max_result_rows,
                    max_result_bytes: context.max_result_bytes,
                    max_memory_bytes: context.max_memory_bytes,
                    max_temp_bytes: context.max_temp_bytes,
                    snapshot: match remote_snapshot_mode {
                        RemoteSnapshotMode::LatestVisible => None,
                        RemoteSnapshotMode::Coordinator => {
                            Some(aiondb_fragment_transport::protocol::FragmentSnapshot {
                                xmin: context.snapshot.xmin.get(),
                                xmax: context.snapshot.xmax.get(),
                                active: context
                                    .snapshot
                                    .active
                                    .iter()
                                    .map(|txn| txn.get())
                                    .collect(),
                            })
                        }
                    },
                    shard_id: fragment.shard_id,
                    deadline_epoch_ms: context.statement_deadline.map(|d| {
                        let now = std::time::Instant::now();
                        let remaining = d.saturating_duration_since(now);
                        let epoch = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default();
                        u64::try_from((epoch + remaining).as_millis()).unwrap_or(u64::MAX)
                    }),
                };
                execute_remote_fragment_client_blocking(
                    std::sync::Arc::clone(&client),
                    fragment.plan.clone(),
                    frag_context,
                )
            });
        if let Some(registry) = node_registry.as_ref() {
            registry.register(
                node_id.clone(),
                node_config.addr.clone(),
                Arc::clone(&handler),
            );
        }
        dispatcher.register_remote_handler(node_id, handler);
    }

    if !runtime_config.distributed.remote_nodes.is_empty() {
        info!(
            remote_nodes = runtime_config.distributed.remote_nodes.len(),
            "configured remote fragment transport handlers"
        );
    }

    if runtime_config.distributed.allow_unregistered_loopback_nodes {
        dispatcher.set_default_remote_handler(Arc::new(|fragment, executor, context| {
            executor.execute(&fragment.plan, context)
        }));
    }

    Ok(Some(Arc::new(dispatcher)))
}

fn execute_remote_fragment_client_blocking(
    client: std::sync::Arc<aiondb_fragment_transport::FragmentClient>,
    plan: aiondb_plan::PhysicalPlan,
    context: aiondb_fragment_transport::client::FragmentContext,
) -> DbResult<aiondb_executor::ExecutionResult> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return execute_remote_fragment_client_on_dedicated_thread(client, plan, context);
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            DbError::internal(format!(
                "failed to create tokio runtime for remote fragment execution: {error}"
            ))
        })?;
    runtime.block_on(client.execute(&plan, context))
}

fn execute_remote_fragment_client_on_dedicated_thread(
    client: std::sync::Arc<aiondb_fragment_transport::FragmentClient>,
    plan: aiondb_plan::PhysicalPlan,
    context: aiondb_fragment_transport::client::FragmentContext,
) -> DbResult<aiondb_executor::ExecutionResult> {
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::Builder::new()
        .name("aiondb-remote-fragment-client".to_owned())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    DbError::internal(format!(
                        "failed to create tokio runtime for remote fragment execution: {error}"
                    ))
                })
                .and_then(|runtime| runtime.block_on(client.execute(&plan, context)));
            let _ = result_tx.send(result);
        })
        .map_err(|error| {
            DbError::internal(format!(
                "failed to spawn remote fragment client thread: {error}"
            ))
        })?;

    let result = result_rx.recv().map_err(|error| {
        DbError::internal(format!(
            "remote fragment client thread exited without a result: {error}"
        ))
    })?;
    thread
        .join()
        .map_err(|_| DbError::internal("remote fragment client thread panicked"))?;
    result
}

fn build_inter_node_tls_client_config(
    config: &aiondb_config::DistributedConfig,
) -> Option<aiondb_fragment_transport::tls::TlsClientConfig> {
    let ca_cert_path = config.tls_ca_cert_path.as_ref()?;
    Some(aiondb_fragment_transport::tls::TlsClientConfig {
        ca_cert_path: ca_cert_path.clone(),
        client_cert_path: config.tls_cert_path.clone(),
        client_key_path: config.tls_key_path.clone(),
    })
}

fn remote_fragment_client_config_from_runtime(
    runtime_config: &RuntimeConfig,
    node_config: &aiondb_config::RemoteNodeConfig,
) -> aiondb_fragment_transport::client::FragmentClientConfig {
    aiondb_fragment_transport::client::FragmentClientConfig {
        addr: node_config.addr.clone(),
        auth_token: aiondb_fragment_transport::AuthToken::new(
            runtime_config
                .distributed
                .inter_node_auth_token
                .clone()
                .unwrap_or_default(),
        ),
        tls: build_inter_node_tls_client_config(&runtime_config.distributed),
        connect_timeout: runtime_config.distributed.remote_connect_timeout,
        max_retries: runtime_config.distributed.remote_max_retries,
        retry_backoff: runtime_config.distributed.remote_retry_backoff,
    }
}

fn replication_slot_dir(data_dir: &Path, runtime_config: &RuntimeConfig) -> PathBuf {
    storage_backend_wal_dir(data_dir, runtime_config).join("replication_slots")
}

fn load_or_create_system_identifier(data_dir: &Path) -> DbResult<String> {
    let metadata_dir = data_dir.join("replication");
    fs::create_dir_all(&metadata_dir).map_err(|error| {
        DbError::internal(format!(
            "failed to create replication metadata directory {}: {error}",
            metadata_dir.display()
        ))
    })?;
    aiondb_wal::segment::sync_dir(data_dir)?;

    let path = metadata_dir.join("system_id");
    if let Ok(raw) = fs::read_to_string(&path) {
        let id = raw.trim();
        if !id.is_empty() {
            return Ok(id.to_owned());
        }
    }

    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to generate replication system identifier: {error}"
        ))
    })?;
    let system_identifier = u64::from_le_bytes(bytes).to_string();

    let temp_path = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&temp_path)
        .map_err(|error| {
            DbError::internal(format!(
                "failed to create replication system identifier temp file {}: {error}",
                temp_path.display()
            ))
        })?;
    file.write_all(system_identifier.as_bytes())
        .map_err(|error| {
            DbError::internal(format!(
                "failed to write replication system identifier temp file {}: {error}",
                temp_path.display()
            ))
        })?;
    file.flush().map_err(|error| {
        DbError::internal(format!(
            "failed to flush replication system identifier temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        DbError::internal(format!(
            "failed to sync replication system identifier temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    drop(file);
    fs::rename(&temp_path, &path).map_err(|error| {
        DbError::internal(format!(
            "failed to publish replication system identifier {}: {error}",
            path.display()
        ))
    })?;
    aiondb_wal::segment::sync_dir(&metadata_dir)?;
    Ok(system_identifier)
}

fn load_or_create_timeline_id(data_dir: &Path, promote_on_start: bool) -> DbResult<u32> {
    let metadata_dir = data_dir.join("replication");
    fs::create_dir_all(&metadata_dir).map_err(|error| {
        DbError::internal(format!(
            "failed to create replication metadata directory {}: {error}",
            metadata_dir.display()
        ))
    })?;
    aiondb_wal::segment::sync_dir(data_dir)?;

    let path = metadata_dir.join("timeline");
    let mut timeline = fs::read_to_string(&path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1);

    if promote_on_start {
        let parent = timeline;
        timeline = timeline.checked_add(1).ok_or_else(|| {
            DbError::internal("replication timeline overflow while promoting on start")
        })?;
        write_timeline_history_file(&metadata_dir, timeline, parent)?;
    }

    write_replication_metadata_atomic(&path, timeline.to_string().as_bytes(), "timeline")?;
    Ok(timeline)
}

fn write_timeline_history_file(
    metadata_dir: &Path,
    timeline: u32,
    parent_timeline: u32,
) -> DbResult<()> {
    let path = metadata_dir.join(format!("{timeline:08X}.history"));
    let content =
        format!("{parent_timeline:08X}\t0/0\tpromotion from timeline {parent_timeline:08X}\n");
    write_replication_metadata_atomic(&path, content.as_bytes(), "timeline history")
}

fn write_replication_metadata_atomic(path: &Path, bytes: &[u8], label: &str) -> DbResult<()> {
    let temp_path = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&temp_path)
        .map_err(|error| {
            DbError::internal(format!(
                "failed to create replication {label} temp file {}: {error}",
                temp_path.display()
            ))
        })?;
    file.write_all(bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to write replication {label} temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    file.flush().map_err(|error| {
        DbError::internal(format!(
            "failed to flush replication {label} temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        DbError::internal(format!(
            "failed to sync replication {label} temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    drop(file);

    fs::rename(&temp_path, path).map_err(|error| {
        DbError::internal(format!(
            "failed to publish replication {label} {}: {error}",
            path.display()
        ))
    })?;
    if let Some(parent) = path.parent() {
        aiondb_wal::segment::sync_dir(parent)?;
    }
    Ok(())
}

fn storage_backend_spec(data_dir: &Path, runtime_config: &RuntimeConfig) -> StorageBackendSpec {
    match runtime_config.storage.backend {
        StorageBackend::InMemory => StorageBackendSpec::InMemory {
            memory_limit_bytes: None,
        },
        StorageBackend::Durable => {
            StorageBackendSpec::durable(durable_storage_options(data_dir, runtime_config))
        }
        StorageBackend::Disk => StorageBackendSpec::Disk {
            config: DiskBackendConfig {
                buffer_pool: StorageBufferPoolConfig {
                    table_frames: runtime_config.storage.table_pool_frames,
                    snapshot_frames: runtime_config.storage.snapshot_pool_frames,
                    index_frames: runtime_config.storage.table_pool_frames,
                },
                max_open_files: runtime_config.storage.max_open_files,
                wal_group_commit_delay_micros: runtime_config.storage.wal_group_commit_delay_micros,
                ..DiskBackendConfig::new(data_dir.join("disk"))
            },
        },
        StorageBackend::PageEngine => StorageBackendSpec::PageEngine {
            config: PageEngineBackendConfig {
                base_path: data_dir.join("page_engine"),
                page_size: runtime_config.storage.page_size,
                buffer_pool_pages: runtime_config.storage.table_pool_frames
                    + runtime_config.storage.snapshot_pool_frames,
                sync_policy: aiondb_storage_engine::PageSyncPolicy::Always,
                wal_group_commit_delay_micros: runtime_config.storage.wal_group_commit_delay_micros,
            },
        },
        StorageBackend::Lsm => StorageBackendSpec::Lsm {
            config: LsmBackendConfig {
                wal_group_commit_delay_micros: runtime_config.storage.wal_group_commit_delay_micros,
                ..LsmBackendConfig::new(data_dir.join("lsm"))
            },
        },
    }
}

fn storage_backend_kind(backend: StorageBackend) -> aiondb_storage_engine::StorageBackendKind {
    match backend {
        StorageBackend::InMemory => aiondb_storage_engine::StorageBackendKind::InMemory,
        StorageBackend::Durable => aiondb_storage_engine::StorageBackendKind::Durable,
        StorageBackend::Disk => aiondb_storage_engine::StorageBackendKind::Disk,
        StorageBackend::PageEngine => aiondb_storage_engine::StorageBackendKind::PageEngine,
        StorageBackend::Lsm => aiondb_storage_engine::StorageBackendKind::Lsm,
    }
}

fn durable_storage_options(data_dir: &Path, runtime_config: &RuntimeConfig) -> StorageOptions {
    let (wal_commit_policy, sync_on_flush) =
        durable_wal_commit_policy(runtime_config.storage.durable_wal_commit_policy);
    let wal_config = WalConfig {
        dir: data_dir.join("wal"),
        sync_on_flush,
        group_commit_delay_micros: effective_group_commit_delay_micros(runtime_config),
        wal_compression: wal_compression_mode(runtime_config),
        wal_lsn_mode: wal_lsn_mode(runtime_config.replication.wal_lsn_mode),
        ..WalConfig::default()
    };
    let mut storage_options = StorageOptions::durable(wal_config);
    storage_options.wal_commit_policy = wal_commit_policy;
    #[cfg(not(test))]
    {
        storage_options.persist_paged_state_on_commit =
            runtime_paged_state_on_commit_enabled(false);
    }
    storage_options.buffer_pool = StorageBufferPoolConfig {
        table_frames: runtime_config.storage.table_pool_frames,
        snapshot_frames: runtime_config.storage.snapshot_pool_frames,
        index_frames: runtime_config.storage.table_pool_frames,
    };
    storage_options.max_open_files = runtime_config.storage.max_open_files;
    storage_options.min_wal_keep_segments = runtime_config.replication.wal_keep_segments;
    storage_options
}

#[cfg(not(test))]
fn runtime_paged_state_on_commit_enabled(default: bool) -> bool {
    std::env::var("AIONDB_PERSIST_PAGED_STATE_ON_COMMIT")
        .ok()
        .map_or(default, |value| {
            let normalized = value.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "0" | "false" | "no" | "off")
        })
}

fn durable_wal_commit_policy(policy: ConfigDurableWalCommitPolicy) -> (WalCommitPolicy, bool) {
    match policy {
        ConfigDurableWalCommitPolicy::Always => (WalCommitPolicy::Always, true),
        ConfigDurableWalCommitPolicy::Every(interval) => (WalCommitPolicy::Every(interval), false),
        ConfigDurableWalCommitPolicy::Never => (WalCommitPolicy::Never, false),
    }
}

fn effective_group_commit_delay_micros(runtime_config: &RuntimeConfig) -> u64 {
    let configured = runtime_config.storage.wal_group_commit_delay_micros;
    match runtime_config.storage.durable_wal_commit_policy {
        ConfigDurableWalCommitPolicy::Always => configured,
        ConfigDurableWalCommitPolicy::Every(_) | ConfigDurableWalCommitPolicy::Never => {
            if configured == DEFAULT_STORAGE_WAL_GROUP_COMMIT_DELAY_MICROS {
                0
            } else {
                configured
            }
        }
    }
}

fn wal_compression_mode(runtime_config: &RuntimeConfig) -> aiondb_wal::WalCompression {
    match runtime_config.replication.wal_compression {
        ConfigWalCompression::None => aiondb_wal::WalCompression::None,
        ConfigWalCompression::Lz4 => aiondb_wal::WalCompression::Lz4,
        ConfigWalCompression::Zstd => aiondb_wal::WalCompression::Zstd,
    }
}

fn wal_lsn_mode(mode: ConfigWalLsnMode) -> aiondb_wal::WalLsnMode {
    match mode {
        ConfigWalLsnMode::Logical => aiondb_wal::WalLsnMode::Logical,
        ConfigWalLsnMode::ByteOffset => aiondb_wal::WalLsnMode::ByteOffset,
    }
}

fn default_rate_limiter(runtime_config: &RuntimeConfig) -> Arc<dyn AuthRateLimiter> {
    if runtime_config.security.durable_auth_lockout {
        let state_path = runtime_config
            .security
            .auth_lockout_state_path
            .clone()
            .unwrap_or_else(|| {
                runtime_config
                    .storage
                    .data_dir
                    .join("security")
                    .join("auth_lockout_state.tsv")
            });
        Arc::new(FileBackedAuthRateLimiter::new(
            runtime_config.security.max_auth_failures,
            runtime_config.security.auth_lockout_window,
            state_path,
        ))
    } else {
        Arc::new(InMemoryAuthRateLimiter::new(
            runtime_config.security.max_auth_failures,
            runtime_config.security.auth_lockout_window,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::unique_temp_path;
    use aiondb_cluster::{MetadataReader, MetadataWriter};
    use aiondb_core::DbError;
    use aiondb_executor::{
        DistributedFragment, ExecutionContext, ExecutionResult, Executor, FragmentDispatcher,
        FragmentTarget,
    };

    #[derive(Debug)]
    struct PassthroughFragmentDispatcher;

    impl FragmentDispatcher for PassthroughFragmentDispatcher {
        fn execute_fragment(
            &self,
            fragment: &DistributedFragment,
            executor: &Executor,
            context: &ExecutionContext,
        ) -> DbResult<ExecutionResult> {
            match &fragment.target {
                FragmentTarget::Local | FragmentTarget::Remote(_) => {
                    executor.execute(&fragment.plan, context)
                }
            }
        }
    }

    #[test]
    fn remote_fragment_client_blocking_is_safe_inside_tokio_runtime() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should build");
        let client = Arc::new(aiondb_fragment_transport::FragmentClient::new(
            aiondb_fragment_transport::client::FragmentClientConfig::new(
                "127.0.0.1:9",
                aiondb_fragment_transport::AuthToken::new("tok"),
            )
            .with_connect_timeout(std::time::Duration::from_millis(1))
            .with_max_retries(0),
        ));
        let plan = aiondb_plan::PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        };
        let context = aiondb_fragment_transport::client::FragmentContext {
            txn_id: 1,
            isolation: "ReadCommitted".to_owned(),
            max_result_rows: 1,
            max_result_bytes: 1024,
            max_memory_bytes: 1024,
            max_temp_bytes: 1024,
            snapshot: None,
            shard_id: Some(3),
            deadline_epoch_ms: None,
        };

        let result = runtime
            .block_on(async { execute_remote_fragment_client_blocking(client, plan, context) });

        assert!(
            result.is_err(),
            "unreachable test endpoint should return a connection error, not panic"
        );
    }

    #[test]
    fn durable_storage_spec_uses_runtime_pool_sizes() -> DbResult<()> {
        let mut runtime = RuntimeConfig::default();
        runtime.storage.backend = StorageBackend::Durable;
        runtime.storage.table_pool_frames = 320;
        runtime.storage.snapshot_pool_frames = 80;
        runtime.storage.max_open_files = 96;
        runtime.storage.wal_group_commit_delay_micros = 250;
        runtime.replication.wal_keep_segments = 48;
        runtime.replication.wal_compression = ConfigWalCompression::Lz4;
        runtime.replication.wal_lsn_mode = ConfigWalLsnMode::Logical;

        let spec = storage_backend_spec(Path::new("/srv/aiondb"), &runtime);
        let StorageBackendSpec::Durable { options } = spec else {
            return Err(DbError::internal("expected durable storage spec"));
        };

        assert_eq!(options.wal_config.dir, PathBuf::from("/srv/aiondb/wal"));
        assert_eq!(
            options.wal_config.wal_compression,
            aiondb_wal::WalCompression::Lz4
        );
        assert_eq!(
            options.wal_config.wal_lsn_mode,
            aiondb_wal::WalLsnMode::Logical
        );
        assert_eq!(options.wal_commit_policy, WalCommitPolicy::Always);
        assert!(options.wal_config.sync_on_flush);
        assert_eq!(options.wal_config.group_commit_delay_micros, 250);
        assert_eq!(options.buffer_pool.table_frames, 320);
        assert_eq!(options.buffer_pool.snapshot_frames, 80);
        assert_eq!(options.max_open_files, 96);
        assert_eq!(options.min_wal_keep_segments, 48);
        Ok(())
    }

    #[test]
    fn durable_storage_spec_maps_relaxed_wal_commit_policy() -> DbResult<()> {
        let mut runtime = RuntimeConfig::default();
        runtime.storage.backend = StorageBackend::Durable;
        runtime.storage.durable_wal_commit_policy = ConfigDurableWalCommitPolicy::Every(64);

        let spec = storage_backend_spec(Path::new("/srv/aiondb"), &runtime);
        let StorageBackendSpec::Durable { options } = spec else {
            return Err(DbError::internal("expected durable storage spec"));
        };

        assert_eq!(options.wal_commit_policy, WalCommitPolicy::Every(64));
        assert!(!options.wal_config.sync_on_flush);
        assert_eq!(options.wal_config.group_commit_delay_micros, 0);
        Ok(())
    }

    #[test]
    fn durable_storage_spec_respects_explicit_group_commit_delay_with_relaxed_policy(
    ) -> DbResult<()> {
        let mut runtime = RuntimeConfig::default();
        runtime.storage.backend = StorageBackend::Durable;
        runtime.storage.durable_wal_commit_policy = ConfigDurableWalCommitPolicy::Every(64);
        runtime.storage.wal_group_commit_delay_micros = 400;

        let spec = storage_backend_spec(Path::new("/srv/aiondb"), &runtime);
        let StorageBackendSpec::Durable { options } = spec else {
            return Err(DbError::internal("expected durable storage spec"));
        };

        assert_eq!(options.wal_commit_policy, WalCommitPolicy::Every(64));
        assert_eq!(options.wal_config.group_commit_delay_micros, 400);
        Ok(())
    }

    #[test]
    fn page_engine_storage_spec_uses_dedicated_subdir() -> DbResult<()> {
        let mut runtime = RuntimeConfig::default();
        runtime.storage.backend = StorageBackend::PageEngine;
        runtime.storage.table_pool_frames = 256;
        runtime.storage.snapshot_pool_frames = 64;
        runtime.storage.wal_group_commit_delay_micros = 333;

        let spec = storage_backend_spec(Path::new("/srv/aiondb"), &runtime);
        let StorageBackendSpec::PageEngine { config } = spec else {
            return Err(DbError::internal("expected page engine storage spec"));
        };

        assert_eq!(config.base_path, PathBuf::from("/srv/aiondb/page_engine"));
        assert_eq!(config.page_size, runtime.storage.page_size);
        assert_eq!(config.buffer_pool_pages, 320);
        assert_eq!(config.wal_group_commit_delay_micros, 333);
        Ok(())
    }

    #[test]
    fn disk_storage_spec_uses_dedicated_subdir() -> DbResult<()> {
        let mut runtime = RuntimeConfig::default();
        runtime.storage.backend = StorageBackend::Disk;
        runtime.storage.table_pool_frames = 384;
        runtime.storage.snapshot_pool_frames = 96;
        runtime.storage.max_open_files = 48;
        runtime.storage.wal_group_commit_delay_micros = 444;

        let spec = storage_backend_spec(Path::new("/srv/aiondb"), &runtime);
        let StorageBackendSpec::Disk { config } = spec else {
            return Err(DbError::internal("expected disk storage spec"));
        };

        assert_eq!(config.path, PathBuf::from("/srv/aiondb/disk"));
        assert_eq!(config.buffer_pool.table_frames, 384);
        assert_eq!(config.buffer_pool.snapshot_frames, 96);
        assert_eq!(config.max_open_files, 48);
        assert_eq!(config.wal_group_commit_delay_micros, 444);
        Ok(())
    }

    #[test]
    fn lsm_storage_spec_uses_dedicated_subdir() -> DbResult<()> {
        let mut runtime = RuntimeConfig::default();
        runtime.storage.backend = StorageBackend::Lsm;
        runtime.storage.wal_group_commit_delay_micros = 555;

        let spec = storage_backend_spec(Path::new("/srv/aiondb"), &runtime);
        let StorageBackendSpec::Lsm { config } = spec else {
            return Err(DbError::internal("expected lsm storage spec"));
        };

        assert_eq!(config.base_dir, PathBuf::from("/srv/aiondb/lsm"));
        assert_eq!(config.wal_group_commit_delay_micros, 555);
        Ok(())
    }

    #[test]
    fn build_accepts_custom_fragment_dispatcher() -> DbResult<()> {
        let _engine = EngineBuilder::for_testing()
            .with_fragment_dispatcher(Arc::new(PassthroughFragmentDispatcher))
            .build()?;
        Ok(())
    }

    #[test]
    fn configured_fragment_dispatcher_uses_distributed_loopback_nodes() {
        let mut runtime = RuntimeConfig::default();
        runtime.distributed.loopback_remote_nodes = vec!["node-a".to_owned(), "node-b".to_owned()];
        assert!(configured_fragment_dispatcher_from_config(&runtime, None)
            .expect("should succeed")
            .is_some());
    }

    #[test]
    fn configured_fragment_dispatcher_none_when_no_distributed_nodes() {
        let runtime = RuntimeConfig::default();
        assert!(configured_fragment_dispatcher_from_config(&runtime, None)
            .expect("should succeed")
            .is_none());
    }

    #[test]
    fn configured_fragment_dispatcher_uses_default_handler_when_unregistered_enabled() {
        let mut runtime = RuntimeConfig::default();
        runtime.distributed.allow_unregistered_loopback_nodes = true;
        assert!(configured_fragment_dispatcher_from_config(&runtime, None)
            .expect("should succeed")
            .is_some());
    }

    #[test]
    fn configured_fragment_dispatcher_rejects_remote_nodes_without_auth_token() {
        let mut runtime = RuntimeConfig::default();
        runtime.distributed.remote_nodes = vec![aiondb_config::RemoteNodeConfig {
            node_id: "node-b".to_owned(),
            addr: "127.0.0.1:9100".to_owned(),
        }];

        assert!(configured_fragment_dispatcher_from_config(&runtime, None).is_err());
    }

    #[test]
    fn configured_fragment_dispatcher_rejects_short_remote_auth_token() {
        let mut runtime = RuntimeConfig::default();
        runtime.distributed.remote_nodes = vec![aiondb_config::RemoteNodeConfig {
            node_id: "node-b".to_owned(),
            addr: "127.0.0.1:9100".to_owned(),
        }];
        runtime.distributed.inter_node_auth_token = Some("too-short".to_owned());
        runtime.distributed.require_tls = false;

        assert!(configured_fragment_dispatcher_from_config(&runtime, None).is_err());
    }

    #[test]
    fn configured_fragment_dispatcher_registers_remote_nodes_in_node_registry() {
        let mut runtime = RuntimeConfig::default();
        runtime.distributed.remote_nodes = vec![aiondb_config::RemoteNodeConfig {
            node_id: "node-b".to_owned(),
            addr: "127.0.0.1:9100".to_owned(),
        }];
        runtime.distributed.inter_node_auth_token = Some(TEST_INTER_NODE_AUTH_TOKEN.to_owned());
        runtime.distributed.require_tls = false;
        let registry = Arc::new(NodeRegistry::with_circuit_breaker_config(
            1,
            std::time::Duration::from_secs(60),
        ));

        let dispatcher =
            configured_fragment_dispatcher_from_config(&runtime, Some(Arc::clone(&registry)))
                .expect("remote dispatcher should build");

        assert!(dispatcher.is_some());
        let entry = registry.get("node-b").expect("remote node registered");
        assert_eq!(entry.addr, "127.0.0.1:9100");
    }

    #[test]
    fn remote_fragment_client_config_uses_runtime_retry_settings() {
        let mut runtime = RuntimeConfig::default();
        runtime.distributed.inter_node_auth_token = Some(TEST_INTER_NODE_AUTH_TOKEN.to_owned());
        runtime.distributed.remote_connect_timeout = std::time::Duration::from_millis(250);
        runtime.distributed.remote_max_retries = 1;
        runtime.distributed.remote_retry_backoff = std::time::Duration::from_millis(25);
        let node = aiondb_config::RemoteNodeConfig {
            node_id: "node-b".to_owned(),
            addr: "127.0.0.1:9100".to_owned(),
        };

        let config = remote_fragment_client_config_from_runtime(&runtime, &node);

        assert_eq!(config.addr, "127.0.0.1:9100");
        assert_eq!(
            config.connect_timeout,
            std::time::Duration::from_millis(250)
        );
        assert_eq!(config.max_retries, 1);
        assert_eq!(config.retry_backoff, std::time::Duration::from_millis(25));
    }

    #[test]
    fn build_with_remote_nodes_exposes_distributed_registry_snapshot() -> DbResult<()> {
        let mut runtime = RuntimeConfig::default();
        runtime.security = SecurityConfig::from_profile(SecurityProfile::Development);
        runtime.security.allow_ephemeral_users = true;
        runtime.distributed.remote_nodes = vec![aiondb_config::RemoteNodeConfig {
            node_id: "node-b".to_owned(),
            addr: "127.0.0.1:9100".to_owned(),
        }];
        runtime.distributed.inter_node_auth_token = Some(TEST_INTER_NODE_AUTH_TOKEN.to_owned());
        runtime.distributed.require_tls = false;

        let engine = EngineBuilder::for_testing()
            .with_runtime_config(runtime)
            .build()?;
        let snapshot = engine
            .distributed_node_registry_snapshot()
            .expect("remote nodes should create a registry snapshot");

        assert_eq!(snapshot.total_nodes, 1);
        assert_eq!(snapshot.available_nodes, 1);
        assert_eq!(snapshot.nodes[0].node_id, "node-b");
        assert_eq!(snapshot.nodes[0].addr, "127.0.0.1:9100");
        Ok(())
    }

    #[test]
    fn build_with_remote_nodes_exposes_distributed_control_plane_snapshot() -> DbResult<()> {
        let mut runtime = RuntimeConfig::default();
        runtime.security = SecurityConfig::from_profile(SecurityProfile::Development);
        runtime.security.allow_ephemeral_users = true;
        runtime.distributed.fragment_transport_port = 9444;
        runtime.distributed.remote_nodes = vec![aiondb_config::RemoteNodeConfig {
            node_id: "node-b".to_owned(),
            addr: "127.0.0.1:9100".to_owned(),
        }];
        runtime.distributed.inter_node_auth_token = Some(TEST_INTER_NODE_AUTH_TOKEN.to_owned());
        runtime.distributed.require_tls = false;

        let engine = EngineBuilder::for_testing()
            .with_runtime_config(runtime)
            .build()?;
        let snapshot = engine.distributed_control_plane_snapshot()?;

        assert_eq!(snapshot.total_nodes, 2);
        assert_eq!(snapshot.live_nodes, 2);
        assert_eq!(snapshot.nodes[0].node_id.as_str(), "local");
        assert_eq!(snapshot.nodes[0].rpc_endpoint, "127.0.0.1:9444");
        assert_eq!(snapshot.nodes[1].node_id.as_str(), "node-b");
        assert_eq!(snapshot.nodes[1].rpc_endpoint, "127.0.0.1:9100");
        Ok(())
    }

    #[test]
    fn distributed_control_plane_shard_leaders_feed_engine_runtime_context() -> DbResult<()> {
        let mut runtime = RuntimeConfig::default();
        runtime.security = SecurityConfig::from_profile(SecurityProfile::Development);
        runtime.security.allow_ephemeral_users = true;
        runtime.distributed.remote_nodes = vec![aiondb_config::RemoteNodeConfig {
            node_id: "node-b".to_owned(),
            addr: "127.0.0.1:9100".to_owned(),
        }];
        runtime.distributed.inter_node_auth_token = Some(TEST_INTER_NODE_AUTH_TOKEN.to_owned());
        runtime.distributed.require_tls = false;
        let engine = EngineBuilder::for_testing()
            .with_runtime_config(runtime)
            .build()?;

        engine
            .distributed_control_plane()
            .upsert_shard(aiondb_cluster::ShardDescriptor {
                database_id: aiondb_cluster::DatabaseId::DEFAULT,
                table_id: aiondb_core::RelationId::new(42),
                shard_id: aiondb_cluster::ShardId::new(7),
                placements: vec![aiondb_cluster::ShardPlacement {
                    shard_id: aiondb_cluster::ShardId::new(7),
                    node_id: aiondb_cluster::NodeId::new("node-b"),
                    role: aiondb_cluster::ReplicaRole::Leader,
                    lease_epoch: aiondb_cluster::PlacementEpoch::default(),
                }],
            })?;

        assert_eq!(
            engine
                .distributed_shard_leader_nodes_for_database(aiondb_cluster::DatabaseId::DEFAULT)?,
            vec![(7, "node-b".to_owned())]
        );
        assert_eq!(engine.distributed_control_plane_snapshot()?.total_shards, 1);
        Ok(())
    }

    fn repairable_distributed_runtime(auto_rebalance: bool) -> RuntimeConfig {
        let mut runtime = RuntimeConfig::default();
        runtime.security = SecurityConfig::from_profile(SecurityProfile::Development);
        runtime.security.allow_ephemeral_users = true;
        runtime.distributed.sharding.enabled = true;
        runtime.distributed.sharding.replication_factor = 2;
        runtime.distributed.sharding.auto_rebalance = auto_rebalance;
        runtime.distributed.remote_nodes = vec![
            aiondb_config::RemoteNodeConfig {
                node_id: "node-b".to_owned(),
                addr: "127.0.0.1:9100".to_owned(),
            },
            aiondb_config::RemoteNodeConfig {
                node_id: "node-c".to_owned(),
                addr: "127.0.0.1:9101".to_owned(),
            },
            aiondb_config::RemoteNodeConfig {
                node_id: "node-d".to_owned(),
                addr: "127.0.0.1:9102".to_owned(),
            },
        ];
        runtime.distributed.inter_node_auth_token = Some(TEST_INTER_NODE_AUTH_TOKEN.to_owned());
        runtime.distributed.require_tls = false;
        runtime
    }

    fn seed_repairable_distributed_shard(engine: &Engine) -> DbResult<()> {
        engine
            .distributed_control_plane()
            .upsert_shard(aiondb_cluster::ShardDescriptor {
                database_id: aiondb_cluster::DatabaseId::DEFAULT,
                table_id: aiondb_core::RelationId::new(42),
                shard_id: aiondb_cluster::ShardId::new(7),
                placements: vec![
                    aiondb_cluster::ShardPlacement {
                        shard_id: aiondb_cluster::ShardId::new(7),
                        node_id: aiondb_cluster::NodeId::new("local"),
                        role: aiondb_cluster::ReplicaRole::Leader,
                        lease_epoch: aiondb_cluster::PlacementEpoch::default(),
                    },
                    aiondb_cluster::ShardPlacement {
                        shard_id: aiondb_cluster::ShardId::new(7),
                        node_id: aiondb_cluster::NodeId::new("node-b"),
                        role: aiondb_cluster::ReplicaRole::Follower,
                        lease_epoch: aiondb_cluster::PlacementEpoch::default(),
                    },
                    aiondb_cluster::ShardPlacement {
                        shard_id: aiondb_cluster::ShardId::new(7),
                        node_id: aiondb_cluster::NodeId::new("node-c"),
                        role: aiondb_cluster::ReplicaRole::Follower,
                        lease_epoch: aiondb_cluster::PlacementEpoch::default(),
                    },
                ],
            })
    }

    #[test]
    fn engine_replication_maintenance_options_use_configured_learner_throttles() -> DbResult<()> {
        let mut runtime = repairable_distributed_runtime(true);
        runtime.distributed.sharding.max_learners_per_shard = 2;
        runtime.distributed.sharding.max_learners_per_node = 7;
        runtime
            .distributed
            .sharding
            .leadership_max_transfers_per_maintenance = 3;
        runtime.distributed.sharding.leadership_min_load_delta = 2;
        let engine = EngineBuilder::for_testing()
            .with_runtime_config(runtime)
            .build()?;

        let options = engine.configured_distributed_replication_maintenance_options();

        assert_eq!(
            options.replica_repair.repair_mode,
            aiondb_cluster::ReplicaRepairMode::LearnerFirst
        );
        assert_eq!(options.replica_repair.max_learners_per_shard, 2);
        assert_eq!(options.replica_repair.max_learners_per_node, 7);
        assert_eq!(options.leadership_balance.max_transfers, 3);
        assert_eq!(options.leadership_balance.min_load_delta, 2);
        Ok(())
    }

    #[test]
    fn engine_replica_placement_policy_uses_configured_attributes() -> DbResult<()> {
        let mut runtime = repairable_distributed_runtime(true);
        runtime.distributed.sharding.node_attributes = std::collections::BTreeMap::from([(
            "local".to_owned(),
            std::collections::BTreeMap::from([
                ("region".to_owned(), "eu-west".to_owned()),
                ("zone".to_owned(), "az-a".to_owned()),
            ]),
        )]);
        runtime.distributed.sharding.placement_required_attributes =
            vec![aiondb_shard::PlacementAttributeConstraint {
                key: "disk".to_owned(),
                value: "ssd".to_owned(),
            }];
        runtime.distributed.sharding.lease_preference_attributes =
            vec![aiondb_shard::PlacementAttributeConstraint {
                key: "region".to_owned(),
                value: "eu-west".to_owned(),
            }];
        runtime.distributed.sharding.placement_spread_attributes =
            vec!["region".to_owned(), "zone".to_owned()];
        let engine = EngineBuilder::for_testing()
            .with_runtime_config(runtime)
            .build()?;

        let policy = engine.configured_distributed_replica_placement_policy();

        assert_eq!(
            policy.node_attributes[&aiondb_cluster::NodeId::local()]["region"],
            "eu-west"
        );
        assert_eq!(policy.required_attributes[0].key, "disk");
        assert_eq!(policy.lease_preferences[0].value, "eu-west");
        assert_eq!(
            policy.spread_attributes,
            vec!["region".to_owned(), "zone".to_owned()]
        );
        Ok(())
    }

    #[test]
    fn engine_mark_node_down_runs_configured_auto_rebalance() -> DbResult<()> {
        let engine = EngineBuilder::for_testing()
            .with_runtime_config(repairable_distributed_runtime(true))
            .build()?;

        seed_repairable_distributed_shard(&engine)?;

        let outcome = engine.mark_distributed_node_live_and_maintain(
            aiondb_cluster::NodeId::new("node-b"),
            false,
            aiondb_cluster::DatabaseId::DEFAULT,
        )?;

        let maintenance = outcome
            .maintenance
            .expect("auto rebalance should run maintenance");
        assert_eq!(maintenance.replica_repairs.len(), 1);
        assert_eq!(
            maintenance.replica_repairs[0].status,
            aiondb_cluster::ReplicaRepairStatus::Applied
        );
        let placements = engine
            .distributed_control_plane()
            .table_shards(
                aiondb_cluster::DatabaseId::DEFAULT,
                aiondb_core::RelationId::new(42),
            )?
            .pop()
            .unwrap()
            .placements;
        assert!(placements.iter().any(|placement| {
            placement.node_id == aiondb_cluster::NodeId::new("node-d")
                && placement.role == aiondb_cluster::ReplicaRole::Learner
        }));
        assert!(!placements
            .iter()
            .any(|placement| placement.node_id == aiondb_cluster::NodeId::new("node-b")));
        Ok(())
    }

    #[test]
    fn engine_promotes_caught_up_staged_replica() -> DbResult<()> {
        let engine = EngineBuilder::for_testing()
            .with_runtime_config(repairable_distributed_runtime(true))
            .build()?;

        seed_repairable_distributed_shard(&engine)?;
        engine.mark_distributed_node_live_and_maintain(
            aiondb_cluster::NodeId::new("node-b"),
            false,
            aiondb_cluster::DatabaseId::DEFAULT,
        )?;

        let caught_up = std::collections::BTreeSet::from([aiondb_cluster::ReplicaCatchupKey::new(
            aiondb_cluster::DatabaseId::DEFAULT,
            aiondb_core::RelationId::new(42),
            aiondb_cluster::ShardId::new(7),
            aiondb_cluster::NodeId::new("node-d"),
        )]);
        let outcome = engine.maintain_distributed_replication_with_caught_up_learners(
            aiondb_cluster::DatabaseId::DEFAULT,
            &caught_up,
            engine.configured_distributed_replication_maintenance_options(),
        )?;

        assert_eq!(outcome.replica_repairs.len(), 1);
        assert_eq!(
            outcome.replica_repairs[0].status,
            aiondb_cluster::ReplicaRepairStatus::Applied
        );
        let placements = engine
            .distributed_control_plane()
            .table_shards(
                aiondb_cluster::DatabaseId::DEFAULT,
                aiondb_core::RelationId::new(42),
            )?
            .pop()
            .unwrap()
            .placements;
        assert!(placements.iter().any(|placement| {
            placement.node_id == aiondb_cluster::NodeId::new("node-d")
                && placement.role == aiondb_cluster::ReplicaRole::Follower
        }));
        Ok(())
    }

    #[test]
    fn engine_promotes_caught_up_nodes_without_shard_keys() -> DbResult<()> {
        let engine = EngineBuilder::for_testing()
            .with_runtime_config(repairable_distributed_runtime(true))
            .build()?;

        seed_repairable_distributed_shard(&engine)?;
        engine.mark_distributed_node_live_and_maintain(
            aiondb_cluster::NodeId::new("node-b"),
            false,
            aiondb_cluster::DatabaseId::DEFAULT,
        )?;

        let caught_up_nodes =
            std::collections::BTreeSet::from([aiondb_cluster::NodeId::new("node-d")]);
        let caught_up_keys = engine.distributed_caught_up_learner_keys_for_nodes(
            aiondb_cluster::DatabaseId::DEFAULT,
            &caught_up_nodes,
        )?;

        assert_eq!(
            caught_up_keys,
            std::collections::BTreeSet::from([aiondb_cluster::ReplicaCatchupKey::new(
                aiondb_cluster::DatabaseId::DEFAULT,
                aiondb_core::RelationId::new(42),
                aiondb_cluster::ShardId::new(7),
                aiondb_cluster::NodeId::new("node-d"),
            )])
        );

        let outcome = engine.maintain_distributed_replication_from_config_with_caught_up_nodes(
            aiondb_cluster::DatabaseId::DEFAULT,
            &caught_up_nodes,
        )?;

        assert_eq!(outcome.replica_repairs.len(), 1);
        assert_eq!(
            outcome.replica_repairs[0].status,
            aiondb_cluster::ReplicaRepairStatus::Applied
        );
        let placements = engine
            .distributed_control_plane()
            .table_shards(
                aiondb_cluster::DatabaseId::DEFAULT,
                aiondb_core::RelationId::new(42),
            )?
            .pop()
            .unwrap()
            .placements;
        assert!(placements.iter().any(|placement| {
            placement.node_id == aiondb_cluster::NodeId::new("node-d")
                && placement.role == aiondb_cluster::ReplicaRole::Follower
        }));
        Ok(())
    }

    #[test]
    fn engine_promotes_learner_from_primary_replica_progress() -> DbResult<()> {
        let data_dir = unique_temp_path("engine-builder", "primary-progress-promotion");
        let mut runtime = repairable_distributed_runtime(true);
        runtime.replication.role = aiondb_config::ReplicationRole::Primary;
        let engine = EngineBuilder::new_durable_with_config(data_dir.clone(), runtime)?.build()?;

        seed_repairable_distributed_shard(&engine)?;
        engine.mark_distributed_node_live_and_maintain(
            aiondb_cluster::NodeId::new("node-b"),
            false,
            aiondb_cluster::DatabaseId::DEFAULT,
        )?;

        let manager = crate::QueryEngine::replication_manager(&engine)
            .expect("primary engine should expose replication manager");
        let (_sender, replica_id) = manager.create_wal_sender(aiondb_wal::Lsn::new(1))?;
        manager
            .state()
            .replica_registry()
            .set_application_name(replica_id, "node-d".to_owned());
        manager.state().replica_registry().update_progress(
            replica_id,
            aiondb_wal::Lsn::new(20),
            aiondb_wal::Lsn::new(20),
            aiondb_wal::Lsn::new(20),
        )?;

        let caught_up_nodes = engine.distributed_caught_up_nodes_from_primary_progress(20)?;
        assert_eq!(
            caught_up_nodes,
            std::collections::BTreeSet::from([aiondb_cluster::NodeId::new("node-d")])
        );

        let outcome = engine.maintain_distributed_replication_from_config_with_primary_progress(
            aiondb_cluster::DatabaseId::DEFAULT,
            20,
        )?;

        assert_eq!(outcome.replica_repairs.len(), 1);
        assert_eq!(
            outcome.replica_repairs[0].status,
            aiondb_cluster::ReplicaRepairStatus::Applied
        );
        let placements = engine
            .distributed_control_plane()
            .table_shards(
                aiondb_cluster::DatabaseId::DEFAULT,
                aiondb_core::RelationId::new(42),
            )?
            .pop()
            .unwrap()
            .placements;
        assert!(placements.iter().any(|placement| {
            placement.node_id == aiondb_cluster::NodeId::new("node-d")
                && placement.role == aiondb_cluster::ReplicaRole::Follower
        }));

        let _ = std::fs::remove_dir_all(data_dir);
        Ok(())
    }

    #[test]
    fn engine_primary_progress_prefers_application_name_over_slot_name() -> DbResult<()> {
        let data_dir = unique_temp_path("engine-builder", "primary-progress-identity");
        let mut runtime = repairable_distributed_runtime(true);
        runtime.replication.role = aiondb_config::ReplicationRole::Primary;
        let engine = EngineBuilder::new_durable_with_config(data_dir.clone(), runtime)?.build()?;

        seed_repairable_distributed_shard(&engine)?;
        engine.mark_distributed_node_live_and_maintain(
            aiondb_cluster::NodeId::new("node-b"),
            false,
            aiondb_cluster::DatabaseId::DEFAULT,
        )?;

        let manager = crate::QueryEngine::replication_manager(&engine)
            .expect("primary engine should expose replication manager");
        manager.create_physical_slot("node_c", false)?;
        let (_sender, replica_id) =
            manager.create_wal_sender_for_slot(aiondb_wal::Lsn::new(1), Some("node_c"))?;
        manager
            .state()
            .replica_registry()
            .set_application_name(replica_id, "node-d".to_owned());
        manager.state().replica_registry().update_progress(
            replica_id,
            aiondb_wal::Lsn::new(30),
            aiondb_wal::Lsn::new(30),
            aiondb_wal::Lsn::new(30),
        )?;

        let caught_up_nodes = engine.distributed_caught_up_nodes_from_primary_progress(30)?;
        assert_eq!(
            caught_up_nodes,
            std::collections::BTreeSet::from([aiondb_cluster::NodeId::new("node-d")])
        );

        let _ = std::fs::remove_dir_all(data_dir);
        Ok(())
    }

    #[test]
    fn engine_primary_progress_uses_slot_name_and_filters_unsafe_nodes() -> DbResult<()> {
        let data_dir = unique_temp_path("engine-builder", "primary-progress-filter");
        let mut runtime = repairable_distributed_runtime(true);
        runtime.replication.role = aiondb_config::ReplicationRole::Primary;
        runtime
            .distributed
            .remote_nodes
            .push(aiondb_config::RemoteNodeConfig {
                node_id: "node_d".to_owned(),
                addr: "127.0.0.1:9103".to_owned(),
            });
        let engine = EngineBuilder::new_durable_with_config(data_dir.clone(), runtime)?.build()?;
        let manager = crate::QueryEngine::replication_manager(&engine)
            .expect("primary engine should expose replication manager");

        engine
            .distributed_control_plane()
            .mark_node_live(&aiondb_cluster::NodeId::new("node-b"), false)?;

        let (_sender, node_d_replica) = {
            manager.create_physical_slot("node_d", false)?;
            manager.create_wal_sender_for_slot(aiondb_wal::Lsn::new(1), Some("node_d"))?
        };
        manager.state().replica_registry().update_progress(
            node_d_replica,
            aiondb_wal::Lsn::new(30),
            aiondb_wal::Lsn::new(30),
            aiondb_wal::Lsn::new(30),
        )?;

        let (_sender, node_c_replica) = manager.create_wal_sender(aiondb_wal::Lsn::new(1))?;
        manager
            .state()
            .replica_registry()
            .set_application_name(node_c_replica, "node-c".to_owned());
        manager.state().replica_registry().update_progress(
            node_c_replica,
            aiondb_wal::Lsn::new(30),
            aiondb_wal::Lsn::new(30),
            aiondb_wal::Lsn::new(29),
        )?;

        let (_sender, down_replica) = manager.create_wal_sender(aiondb_wal::Lsn::new(1))?;
        manager
            .state()
            .replica_registry()
            .set_application_name(down_replica, "node-b".to_owned());
        manager.state().replica_registry().update_progress(
            down_replica,
            aiondb_wal::Lsn::new(30),
            aiondb_wal::Lsn::new(30),
            aiondb_wal::Lsn::new(30),
        )?;

        let (_sender, unknown_replica) = manager.create_wal_sender(aiondb_wal::Lsn::new(1))?;
        manager
            .state()
            .replica_registry()
            .set_application_name(unknown_replica, "node-z".to_owned());
        manager.state().replica_registry().update_progress(
            unknown_replica,
            aiondb_wal::Lsn::new(30),
            aiondb_wal::Lsn::new(30),
            aiondb_wal::Lsn::new(30),
        )?;

        let caught_up_nodes = engine.distributed_caught_up_nodes_from_primary_progress(30)?;
        assert_eq!(
            caught_up_nodes,
            std::collections::BTreeSet::from([aiondb_cluster::NodeId::new("node_d")])
        );

        let _ = std::fs::remove_dir_all(data_dir);
        Ok(())
    }

    #[test]
    fn engine_mark_node_down_skips_auto_rebalance_when_disabled() -> DbResult<()> {
        let engine = EngineBuilder::for_testing()
            .with_runtime_config(repairable_distributed_runtime(false))
            .build()?;

        seed_repairable_distributed_shard(&engine)?;

        let outcome = engine.mark_distributed_node_live_and_maintain(
            aiondb_cluster::NodeId::new("node-b"),
            false,
            aiondb_cluster::DatabaseId::DEFAULT,
        )?;

        assert!(outcome.maintenance.is_none());
        let placements = engine
            .distributed_control_plane()
            .table_shards(
                aiondb_cluster::DatabaseId::DEFAULT,
                aiondb_core::RelationId::new(42),
            )?
            .pop()
            .unwrap()
            .placements;
        assert!(placements
            .iter()
            .any(|placement| placement.node_id == aiondb_cluster::NodeId::new("node-b")));
        assert!(!placements
            .iter()
            .any(|placement| placement.node_id == aiondb_cluster::NodeId::new("node-d")));
        Ok(())
    }

    #[test]
    fn new_with_config_accepts_disk_backend() {
        let data_dir = unique_temp_path("engine-builder", "disk");
        let mut runtime = RuntimeConfig::default();
        runtime.storage.backend = StorageBackend::Disk;

        let builder = EngineBuilder::new_with_config(data_dir.clone(), runtime)
            .expect("disk backend should initialize");

        assert!(builder.durable);
        assert_eq!(builder.runtime_config.storage.backend, StorageBackend::Disk);
        assert_eq!(builder.runtime_config.storage.data_dir, data_dir);

        let _ = std::fs::remove_dir_all(&builder.runtime_config.storage.data_dir);
    }

    #[test]
    fn new_with_config_accepts_lsm_backend() {
        let data_dir = unique_temp_path("engine-builder", "lsm");
        let mut runtime = RuntimeConfig::default();
        runtime.storage.backend = StorageBackend::Lsm;

        let builder = EngineBuilder::new_with_config(data_dir.clone(), runtime)
            .expect("lsm backend should initialize");

        assert!(builder.durable);
        assert_eq!(builder.runtime_config.storage.backend, StorageBackend::Lsm);
        assert_eq!(builder.runtime_config.storage.data_dir, data_dir);
        assert!(builder.runtime_config.storage.data_dir.join("lsm").is_dir());
        assert!(builder
            .runtime_config
            .storage
            .data_dir
            .join("lsm")
            .join("manifest.json")
            .is_file());

        let _ = std::fs::remove_dir_all(&builder.runtime_config.storage.data_dir);
    }

    #[test]
    fn new_with_config_accepts_page_engine_backend() {
        let data_dir = unique_temp_path("engine-builder", "page-engine");
        let mut runtime = RuntimeConfig::default();
        runtime.storage.backend = StorageBackend::PageEngine;

        let builder = EngineBuilder::new_with_config(data_dir.clone(), runtime)
            .expect("page_engine backend should initialize");

        assert!(builder.durable);
        assert_eq!(
            builder.runtime_config.storage.backend,
            StorageBackend::PageEngine
        );
        assert_eq!(builder.runtime_config.storage.data_dir, data_dir);

        let _ = std::fs::remove_dir_all(&builder.runtime_config.storage.data_dir);
    }

    #[test]
    fn timeline_id_defaults_to_one() {
        let data_dir = unique_temp_path("engine-builder", "timeline-default");
        let timeline =
            load_or_create_timeline_id(&data_dir, false).expect("timeline should initialize");
        assert_eq!(timeline, 1);

        let stored = std::fs::read_to_string(data_dir.join("replication").join("timeline"))
            .expect("timeline file should exist");
        assert_eq!(stored.trim(), "1");

        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[test]
    fn timeline_promotion_on_start_bumps_timeline_and_writes_history() {
        let data_dir = unique_temp_path("engine-builder", "timeline-promotion");
        let initial =
            load_or_create_timeline_id(&data_dir, false).expect("initial timeline should exist");
        assert_eq!(initial, 1);

        let promoted =
            load_or_create_timeline_id(&data_dir, true).expect("promotion should bump timeline");
        assert_eq!(promoted, 2);

        let history =
            std::fs::read_to_string(data_dir.join("replication").join("00000002.history"))
                .expect("timeline history file should exist");
        assert!(history.contains("00000001"));

        let _ = std::fs::remove_dir_all(&data_dir);
    }
}
