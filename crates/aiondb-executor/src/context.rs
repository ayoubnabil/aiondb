use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::result::ExecutionResult;
use aiondb_catalog::IndexDescriptor;
use aiondb_core::{
    compat_setting_value, DataType, DateStyleSetting, DbError, DbResult, RelationId, SequenceId,
    SqlState, TimeZoneSetting, TupleId, TxnId, Value,
};
use aiondb_tx::{IsolationLevel, LockManager, LockMode, SerializableCoordinator, Snapshot};

type PlpgsqlRuntimeFn = dyn for<'a> Fn(&PlpgsqlInvocation<'a>) -> DbResult<Value> + Send + Sync;

#[derive(Clone)]
pub struct PlpgsqlRuntimeHandle {
    inner: Arc<PlpgsqlRuntimeFn>,
}

impl PlpgsqlRuntimeHandle {
    pub fn new<F>(f: F) -> Self
    where
        F: for<'a> Fn(&PlpgsqlInvocation<'a>) -> DbResult<Value> + Send + Sync + 'static,
    {
        Self { inner: Arc::new(f) }
    }

    pub fn invoke<'a>(&self, invocation: &PlpgsqlInvocation<'a>) -> DbResult<Value> {
        (self.inner)(invocation)
    }
}

#[derive(Clone, Copy)]
pub struct TriggerInvocation<'a> {
    pub new_row: Option<&'a [Value]>,
    pub old_row: Option<&'a [Value]>,
    pub columns: &'a [String],
    pub tg_op: &'a str,
    pub tg_name: &'a str,
    pub tg_table_name: &'a str,
    pub tg_args: &'a [String],
    pub tg_when: &'a str,
    pub tg_level: &'a str,
    pub tg_table_schema: &'a str,
    pub tg_relid: u32,
}

pub struct PlpgsqlInvocation<'a> {
    pub body: &'a str,
    pub parameters: &'a [(String, DataType)],
    pub argument_values: &'a [Value],
    pub execution_context: &'a ExecutionContext,
    pub trigger_context: Option<TriggerInvocation<'a>>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct SequenceSessionState {
    current_values: HashMap<SequenceId, i64>,
    last_value: Option<i64>,
}

impl SequenceSessionState {
    pub fn record_next_value(&mut self, sequence_id: SequenceId, value: i64) {
        self.current_values.insert(sequence_id, value);
        self.last_value = Some(value);
    }

    pub fn record_set_value(&mut self, sequence_id: SequenceId, value: i64, is_called: bool) {
        if is_called {
            self.current_values.insert(sequence_id, value);
        }
    }

    pub fn current_value(&self, sequence_id: SequenceId) -> Option<i64> {
        self.current_values.get(&sequence_id).copied()
    }

    pub fn last_value(&self) -> Option<i64> {
        self.last_value
    }

    pub fn clear(&mut self) {
        self.current_values.clear();
        self.last_value = None;
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct SessionSettings {
    values: HashMap<String, String>,
    tenant_schema_name: Option<String>,
    current_user: Option<String>,
    is_superuser: bool,
}

impl SessionSettings {
    pub fn new(
        values: HashMap<String, String>,
        tenant_schema_name: Option<String>,
        current_user: Option<String>,
        is_superuser: bool,
    ) -> Self {
        Self {
            values,
            tenant_schema_name,
            current_user,
            is_superuser,
        }
    }

    pub fn current_user_name(&self) -> Option<String> {
        self.current_user.clone()
    }

    pub fn resolve_value(&self, name: &str) -> Option<String> {
        let normalized: Cow<'_, str> = if name.bytes().any(|b| b.is_ascii_uppercase()) {
            Cow::Owned(name.to_ascii_lowercase())
        } else {
            Cow::Borrowed(name)
        };
        let normalized_name = normalized.as_ref();

        if let Some(value) = self.values.get(normalized_name) {
            return Some(match normalized_name {
                "datestyle" => DateStyleSetting::parse(value).show_value(),
                "timezone" => TimeZoneSetting::parse(value).show_value(),
                _ => value.clone(),
            });
        }

        match normalized_name {
            "application_name" => Some(String::new()),
            "role" => Some("none".to_owned()),
            "is_superuser" => Some(if self.is_superuser { "on" } else { "off" }.to_owned()),
            "transaction_isolation" => self
                .values
                .get("default_transaction_isolation")
                .cloned()
                .or_else(|| compat_setting_value(normalized_name).map(Cow::into_owned)),
            "transaction_read_only" => self
                .values
                .get("default_transaction_read_only")
                .cloned()
                .or_else(|| compat_setting_value(normalized_name).map(Cow::into_owned)),
            "transaction_deferrable" => self
                .values
                .get("default_transaction_deferrable")
                .cloned()
                .or_else(|| compat_setting_value(normalized_name).map(Cow::into_owned)),
            "search_path" => self
                .tenant_schema_name
                .clone()
                .or_else(|| compat_setting_value(normalized_name).map(Cow::into_owned)),
            _ => compat_setting_value(normalized_name).map(Cow::into_owned),
        }
    }

    pub fn current_setting(&self, name: &str, missing_ok: bool) -> DbResult<Option<String>> {
        match self.resolve_value(name) {
            Some(value) => Ok(Some(value)),
            None if missing_ok => Ok(None),
            None => Err(DbError::parse_error(
                SqlState::UndefinedObject,
                format!("unrecognized configuration parameter \"{name}\""),
            )),
        }
    }

    pub fn set_raw_value(&mut self, name: &str, value: &str) -> DbResult<()> {
        const MAX_SESSION_SETTINGS: usize = 1024;
        if !self.values.contains_key(&name.to_ascii_lowercase())
            && self.values.len() >= MAX_SESSION_SETTINGS
        {
            return Err(DbError::program_limit(
                "maximum number of session settings reached (1024)",
            ));
        }
        self.values
            .insert(name.to_ascii_lowercase(), value.to_owned());
        Ok(())
    }
}

/// Identifies the share of a sequential scan that a single Gather worker is
/// responsible for. Each worker only emits tuples whose `tuple_id` mod
/// `num_workers` equals its `worker_id`, giving disjoint, deterministic
/// partitions without any storage-side cooperation.
#[derive(Clone, Copy, Debug)]
pub struct ParallelScanPartition {
    pub worker_id: u32,
    pub num_workers: u32,
}

#[derive(Clone)]
pub struct ExecutionContext {
    pub txn_id: TxnId,
    pub isolation: IsolationLevel,
    pub implicit_transaction: bool,
    pub storage_autocommit_fast_path: bool,
    pub snapshot: Snapshot,
    pub max_result_rows: u64,
    pub collect_row_limit: Option<u64>,
    pub collect_row_offset: u64,
    pub max_result_bytes: u64,
    pub max_memory_bytes: u64,
    pub max_temp_bytes: u64,
    pub statement_deadline: Option<Instant>,
    pub server_data_dir: Option<PathBuf>,
    pub budget_accounting_lock: Arc<Mutex<()>>,
    pub memory_used: Arc<AtomicU64>,
    pub temp_used: Arc<AtomicU64>,
    pub sequence_session_state: Option<Arc<Mutex<SequenceSessionState>>>,
    pub session_settings: Option<Arc<Mutex<SessionSettings>>>,
    pub session_setting_applier:
        Option<Arc<dyn Fn(String, String, bool) -> DbResult<()> + Send + Sync>>,
    pub lock_owner_id: TxnId,
    pub lock_timeout: Option<Duration>,
    pub lock_manager: Option<Arc<dyn LockManager>>,
    pub serializable_coordinator: Option<Arc<dyn SerializableCoordinator>>,
    pub cancellation_checker: Option<Arc<dyn Fn() -> DbResult<()> + Send + Sync>>,
    pub relation_has_explicit_oid_cache: Arc<Mutex<HashMap<RelationId, bool>>>,
    pub compat_row_width_cache: Arc<Mutex<HashMap<RelationId, usize>>>,
    pub sequence_lookup_cache: Arc<Mutex<HashMap<String, SequenceId>>>,
    pub table_index_cache: Arc<Mutex<HashMap<RelationId, Vec<IndexDescriptor>>>>,
    pub statement_tuple_writes: Arc<Mutex<HashSet<(RelationId, TupleId)>>>,
    pub graph_profile_actual_rows: Arc<Mutex<HashMap<String, u64>>>,
    pub graph_profile_elapsed_nanos: Arc<Mutex<HashMap<String, u64>>>,
    pub udf_depth: Arc<AtomicU32>,
    /// Counts how deeply trigger invocations are nested in the current
    /// statement. Triggers that fire DML which fires triggers can recurse
    /// indefinitely; PG caps at ~128. We refuse `>= 256` to bound stack
    /// usage on default-sized worker threads.
    pub trigger_depth: Arc<AtomicU32>,
    /// Counts how deeply FK CASCADE / FK ON UPDATE actions are nested.
    /// A schema with cyclic ON DELETE CASCADE references would otherwise
    /// loop on the host stack until SIGSEGV.
    pub fk_cascade_depth: Arc<AtomicU32>,
    pub max_parallel_workers_per_query: usize,
    /// Set by the Gather executor in each worker thread so downstream
    /// SeqScans return only the share of tuples assigned to this worker
    /// (`tuple_id % num_workers == worker_id`). `None` outside Gather.
    pub parallel_scan_partition: Option<ParallelScanPartition>,
    pub distributed_loopback_remote_nodes: Arc<Vec<String>>,
    pub distributed_shared_storage_remote_nodes: Arc<Vec<String>>,
    pub distributed_shard_leader_nodes: Arc<HashMap<u32, String>>,
    pub distributed_current_shard_id: Option<u32>,
    pub side_effect_query_cache: Arc<Mutex<HashMap<u64, ExecutionResult>>>,
    /// Slot used by row-trigger evaluators to publish a `NEW.col := …`
    /// rewritten row back to the firing site. The trigger executor writes
    /// it; `fire_before_*_triggers` reads and clears it after each call.
    pub trigger_modified_new: Arc<Mutex<Option<Vec<Value>>>>,
}

impl ExecutionContext {
    pub fn new(
        txn_id: TxnId,
        isolation: IsolationLevel,
        snapshot: Snapshot,
        max_result_rows: u64,
        collect_row_limit: Option<u64>,
        collect_row_offset: u64,
        max_result_bytes: u64,
        max_memory_bytes: u64,
        max_temp_bytes: u64,
        statement_deadline: Option<Instant>,
        server_data_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            txn_id,
            isolation,
            implicit_transaction: false,
            storage_autocommit_fast_path: false,
            snapshot,
            max_result_rows,
            collect_row_limit,
            collect_row_offset,
            max_result_bytes,
            max_memory_bytes,
            max_temp_bytes,
            statement_deadline,
            server_data_dir,
            budget_accounting_lock: Arc::new(Mutex::new(())),
            memory_used: Arc::new(AtomicU64::new(0)),
            temp_used: Arc::new(AtomicU64::new(0)),
            sequence_session_state: None,
            session_settings: None,
            session_setting_applier: None,
            lock_owner_id: txn_id,
            lock_timeout: None,
            lock_manager: None,
            serializable_coordinator: None,
            cancellation_checker: None,
            relation_has_explicit_oid_cache: Arc::new(Mutex::new(HashMap::new())),
            compat_row_width_cache: Arc::new(Mutex::new(HashMap::new())),
            sequence_lookup_cache: Arc::new(Mutex::new(HashMap::new())),
            table_index_cache: Arc::new(Mutex::new(HashMap::new())),
            statement_tuple_writes: Arc::new(Mutex::new(HashSet::new())),
            graph_profile_actual_rows: Arc::new(Mutex::new(HashMap::new())),
            graph_profile_elapsed_nanos: Arc::new(Mutex::new(HashMap::new())),
            udf_depth: Arc::new(AtomicU32::new(0)),
            trigger_depth: Arc::new(AtomicU32::new(0)),
            fk_cascade_depth: Arc::new(AtomicU32::new(0)),
            max_parallel_workers_per_query: 1,
            parallel_scan_partition: None,
            distributed_loopback_remote_nodes: Arc::new(Vec::new()),
            distributed_shared_storage_remote_nodes: Arc::new(Vec::new()),
            distributed_shard_leader_nodes: Arc::new(HashMap::new()),
            distributed_current_shard_id: None,
            side_effect_query_cache: Arc::new(Mutex::new(HashMap::new())),
            trigger_modified_new: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_sequence_session_state(
        mut self,
        sequence_session_state: Arc<Mutex<SequenceSessionState>>,
    ) -> Self {
        self.sequence_session_state = Some(sequence_session_state);
        self
    }

    pub fn with_session_settings(mut self, session_settings: SessionSettings) -> Self {
        self.session_settings = Some(Arc::new(Mutex::new(session_settings)));
        self
    }

    pub fn with_session_setting_applier(
        mut self,
        session_setting_applier: Arc<dyn Fn(String, String, bool) -> DbResult<()> + Send + Sync>,
    ) -> Self {
        self.session_setting_applier = Some(session_setting_applier);
        self
    }

    pub fn with_implicit_transaction(mut self, implicit_transaction: bool) -> Self {
        self.implicit_transaction = implicit_transaction;
        self
    }

    pub fn with_storage_autocommit_fast_path(mut self, enabled: bool) -> Self {
        self.storage_autocommit_fast_path = enabled;
        self
    }

    pub fn resolve_session_setting(&self, name: &str) -> Option<String> {
        self.session_settings
            .as_ref()
            .and_then(|settings| settings.lock().ok())
            .and_then(|settings| settings.resolve_value(name))
    }

    pub fn current_user_name(&self) -> Option<String> {
        self.session_settings
            .as_ref()
            .and_then(|settings| settings.lock().ok())
            .and_then(|settings| settings.current_user_name())
    }

    pub fn cached_sequence_id(&self, sequence_name: &str) -> DbResult<Option<SequenceId>> {
        let cache = self.sequence_lookup_cache.lock().map_err(|e| {
            DbError::internal(format!(
                "execution context sequence cache lock poisoned: {e}"
            ))
        })?;
        Ok(cache.get(sequence_name).copied())
    }

    pub fn cache_sequence_id(&self, sequence_name: &str, sequence_id: SequenceId) -> DbResult<()> {
        let mut cache = self.sequence_lookup_cache.lock().map_err(|e| {
            DbError::internal(format!(
                "execution context sequence cache lock poisoned: {e}"
            ))
        })?;
        cache.insert(sequence_name.to_owned(), sequence_id);
        Ok(())
    }

    pub fn cached_table_indexes(
        &self,
        table_id: RelationId,
    ) -> DbResult<Option<Vec<IndexDescriptor>>> {
        let cache = self.table_index_cache.lock().map_err(|e| {
            DbError::internal(format!(
                "execution context table index cache lock poisoned: {e}"
            ))
        })?;
        Ok(cache.get(&table_id).cloned())
    }

    pub fn cache_table_indexes(
        &self,
        table_id: RelationId,
        indexes: Vec<IndexDescriptor>,
    ) -> DbResult<()> {
        let mut cache = self.table_index_cache.lock().map_err(|e| {
            DbError::internal(format!(
                "execution context table index cache lock poisoned: {e}"
            ))
        })?;
        cache.insert(table_id, indexes);
        Ok(())
    }

    pub fn record_graph_profile_actual_rows(&self, key: &str, actual_rows: u64) -> DbResult<()> {
        let mut counters = self.graph_profile_actual_rows.lock().map_err(|e| {
            DbError::internal(format!(
                "execution context graph profile counters lock poisoned: {e}"
            ))
        })?;
        let entry = counters.entry(key.to_owned()).or_insert(0);
        *entry = entry.saturating_add(actual_rows);
        Ok(())
    }

    pub fn snapshot_graph_profile_actual_rows(&self) -> DbResult<HashMap<String, u64>> {
        self.graph_profile_actual_rows
            .lock()
            .map_err(|e| {
                DbError::internal(format!(
                    "execution context graph profile counters lock poisoned: {e}"
                ))
            })
            .map(|counters| counters.clone())
    }

    pub fn record_graph_profile_elapsed_nanos(
        &self,
        key: &str,
        elapsed_nanos: u64,
    ) -> DbResult<()> {
        let mut counters = self.graph_profile_elapsed_nanos.lock().map_err(|e| {
            DbError::internal(format!(
                "execution context graph profile timings lock poisoned: {e}"
            ))
        })?;
        let entry = counters.entry(key.to_owned()).or_insert(0);
        *entry = entry.saturating_add(elapsed_nanos);
        Ok(())
    }

    pub fn snapshot_graph_profile_elapsed_nanos(&self) -> DbResult<HashMap<String, u64>> {
        self.graph_profile_elapsed_nanos
            .lock()
            .map_err(|e| {
                DbError::internal(format!(
                    "execution context graph profile timings lock poisoned: {e}"
                ))
            })
            .map(|counters| counters.clone())
    }

    pub fn current_session_setting(
        &self,
        name: &str,
        missing_ok: bool,
    ) -> DbResult<Option<String>> {
        match &self.session_settings {
            Some(settings) => settings
                .lock()
                .map_err(|e| DbError::internal(format!("session settings poisoned: {e}")))?
                .current_setting(name, missing_ok),
            None => {
                let normalized = name.to_ascii_lowercase();
                let value = match normalized.as_str() {
                    "application_name" => Some(String::new()),
                    "role" => Some("none".to_owned()),
                    "is_superuser" => Some("on".to_owned()),
                    "max_connections" => Some("128".to_owned()),
                    "server_version_num" => Some(aiondb_core::compat_server_version_num_string()),
                    "current_catalog" => Some(aiondb_core::COMPAT_DEFAULT_DATABASE_NAME.to_owned()),
                    "wal_segment_size" => Some("16777216".to_owned()),
                    _ => compat_setting_value(&normalized).map(Cow::into_owned),
                };
                match value {
                    Some(value) => Ok(Some(value)),
                    None if missing_ok => Ok(None),
                    None => Err(DbError::parse_error(
                        SqlState::UndefinedObject,
                        format!("unrecognized configuration parameter \"{name}\""),
                    )),
                }
            }
        }
    }

    pub fn apply_session_setting(&self, name: &str, value: &str, is_local: bool) -> DbResult<()> {
        if let Some(applier) = &self.session_setting_applier {
            applier(name.to_owned(), value.to_owned(), is_local)?;
        }
        if let Some(settings) = &self.session_settings {
            let mut settings = settings
                .lock()
                .map_err(|e| DbError::internal(format!("session settings poisoned: {e}")))?;
            settings.set_raw_value(name, value)?;
        }
        Ok(())
    }

    pub fn with_lock_manager(
        mut self,
        lock_owner_id: TxnId,
        lock_manager: Arc<dyn LockManager>,
    ) -> Self {
        // Register per-txn lock timeout override when one is set.
        if let Some(timeout) = self.lock_timeout {
            lock_manager.set_txn_lock_timeout(lock_owner_id, timeout);
        }
        self.lock_owner_id = lock_owner_id;
        self.lock_manager = Some(lock_manager);
        self
    }

    pub fn with_lock_timeout(mut self, lock_timeout: Duration) -> Self {
        self.lock_timeout = Some(lock_timeout);
        self
    }

    pub fn with_serializable_coordinator(
        mut self,
        serializable_coordinator: Arc<dyn SerializableCoordinator>,
    ) -> Self {
        self.serializable_coordinator = Some(serializable_coordinator);
        self
    }

    pub fn with_cancellation_checker(
        mut self,
        cancellation_checker: Arc<dyn Fn() -> DbResult<()> + Send + Sync>,
    ) -> Self {
        self.cancellation_checker = Some(cancellation_checker);
        self
    }

    pub fn with_max_parallel_workers_per_query(mut self, workers: usize) -> Self {
        self.max_parallel_workers_per_query = workers.max(1);
        self
    }

    pub fn with_distributed_loopback_remote_nodes(mut self, node_ids: Vec<String>) -> Self {
        self.distributed_loopback_remote_nodes = Arc::new(node_ids);
        self
    }

    pub fn with_distributed_shared_storage_remote_nodes(mut self, node_ids: Vec<String>) -> Self {
        self.distributed_shared_storage_remote_nodes = Arc::new(node_ids);
        self
    }

    pub fn with_distributed_shard_leader_nodes(mut self, nodes: Vec<(u32, String)>) -> Self {
        self.distributed_shard_leader_nodes = Arc::new(nodes.into_iter().collect());
        self
    }

    pub fn distributed_shard_leader_node(&self, shard_id: u32) -> Option<&str> {
        self.distributed_shard_leader_nodes
            .get(&shard_id)
            .map(String::as_str)
    }

    pub fn with_distributed_current_shard_id(mut self, shard_id: Option<u32>) -> Self {
        self.distributed_current_shard_id = shard_id;
        self
    }

    pub fn distributed_hash_partitioning_enabled(&self) -> bool {
        let target_nodes = self.distributed_loopback_remote_nodes.as_ref();
        if target_nodes.is_empty() {
            return self.max_parallel_workers_per_query > 1;
        }

        let shared_storage_nodes = self.distributed_shared_storage_remote_nodes.as_ref();
        target_nodes.iter().all(|target| {
            target.starts_with("loopback:")
                || shared_storage_nodes
                    .iter()
                    .any(|node| node.eq_ignore_ascii_case(target))
        })
    }

    pub fn parallel_workers_for(&self, work_items: usize) -> usize {
        if work_items == 0 {
            return 1;
        }
        self.max_parallel_workers_per_query.max(1).min(work_items)
    }

    pub fn check_deadline(&self) -> DbResult<()> {
        if let Some(deadline) = self.statement_deadline {
            if Instant::now() >= deadline {
                return Err(DbError::query_canceled("statement timeout exceeded"));
            }
        }

        if let Some(cancellation_checker) = &self.cancellation_checker {
            cancellation_checker()?;
        }

        Ok(())
    }

    pub fn has_execution_interrupts(&self) -> bool {
        self.statement_deadline.is_some() || self.cancellation_checker.is_some()
    }

    /// Guard used by join paths that incrementally emit rows.
    pub fn check_join_row_limit(&self) -> DbResult<()> {
        self.check_deadline()
    }

    /// Returns `true` when the current memory usage exceeds the spill
    /// threshold (75% of `max_memory_bytes`) and a temp directory is
    /// available.  Callers can use this to decide whether to spill
    /// intermediate results to disk rather than accumulating more rows
    /// in memory.
    pub fn should_spill(&self) -> bool {
        self.server_data_dir.is_some()
            && self.memory_used() >= self.max_memory_bytes.saturating_mul(3) / 4
    }

    /// Track statement-scoped working-set allocation and enforce both memory
    /// and temporary workspace budgets.
    ///
    /// When spill-to-disk is active, spilled bytes are tracked
    /// separately via `temp_used`; in-memory bytes are tracked via
    /// `memory_used`.  Both budgets are enforced.
    pub fn track_memory(&self, bytes: u64) -> DbResult<()> {
        if bytes == 0 {
            return Ok(());
        }

        let previous_memory = self.memory_used.fetch_add(bytes, Ordering::Relaxed);
        let next_memory = previous_memory.checked_add(bytes).ok_or_else(|| {
            self.memory_used.fetch_sub(bytes, Ordering::Relaxed);
            DbError::program_limit("tracked memory usage counter overflowed for this statement")
        })?;
        if next_memory > self.max_memory_bytes {
            self.memory_used.fetch_sub(bytes, Ordering::Relaxed);
            return Err(DbError::program_limit(
                "maximum memory budget exceeded for this statement",
            ));
        }

        let previous_temp = self.temp_used.fetch_add(bytes, Ordering::Relaxed);
        let next_temp = previous_temp.checked_add(bytes).ok_or_else(|| {
            self.temp_used.fetch_sub(bytes, Ordering::Relaxed);
            self.memory_used.fetch_sub(bytes, Ordering::Relaxed);
            DbError::program_limit(
                "tracked temporary workspace usage counter overflowed for this statement",
            )
        })?;
        if next_temp > self.max_temp_bytes {
            self.temp_used.fetch_sub(bytes, Ordering::Relaxed);
            self.memory_used.fetch_sub(bytes, Ordering::Relaxed);
            return Err(DbError::program_limit(
                "maximum temporary workspace budget exceeded for this statement",
            ));
        }

        Ok(())
    }

    /// Return the current tracked memory usage.
    pub fn memory_used(&self) -> u64 {
        self.memory_used.load(Ordering::Relaxed)
    }

    /// Return the current tracked temporary workspace usage.
    pub fn temp_used(&self) -> u64 {
        self.temp_used.load(Ordering::Relaxed)
    }

    pub fn acquire_table_lock(&self, table_id: RelationId, mode: LockMode) -> DbResult<()> {
        if let Some(lock_manager) = &self.lock_manager {
            lock_manager.acquire_table_lock(self.lock_owner_id, table_id, mode)?;
        }
        Ok(())
    }

    pub fn try_acquire_table_lock_nowait(
        &self,
        table_id: RelationId,
        mode: LockMode,
    ) -> DbResult<()> {
        if let Some(lock_manager) = &self.lock_manager {
            lock_manager.try_acquire_table_lock_nowait(self.lock_owner_id, table_id, mode)?;
        }
        Ok(())
    }

    /// Acquire `mode` on `table_id`. When `nowait` is true, fail immediately
    /// rather than waiting if the lock cannot be granted.
    pub fn acquire_table_lock_with_nowait(
        &self,
        table_id: RelationId,
        mode: LockMode,
        nowait: bool,
    ) -> DbResult<()> {
        if let Some(lock_manager) = &self.lock_manager {
            if nowait {
                lock_manager.try_acquire_table_lock_nowait(self.lock_owner_id, table_id, mode)?;
            } else {
                lock_manager.acquire_table_lock(self.lock_owner_id, table_id, mode)?;
            }
        }
        Ok(())
    }

    pub fn acquire_tuple_lock(
        &self,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: LockMode,
    ) -> DbResult<()> {
        if let Some(lock_manager) = &self.lock_manager {
            lock_manager.acquire_tuple_lock(self.lock_owner_id, table_id, tuple_id, mode)?;
        }
        Ok(())
    }

    pub fn holds_tuple_lock(
        &self,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: LockMode,
    ) -> DbResult<bool> {
        if let Some(lock_manager) = &self.lock_manager {
            return lock_manager.txn_holds_tuple_lock(self.lock_owner_id, table_id, tuple_id, mode);
        }
        Ok(false)
    }

    /// Combined "is held" + "acquire" in a single shard-locked round
    /// trip. Returns whether the lock was already held in the requested
    /// mode prior to this call. The bulk UPDATE row loop uses this to
    /// halve the per-row lock-manager mutex traffic.
    pub fn acquire_tuple_lock_returning_was_held(
        &self,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: LockMode,
    ) -> DbResult<bool> {
        if let Some(lock_manager) = &self.lock_manager {
            return lock_manager.acquire_tuple_lock_returning_was_held(
                self.lock_owner_id,
                table_id,
                tuple_id,
                mode,
            );
        }
        Ok(false)
    }

    pub fn try_acquire_tuple_lock_nowait(
        &self,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: LockMode,
    ) -> DbResult<()> {
        if let Some(lock_manager) = &self.lock_manager {
            lock_manager.try_acquire_tuple_lock_nowait(
                self.lock_owner_id,
                table_id,
                tuple_id,
                mode,
            )?;
        }
        Ok(())
    }

    pub fn record_relation_read(&self, relation_id: RelationId) -> DbResult<()> {
        if self.isolation != IsolationLevel::Serializable {
            return Ok(());
        }
        if let Some(serializable_coordinator) = &self.serializable_coordinator {
            serializable_coordinator.record_relation_read(self.txn_id, relation_id)?;
        }
        Ok(())
    }

    pub fn record_relation_write(&self, relation_id: RelationId) -> DbResult<()> {
        if let Some(serializable_coordinator) = &self.serializable_coordinator {
            serializable_coordinator.record_relation_write(self.txn_id, relation_id)?;
        }
        Ok(())
    }

    pub fn record_tuple_write(&self, relation_id: RelationId, tuple_id: TupleId) -> DbResult<()> {
        if let Some(serializable_coordinator) = &self.serializable_coordinator {
            serializable_coordinator.record_tuple_write(self.txn_id, relation_id, tuple_id)?;
        }
        if self.storage_autocommit_fast_path {
            return Ok(());
        }
        if let Ok(mut writes) = self.statement_tuple_writes.lock() {
            writes.insert((relation_id, tuple_id));
        }
        Ok(())
    }

    pub fn record_tuple_writes(
        &self,
        relation_id: RelationId,
        tuple_ids: &[TupleId],
    ) -> DbResult<()> {
        if tuple_ids.is_empty() {
            return Ok(());
        }
        if let Some(serializable_coordinator) = &self.serializable_coordinator {
            for tuple_id in tuple_ids {
                serializable_coordinator.record_tuple_write(self.txn_id, relation_id, *tuple_id)?;
            }
        }
        if self.storage_autocommit_fast_path {
            return Ok(());
        }
        if let Ok(mut writes) = self.statement_tuple_writes.lock() {
            writes.extend(
                tuple_ids
                    .iter()
                    .copied()
                    .map(|tuple_id| (relation_id, tuple_id)),
            );
        }
        Ok(())
    }

    pub fn tuple_written_in_statement(&self, relation_id: RelationId, tuple_id: TupleId) -> bool {
        self.statement_tuple_writes
            .lock()
            .map(|writes| writes.contains(&(relation_id, tuple_id)))
            .unwrap_or(false)
    }

    pub fn record_sequence_next_value(&self, sequence_id: SequenceId, value: i64) -> DbResult<()> {
        let Some(state) = &self.sequence_session_state else {
            return Ok(());
        };
        let mut state = state.lock().map_err(|e| {
            DbError::internal(format!(
                "sequence session state poisoned during nextval: {e}"
            ))
        })?;
        state.record_next_value(sequence_id, value);
        Ok(())
    }

    pub fn record_sequence_set_value(
        &self,
        sequence_id: SequenceId,
        value: i64,
        is_called: bool,
    ) -> DbResult<()> {
        let Some(state) = &self.sequence_session_state else {
            return Ok(());
        };
        let mut state = state.lock().map_err(|e| {
            DbError::internal(format!(
                "sequence session state poisoned during setval: {e}"
            ))
        })?;
        state.record_set_value(sequence_id, value, is_called);
        Ok(())
    }

    pub fn current_sequence_value(&self, sequence_id: SequenceId) -> DbResult<Option<i64>> {
        let Some(state) = &self.sequence_session_state else {
            return Ok(None);
        };
        let state = state.lock().map_err(|e| {
            DbError::internal(format!(
                "sequence session state poisoned during currval: {e}"
            ))
        })?;
        Ok(state.current_value(sequence_id))
    }

    pub fn last_sequence_value(&self) -> DbResult<Option<i64>> {
        let Some(state) = &self.sequence_session_state else {
            return Ok(None);
        };
        let state = state.lock().map_err(|e| {
            DbError::internal(format!(
                "sequence session state poisoned during lastval: {e}"
            ))
        })?;
        Ok(state.last_value())
    }
}

impl Default for ExecutionContext {
    fn default() -> Self {
        // Default budgets are bounded so the bare `Executor` SDK surface still
        // refuses to load multi-GiB datasets in memory when callers forget to
        // wire engine-side budgets. Callers that need unbounded access still
        // override these explicitly (audit executor F1).
        Self {
            txn_id: TxnId::default(),
            isolation: IsolationLevel::ReadCommitted,
            implicit_transaction: false,
            storage_autocommit_fast_path: false,
            snapshot: Snapshot::new(TxnId::default(), TxnId::default(), Vec::new()),
            max_result_rows: 10_000_000,
            collect_row_limit: None,
            collect_row_offset: 0,
            max_result_bytes: 4 * 1024 * 1024 * 1024, // 4 GiB
            max_memory_bytes: 512 * 1024 * 1024,      // 512 MiB
            max_temp_bytes: 4 * 1024 * 1024 * 1024,   // 4 GiB
            statement_deadline: None,
            server_data_dir: None,
            budget_accounting_lock: Arc::new(Mutex::new(())),
            memory_used: Arc::new(AtomicU64::new(0)),
            temp_used: Arc::new(AtomicU64::new(0)),
            sequence_session_state: None,
            session_settings: None,
            session_setting_applier: None,
            lock_owner_id: TxnId::default(),
            lock_timeout: None,
            lock_manager: None,
            serializable_coordinator: None,
            cancellation_checker: None,
            relation_has_explicit_oid_cache: Arc::new(Mutex::new(HashMap::new())),
            compat_row_width_cache: Arc::new(Mutex::new(HashMap::new())),
            sequence_lookup_cache: Arc::new(Mutex::new(HashMap::new())),
            table_index_cache: Arc::new(Mutex::new(HashMap::new())),
            statement_tuple_writes: Arc::new(Mutex::new(HashSet::new())),
            graph_profile_actual_rows: Arc::new(Mutex::new(HashMap::new())),
            graph_profile_elapsed_nanos: Arc::new(Mutex::new(HashMap::new())),
            udf_depth: Arc::new(AtomicU32::new(0)),
            trigger_depth: Arc::new(AtomicU32::new(0)),
            fk_cascade_depth: Arc::new(AtomicU32::new(0)),
            max_parallel_workers_per_query: 1,
            parallel_scan_partition: None,
            distributed_loopback_remote_nodes: Arc::new(Vec::new()),
            distributed_shared_storage_remote_nodes: Arc::new(Vec::new()),
            distributed_shard_leader_nodes: Arc::new(HashMap::new()),
            distributed_current_shard_id: None,
            side_effect_query_cache: Arc::new(Mutex::new(HashMap::new())),
            trigger_modified_new: Arc::new(Mutex::new(None)),
        }
    }
}

#[allow(clippy::missing_fields_in_debug)]
impl fmt::Debug for ExecutionContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutionContext")
            .field("txn_id", &self.txn_id)
            .field("isolation", &self.isolation)
            .field("snapshot", &self.snapshot)
            .field("max_result_rows", &self.max_result_rows)
            .field("collect_row_limit", &self.collect_row_limit)
            .field("collect_row_offset", &self.collect_row_offset)
            .field("max_result_bytes", &self.max_result_bytes)
            .field("max_memory_bytes", &self.max_memory_bytes)
            .field("max_temp_bytes", &self.max_temp_bytes)
            .field("statement_deadline", &self.statement_deadline)
            .field("server_data_dir", &self.server_data_dir)
            .field("memory_used", &self.memory_used())
            .field("temp_used", &self.temp_used())
            .field(
                "sequence_session_state",
                &self.sequence_session_state.as_ref().map(|_| "configured"),
            )
            .field(
                "session_settings",
                &self.session_settings.as_ref().map(|_| "configured"),
            )
            .field("lock_owner_id", &self.lock_owner_id)
            .field(
                "lock_manager",
                &self.lock_manager.as_ref().map(|_| "configured"),
            )
            .field(
                "cancellation_checker",
                &self.cancellation_checker.as_ref().map(|_| "configured"),
            )
            .field(
                "relation_has_explicit_oid_cache",
                &self
                    .relation_has_explicit_oid_cache
                    .lock()
                    .map(|cache| cache.len())
                    .ok(),
            )
            .field(
                "compat_row_width_cache",
                &self
                    .compat_row_width_cache
                    .lock()
                    .map(|cache| cache.len())
                    .ok(),
            )
            .field(
                "max_parallel_workers_per_query",
                &self.max_parallel_workers_per_query,
            )
            .field(
                "distributed_loopback_remote_nodes",
                &self.distributed_loopback_remote_nodes,
            )
            .field(
                "distributed_shared_storage_remote_nodes",
                &self.distributed_shared_storage_remote_nodes,
            )
            .field(
                "distributed_shard_leader_nodes",
                &self.distributed_shard_leader_nodes,
            )
            .field(
                "distributed_current_shard_id",
                &self.distributed_current_shard_id,
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::*;

    #[test]
    fn track_memory_enforces_budget_across_threads() {
        let context = ExecutionContext::new(
            TxnId::default(),
            IsolationLevel::ReadCommitted,
            Snapshot::new(TxnId::default(), TxnId::default(), Vec::new()),
            u64::MAX,
            None,
            0,
            u64::MAX,
            100,
            100,
            None,
            None,
        );
        let barrier = Arc::new(Barrier::new(2));

        let first_context = context.clone();
        let first_barrier = barrier.clone();
        let first = thread::spawn(move || {
            first_barrier.wait();
            first_context.track_memory(80)
        });

        let second_context = context.clone();
        let second = thread::spawn(move || {
            barrier.wait();
            second_context.track_memory(80)
        });

        let first_result = first.join().expect("first worker must finish");
        let second_result = second.join().expect("second worker must finish");
        let success_count = [first_result, second_result].into_iter().flatten().count();

        assert_eq!(success_count, 1);
        assert_eq!(context.memory_used(), 80);
        assert_eq!(context.temp_used(), 80);
    }

    #[test]
    fn track_memory_rejects_counter_overflow() {
        let context = ExecutionContext::new(
            TxnId::default(),
            IsolationLevel::ReadCommitted,
            Snapshot::new(TxnId::default(), TxnId::default(), Vec::new()),
            u64::MAX,
            None,
            0,
            u64::MAX,
            u64::MAX,
            u64::MAX,
            None,
            None,
        );

        context.memory_used.store(u64::MAX - 5, Ordering::Relaxed);
        context.temp_used.store(u64::MAX - 5, Ordering::Relaxed);

        let error = context
            .track_memory(10)
            .expect_err("counter overflow must be rejected");
        assert!(error.to_string().contains("overflowed"));
        assert_eq!(context.memory_used(), u64::MAX - 5);
        assert_eq!(context.temp_used(), u64::MAX - 5);
    }

    #[test]
    fn distributed_hash_partitioning_enabled_for_generated_loopback_workers() {
        let context = ExecutionContext::default().with_max_parallel_workers_per_query(3);

        assert!(context.distributed_hash_partitioning_enabled());
    }

    #[test]
    fn distributed_hash_partitioning_enabled_for_configured_shared_storage_nodes() {
        let context = ExecutionContext::default()
            .with_max_parallel_workers_per_query(3)
            .with_distributed_loopback_remote_nodes(vec!["node-a".to_owned()])
            .with_distributed_shared_storage_remote_nodes(vec!["NODE-A".to_owned()]);

        assert!(context.distributed_hash_partitioning_enabled());
    }

    #[test]
    fn distributed_hash_partitioning_disabled_for_real_remote_nodes() {
        let context = ExecutionContext::default()
            .with_max_parallel_workers_per_query(3)
            .with_distributed_loopback_remote_nodes(vec!["remote-a".to_owned()]);

        assert!(!context.distributed_hash_partitioning_enabled());
    }
}
