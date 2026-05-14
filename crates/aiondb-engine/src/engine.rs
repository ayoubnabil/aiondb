#![allow(clippy::pedantic)]
#![allow(clippy::similar_names)]

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
    time::Instant,
};

use parking_lot::RwLock as PlRwLock;

use aiondb_catalog::{CatalogReader, CatalogTxnParticipant, CatalogWriter, SequenceManager};
use aiondb_cluster::distributed::{
    ControlPlaneSnapshot, InMemoryControlPlane, MetadataReader, ReplicaRole,
};
use aiondb_cluster::ClusterCatalog;
use aiondb_config::RuntimeConfig;
#[cfg(test)]
pub(crate) use aiondb_core::Value;
use aiondb_core::{DbError, DbResult, RelationId, TxnId};
use aiondb_executor::node_registry::{NodeRegistry, NodeRegistrySnapshot};
use aiondb_executor::{ExecutionContext, ExecutionResult, Executor, FragmentDispatcher};
use aiondb_fragment_transport::server::FragmentExecutor;
use aiondb_optimizer::{OptimizeRequest, Optimizer};
use aiondb_parser::{parse_prepared_statement, parse_sql, Statement};
use aiondb_plan::PhysicalPlan;
use aiondb_planner::{PlanRequest, Planner};
use aiondb_security::{
    AccessRequest, AccessTarget, Action, AuthRateLimiter, AuthenticatedIdentity, Authenticator,
    Authorizer,
};
use aiondb_storage_api::{StorageDDL, StorageDML, StorageTxnParticipant};
use aiondb_tx::{
    ActiveTransaction, IsolationLevel, LockManager, SerializableCoordinator, Snapshot,
    SnapshotOracle, TransactionLifecycle,
};
use tracing::{debug, info, warn};

use crate::{auth_audit::AuthAuditSink, catalog_auth::CatalogAuthPolicy};

pub mod api;
pub(crate) mod async_notify;
mod async_notify_ops;
mod auth_ops;
mod backup;
mod checkpoint;
mod compat;
mod compat_aggregate_rewrite;
mod compat_router;
mod copy_support;
mod cross_database;
pub(crate) mod cypher_sql;
mod eval_context;
mod extensions;
mod functions;
pub mod ha;
mod implicit_txn;
pub mod metrics;
mod pg_compat_hooks;
mod plan_cache;
mod portal_exec;
mod query_api;
mod query_api_copy_compat;
mod query_api_explain;
mod query_api_session;
mod query_api_wire;
mod recursive_cte;
pub mod replication;
mod replication_maintenance;
mod savepoints;
mod session_access;
mod session_lifecycle;
mod session_vars;
mod statement_exec;
mod statement_policy;
pub mod streaming;
mod support;
mod tenant;

pub use self::api::{
    QueryEngine, ReplicationIdentity, SqlStatementWireMetadata, StartupAuthentication,
    StartupParams, WireStateCleanupHint,
};
pub use self::replication_maintenance::DistributedMembershipMaintenanceOutcome;
use self::statement_policy::{
    is_acl_pseudo_role, normalize_acl_statement, reject_pg_database_catalog_update,
    statement_requires_implicit_transaction,
};
use self::support::{
    map_execution_result, map_transaction_mode, next_session_handle, ExplainAnalyzeSummary,
};

#[cfg(test)]
use std::collections::BTreeMap;

#[cfg(test)]
use aiondb_security::{Credential, TransportInfo};

use crate::{
    config::EngineConfig,
    prepared::{
        PortalBatch, PortalDescription, PortalState, PreparedStatementDesc, PreparedStatementState,
        ResultColumn, StatementResult,
    },
    session::{CompatMiscObjectAttrs, SessionHandle, SessionInfo, SessionRecord},
};

#[derive(Clone, Debug)]
struct PreparedTransactionRecord {
    txn: ActiveTransaction,
    include_catalog_participant: bool,
    include_storage_participant: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PreparedSelectResultCacheKey {
    database_id: aiondb_cluster::DatabaseId,
    sql: String,
}

pub struct Engine {
    config: EngineConfig,
    runtime_config: RuntimeConfig,
    authenticator: Arc<dyn Authenticator>,
    authorizer: Arc<dyn Authorizer>,
    authorizer_is_noop: bool,
    rate_limiter: Arc<dyn AuthRateLimiter>,
    auth_audit_sink: Arc<dyn AuthAuditSink>,
    tx_manager: Arc<dyn TransactionLifecycle>,
    snapshot_oracle: Arc<dyn SnapshotOracle>,
    serializable_coordinator: Arc<dyn SerializableCoordinator>,
    lock_manager: Arc<dyn LockManager>,
    catalog_txn: Arc<dyn CatalogTxnParticipant>,
    catalog_reader: Arc<dyn CatalogReader>,
    catalog_writer: Arc<dyn CatalogWriter>,
    storage_dml: Arc<dyn StorageDML>,
    storage_txn: Arc<dyn StorageTxnParticipant>,
    planner: Planner,
    optimizer: Optimizer,
    executor: Executor,
    node_registry: Option<Arc<NodeRegistry>>,
    distributed_control_plane: Arc<InMemoryControlPlane>,
    startup_auth_policy: Option<Arc<CatalogAuthPolicy>>,
    replication_handle: Option<replication::EngineReplicationHandle>,
    replication_manager: Option<Arc<streaming::ReplicationManager>>,
    replication_identity: Option<ReplicationIdentity>,
    sessions: RwLock<HashMap<SessionHandle, Arc<Mutex<SessionRecord>>>>,
    /// Serializes Postgres-compat advisory lock state mutations
    /// (`pg_advisory_lock` / `pg_try_advisory_lock` / unlock variants) so the
    /// catalog-backed registry observes the same total order across sessions.
    compat_advisory_mutex: Mutex<()>,
    compat_prepared_xacts: Mutex<HashMap<String, PreparedTransactionRecord>>,
    compat_global_comments: Mutex<HashMap<(String, String), String>>,
    compat_misc_global_objects: Mutex<HashMap<(String, String), String>>,
    compat_misc_global_attrs: Mutex<HashMap<(String, String), CompatMiscObjectAttrs>>,
    alter_system_overrides: Mutex<HashMap<String, String>>,
    compat_role_membership_dependencies: PlRwLock<compat::CompatRoleMembershipDependencyRegistry>,
    compat_granted_privilege_dependencies:
        PlRwLock<compat::CompatGrantedPrivilegeDependencyRegistry>,
    metrics: Arc<metrics::EngineMetrics>,
    /// Instrumentation per-CompatCommand (ADR-0003): calls, fallbacks,
    /// parse/bind/execute latencies. Snapshot via `compat_metrics_snapshot()`.
    compat_metrics: Arc<aiondb_pg_compat::metrics::CompatCommandMetrics>,
    /// Cluster catalog: source of truth for databases + shared roles
    /// (ADR-0014). Phase 1: in-memory catalog with the `default` database
    /// pre-registered as `DatabaseId::DEFAULT`. Later phases route
    /// catalog/storage by `DatabaseId`.
    cluster_catalog: Arc<dyn aiondb_cluster::ClusterCatalog>,
    /// Held across `validate_commit → commit_ts allocation → finish_commit`
    /// for any transaction that is not pure READ COMMITTED DML. Dropping
    /// between validate and finish would let two writers both validate against
    /// the same predicate set and then race to commit.
    commit_lock: Mutex<()>,
    /// Synthetic owner ids handed to the lock manager for statements that run
    /// outside an explicit transaction. Counts down from `u64::MAX` so the ids
    /// can never collide with a real `TxnId` (which is allocated upward from
    /// 0). Each statement gets a fresh id and the lock is released at the end
    /// of the statement.
    statement_lock_owner: AtomicU64,
    plan_cache_hits: AtomicU64,
    prepared_select_result_cache:
        RwLock<HashMap<PreparedSelectResultCacheKey, (u64, u64, PortalBatch)>>,
    extension_registry: Arc<aiondb_extension::ExtensionRegistry>,
    notification_bus: Arc<self::async_notify::NotificationBus>,
}

fn hnsw_ef_search_from_execution_context(context: &ExecutionContext) -> DbResult<Option<usize>> {
    let Some(value) =
        context.current_session_setting(self::session_vars::HNSW_EF_SEARCH_SETTING, true)?
    else {
        return Ok(None);
    };
    self::session_vars::parse_positive_integer_setting_value(
        self::session_vars::HNSW_EF_SEARCH_SETTING,
        &value,
    )
    .map(Some)
}

impl Engine {
    pub(crate) fn notification_bus(&self) -> &Arc<self::async_notify::NotificationBus> {
        &self.notification_bus
    }

    pub fn distributed_node_registry_snapshot(&self) -> Option<NodeRegistrySnapshot> {
        self.node_registry
            .as_ref()
            .map(|registry| registry.health_snapshot())
    }

    pub fn distributed_control_plane_snapshot(&self) -> DbResult<ControlPlaneSnapshot> {
        self.distributed_control_plane.snapshot()
    }

    pub fn distributed_control_plane(&self) -> Arc<InMemoryControlPlane> {
        Arc::clone(&self.distributed_control_plane)
    }

    pub(crate) fn distributed_shard_leader_nodes_for_database(
        &self,
        database_id: aiondb_cluster::DatabaseId,
    ) -> DbResult<Vec<(u32, String)>> {
        let mut leaders = Vec::new();
        for shard in self
            .distributed_control_plane
            .database_shards(database_id)?
        {
            if let Some(leader) = shard
                .placements
                .iter()
                .find(|placement| placement.role == ReplicaRole::Leader)
            {
                leaders.push((shard.shard_id.get(), leader.node_id.as_str().to_owned()));
            }
        }
        leaders.sort_unstable_by_key(|(shard_id, _)| *shard_id);
        Ok(leaders)
    }

    pub(crate) fn distributed_shard_leader_nodes_for_table(
        &self,
        database_id: aiondb_cluster::DatabaseId,
        table_id: RelationId,
    ) -> DbResult<Vec<(u32, String)>> {
        let mut leaders = Vec::new();
        for shard in self
            .distributed_control_plane
            .table_shards(database_id, table_id)?
        {
            if let Some(leader) = shard
                .placements
                .iter()
                .find(|placement| placement.role == ReplicaRole::Leader)
            {
                leaders.push((shard.shard_id.get(), leader.node_id.as_str().to_owned()));
            }
        }
        leaders.sort_unstable_by_key(|(shard_id, _)| *shard_id);
        Ok(leaders)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Engine-owned compat state drops with the engine. This hook remains
        // intentionally empty for now.
    }
}

impl Engine {
    pub(crate) fn new(
        config: EngineConfig,
        runtime_config: RuntimeConfig,
        authenticator: Arc<dyn Authenticator>,
        authorizer: Arc<dyn Authorizer>,
        rate_limiter: Arc<dyn AuthRateLimiter>,
        auth_audit_sink: Arc<dyn AuthAuditSink>,
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
        distributed_control_plane: Arc<InMemoryControlPlane>,
        startup_auth_policy: Option<Arc<CatalogAuthPolicy>>,
        replication_handle: Option<replication::EngineReplicationHandle>,
        replication_manager: Option<Arc<streaming::ReplicationManager>>,
        replication_identity: Option<ReplicationIdentity>,
    ) -> Self {
        let planner = Planner::new(catalog_reader.clone());
        let optimizer = Optimizer::new(catalog_reader.clone());
        let executor_optimizer = Optimizer::new(catalog_reader.clone());
        let mut executor = Executor::new(
            catalog_reader.clone(),
            catalog_writer.clone(),
            catalog_txn.clone(),
            sequence_manager,
            storage_ddl,
            storage_dml.clone(),
            storage_txn.clone(),
            Arc::new(move |logical_plan, context| {
                let hnsw_ef_search = hnsw_ef_search_from_execution_context(context)?;
                executor_optimizer.optimize_with_hnsw_ef_search(
                    OptimizeRequest {
                        logical_plan: logical_plan.clone(),
                        txn_id: context.txn_id,
                    },
                    hnsw_ef_search,
                )
            }),
        );
        if let Some(fragment_dispatcher) = fragment_dispatcher {
            executor = executor.with_fragment_dispatcher(fragment_dispatcher);
        }
        let authorizer_is_noop = authorizer.is_noop();
        Self {
            config,
            runtime_config,
            authenticator,
            authorizer,
            authorizer_is_noop,
            rate_limiter,
            auth_audit_sink,
            tx_manager,
            snapshot_oracle,
            serializable_coordinator,
            lock_manager,
            catalog_txn: catalog_txn.clone(),
            catalog_reader: catalog_reader.clone(),
            catalog_writer: catalog_writer.clone(),
            storage_dml: storage_dml.clone(),
            storage_txn: storage_txn.clone(),
            planner,
            optimizer,
            executor,
            node_registry,
            distributed_control_plane,
            startup_auth_policy,
            replication_handle,
            replication_manager,
            replication_identity,
            sessions: RwLock::new(HashMap::new()),
            compat_advisory_mutex: Mutex::new(()),
            compat_prepared_xacts: Mutex::new(HashMap::new()),
            compat_global_comments: Mutex::new(HashMap::new()),
            compat_misc_global_objects: Mutex::new(HashMap::new()),
            compat_misc_global_attrs: Mutex::new(HashMap::new()),
            alter_system_overrides: Mutex::new(HashMap::new()),
            compat_role_membership_dependencies: PlRwLock::new(Default::default()),
            compat_granted_privilege_dependencies: PlRwLock::new(Default::default()),
            metrics: Arc::new(metrics::EngineMetrics::new()),
            compat_metrics: Arc::new(aiondb_pg_compat::metrics::CompatCommandMetrics::new()),
            cluster_catalog: {
                let cat = aiondb_cluster::InMemoryClusterCatalog::new();
                // Bootstrap the default database so callers can resolve
                // DatabaseId::DEFAULT immediately after Engine::new.
                let _ = cat.bootstrap_default("postgres");
                // Seed the common PostgreSQL client defaults. Drivers and
                // regression harnesses frequently connect to `default` /
                // `test` without issuing an explicit CREATE DATABASE first.
                // These names now live directly in the cluster catalog.
                for builtin_name in [aiondb_core::COMPAT_DEFAULT_DATABASE_NAME, "test"] {
                    let _ = cat.create_database(aiondb_cluster::CreateDatabaseRequest::simple(
                        builtin_name.to_owned(),
                        "aiondb".to_owned(),
                    ));
                }
                Arc::new(cat)
            },
            commit_lock: Mutex::new(()),
            statement_lock_owner: AtomicU64::new(u64::MAX),
            plan_cache_hits: AtomicU64::new(0),
            prepared_select_result_cache: RwLock::new(HashMap::new()),
            extension_registry: Arc::new(aiondb_extension::ExtensionRegistry::new()),
            notification_bus: Arc::new(self::async_notify::NotificationBus::new()),
        }
    }

    /// Returns a reference to the extension registry.
    pub fn extension_registry(&self) -> &aiondb_extension::ExtensionRegistry {
        &self.extension_registry
    }

    /// Snapshot the per-`CompatCommand` instrumentation (calls, fallbacks,
    /// latency histograms). Exposed publicly so facades (dashboard, pgwire
    /// metrics endpoint) can render Prometheus / JSON views.
    pub fn compat_metrics_snapshot(&self) -> Vec<aiondb_pg_compat::metrics::CompatCommandSnapshot> {
        self.compat_metrics.snapshot()
    }

    /// Access to the cluster catalog (ADR-0014). Exposed for introspection
    /// and `DatabaseId` allocation from facades and `CREATE DATABASE`
    /// hooks.
    pub fn cluster_catalog(&self) -> &Arc<dyn aiondb_cluster::ClusterCatalog> {
        &self.cluster_catalog
    }

    /// Returns the active `DatabaseId` for the session (ADR-0014 phase 3).
    /// Used by callers that want to log / route / enforce the per-database
    /// isolation boundary.
    pub fn active_database_for(
        &self,
        session: &SessionHandle,
    ) -> DbResult<aiondb_cluster::DatabaseId> {
        self.with_session(session, |record| Ok(record.info.active_database))
    }

    #[cfg(test)]
    fn session_mut<'a>(
        sessions: &'a HashMap<SessionHandle, Arc<Mutex<SessionRecord>>>,
        handle: &SessionHandle,
    ) -> DbResult<MutexGuard<'a, SessionRecord>> {
        let session = sessions
            .get(handle)
            .ok_or_else(|| DbError::invalid_authorization("unknown session handle"))?;
        Self::lock_session(session)
    }
}

fn parse_fragment_isolation(isolation: &str) -> DbResult<IsolationLevel> {
    let normalized: String = isolation
        .chars()
        .filter(|ch| !matches!(ch, ' ' | '_' | '-'))
        .flat_map(char::to_lowercase)
        .collect();
    match normalized.as_str() {
        "readcommitted" => Ok(IsolationLevel::ReadCommitted),
        "snapshotisolation" => Ok(IsolationLevel::SnapshotIsolation),
        "serializable" => Ok(IsolationLevel::Serializable),
        _ => Err(DbError::internal(format!(
            "unsupported remote fragment isolation level: {isolation}"
        ))),
    }
}

impl FragmentExecutor for Engine {
    fn execute_plan(
        &self,
        plan: &PhysicalPlan,
        txn_id: u64,
        isolation: &str,
        snapshot: Option<aiondb_fragment_transport::protocol::FragmentSnapshot>,
        shard_id: Option<u32>,
        max_result_rows: u64,
        max_result_bytes: u64,
        max_memory_bytes: u64,
        max_temp_bytes: u64,
        deadline: Option<Instant>,
    ) -> DbResult<ExecutionResult> {
        let isolation = parse_fragment_isolation(isolation)?;
        let txn_id = TxnId::new(txn_id);
        let bootstrap_snapshot = snapshot.map_or_else(
            || Snapshot::new(TxnId::default(), TxnId::default(), Vec::new()),
            |snapshot| {
                Snapshot::new(
                    TxnId::new(snapshot.xmin),
                    TxnId::new(snapshot.xmax),
                    snapshot.active.into_iter().map(TxnId::new).collect(),
                )
            },
        );
        let txn = ActiveTransaction {
            id: txn_id,
            isolation,
            start_ts: 0,
            snapshot: bootstrap_snapshot,
        };
        let snapshot = txn.snapshot.clone();
        let requested_max_result_rows = max_result_rows;
        let requested_max_result_bytes = max_result_bytes;
        let requested_max_memory_bytes = max_memory_bytes;
        let requested_max_temp_bytes = max_temp_bytes;
        let (max_result_rows, max_result_bytes, max_memory_bytes, max_temp_bytes) =
            crate::config::guard_execution_limits(
                max_result_rows,
                max_result_bytes,
                max_memory_bytes,
                max_temp_bytes,
            );
        if (
            max_result_rows,
            max_result_bytes,
            max_memory_bytes,
            max_temp_bytes,
        ) != (
            requested_max_result_rows,
            requested_max_result_bytes,
            requested_max_memory_bytes,
            requested_max_temp_bytes,
        ) {
            warn!(
                requested_max_result_rows,
                requested_max_result_bytes,
                requested_max_memory_bytes,
                requested_max_temp_bytes,
                max_result_rows,
                max_result_bytes,
                max_memory_bytes,
                max_temp_bytes,
                "clamping fragment execution limits by memory safety guard"
            );
        }
        let distributed_shared_storage_nodes = self
            .runtime_config
            .distributed
            .loopback_remote_nodes
            .clone();
        let mut distributed_fragment_target_nodes =
            if self.runtime_config.distributed.remote_nodes.is_empty() {
                distributed_shared_storage_nodes.clone()
            } else {
                Vec::new()
            };
        if !self.runtime_config.distributed.remote_nodes.is_empty() {
            for remote in &self.runtime_config.distributed.remote_nodes {
                if !distributed_fragment_target_nodes
                    .iter()
                    .any(|node: &String| node.eq_ignore_ascii_case(&remote.node_id))
                {
                    distributed_fragment_target_nodes.push(remote.node_id.clone());
                }
            }
        }
        let distributed_shard_leader_nodes =
            self.distributed_shard_leader_nodes_for_database(aiondb_cluster::DatabaseId::DEFAULT)?;

        let context = ExecutionContext::new(
            txn_id,
            isolation,
            snapshot,
            max_result_rows,
            None,
            0,
            max_result_bytes,
            max_memory_bytes,
            max_temp_bytes,
            deadline,
            Some(self.runtime_config.storage.data_dir.clone()),
        )
        .with_max_parallel_workers_per_query(
            self.runtime_config.limits.max_parallel_workers_per_query,
        )
        .with_distributed_loopback_remote_nodes(distributed_fragment_target_nodes)
        .with_distributed_shared_storage_remote_nodes(distributed_shared_storage_nodes)
        .with_distributed_shard_leader_nodes(distributed_shard_leader_nodes)
        .with_distributed_current_shard_id(shard_id)
        .with_serializable_coordinator(self.serializable_coordinator.clone());

        self.executor.execute(plan, &context)
    }
}

#[cfg(test)]
#[path = "engine_tests/mod.rs"]
mod tests;
