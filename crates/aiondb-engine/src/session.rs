#![allow(clippy::pedantic)]

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use aiondb_eval::{CompatUserCast, CompatUserType, DomainDef};
use aiondb_executor::SequenceSessionState;
use aiondb_parser::Statement;
use aiondb_pg_compat::advisory::CompatAdvisoryKey;
use aiondb_plan::PhysicalPlan;
use aiondb_security::AuthenticatedIdentity;
use aiondb_tx::ActiveTransaction;
use subtle::ConstantTimeEq;

use aiondb_core::{DataType, RelationId, SchemaId, TenantId, TxnId};

use crate::prepared::{PortalState, PreparedStatementState};

const MAX_PENDING_NOTICES: usize = 1024;
const MAX_PARSED_SQL_CACHE_ENTRIES: usize = 64;
const MAX_PARSED_SQL_CACHE_SQL_BYTES: usize = 8 * 1024 * 1024;
const MAX_DESCRIBE_SQL_CACHE_ENTRIES: usize = 64;
const MAX_PLAN_CACHE_ENTRIES: usize = 128;
const MAX_PLAN_CACHE_ADMISSION_ENTRIES: usize = MAX_PLAN_CACHE_ENTRIES;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct StatementFingerprint {
    pub first: u64,
    pub second: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedSqlCacheEntry {
    pub statements: Arc<Vec<Statement>>,
    pub plan_fingerprints: Option<Arc<Vec<Option<StatementFingerprint>>>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct DescribeSqlCacheKey {
    pub statement_sql: String,
    pub txn_id: TxnId,
    pub search_path: String,
    pub current_user: String,
    pub session_user: String,
    pub catalog_revision: u64,
}

#[derive(Clone)]
pub struct SessionHandle {
    token: [u8; 32],
}

impl SessionHandle {
    pub(crate) const fn from_token(token: [u8; 32]) -> Self {
        Self { token }
    }

    /// Create a deterministic handle for use in tests only.
    #[doc(hidden)]
    pub fn test_handle() -> Self {
        Self { token: [0xAA; 32] }
    }

    pub(crate) fn stable_hash_key(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.token.hash(&mut hasher);
        hasher.finish()
    }
}

impl PartialEq for SessionHandle {
    fn eq(&self, other: &Self) -> bool {
        self.token.ct_eq(&other.token).into()
    }
}

impl Eq for SessionHandle {}

impl Hash for SessionHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.token.hash(state);
    }
}

impl fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SessionHandle({:02x}{:02x}{:02x}{:02x}..)",
            self.token[0], self.token[1], self.token[2], self.token[3]
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionLimits {
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

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            statement_timeout: Duration::from_secs(30),
            lock_timeout: Duration::from_secs(1),
            max_result_rows: 10_000,
            max_result_bytes: 8 * 1024 * 1024,
            max_memory_bytes: 64 * 1024 * 1024,
            max_temp_bytes: 256 * 1024 * 1024,
            max_parallel_workers_per_query: 1,
            max_portals: 64,
            max_prepared_statements: 128,
            max_recursive_iterations: 10_000,
            max_recursive_rows: 1_000_000,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub identity: AuthenticatedIdentity,
    pub is_superuser: bool,
    pub limits: SessionLimits,
    pub database_name: String,
    /// Active `DatabaseId` for this session, resolved at startup from the
    /// cluster catalog. Defaults to `DatabaseId::DEFAULT` until full
    /// ADR-0014 phase 3 routing is enabled. Phases 4+ use this field to
    /// route catalog/storage per database.
    pub active_database: aiondb_cluster::DatabaseId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionSavepointSnapshot {
    pub transaction_failed: bool,
    pub pending_notices: Vec<String>,
    pub tenant_id: Option<TenantId>,
    pub tenant_schema_id: Option<SchemaId>,
    pub tenant_schema_name: Option<String>,
    pub session_variables: HashMap<String, String>,
    pub local_session_variables: HashMap<String, String>,
    pub limits: SessionLimits,
    pub shell_types: HashSet<String>,
    pub compat_user_types: Arc<Vec<CompatUserType>>,
    pub compat_user_casts: Arc<Vec<CompatUserCast>>,
    pub domain_defs: Arc<Vec<DomainDef>>,
    pub next_compat_type_oid: i32,
    pub next_compat_cast_oid: i32,
    pub next_compat_function_oid: i32,
    pub compat_rules: HashMap<(String, String), CompatRule>,
    pub compat_aggregate_rewrites: HashMap<String, CompatAggregateRewrite>,
    pub plpgsql_prefix_reserved: bool,
    pub compat_misc_objects: HashMap<(String, String), String>,
    pub compat_misc_attrs: HashMap<(String, String), CompatMiscObjectAttrs>,
    pub compat_trigger_state: HashMap<(String, String), String>,
    pub security_labels: HashMap<(String, String), (Option<String>, String)>,
    pub comments: HashMap<(String, String), String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CompatAggregateRewrite {
    Avg,
    Sum,
    SumWithOffset(i64),
    AvgWithOffset(i64),
    HalfSum,
    MinLeast,
    NullBigInt,
    DirectSfuncFinalfunc { sfunc: String, finalfunc: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingCopyFromState {
    pub table_id: RelationId,
    pub statement_sql: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CompatAdvisorySessionState {
    pub session_locks: HashMap<CompatAdvisoryKey, u32>,
    pub xact_locks: HashMap<CompatAdvisoryKey, u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SavepointEntry {
    pub name: String,
    pub generation: u64,
    pub storage_savepoint_id: u64,
    pub catalog_savepoint_id: u64,
    pub session_state: SessionSavepointSnapshot,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PlanCacheKey {
    pub statement_fingerprint: StatementFingerprint,
    pub txn_id: TxnId,
    pub default_schema: Option<String>,
    pub search_path: Option<String>,
    pub current_user: String,
    pub session_user: String,
    pub catalog_revision: u64,
}

#[derive(Debug)]
pub(crate) struct SessionRecord {
    pub info: SessionInfo,
    pub active_txn: Option<ActiveTransaction>,
    pub active_txn_includes_catalog_participant: bool,
    pub active_txn_includes_storage_participant: bool,
    pub transaction_failed: bool,
    /// One-shot flag: when set, the next call to
    /// `mark_transaction_failed_if_active` consumes the flag instead of marking
    /// the transaction failed. Used by RESTORE to swallow the statement-level
    /// error after an internal savepoint rollback successfully kept the outer
    /// transaction usable, so the caller does not have to ROLLBACK.
    pub suppress_next_transaction_failure_mark: bool,
    pub implicit_txn_active: bool,
    pub next_savepoint_generation: u64,
    pub savepoints: Vec<SavepointEntry>,
    pub prepared_statements: HashMap<String, PreparedStatementState>,
    pub portals: HashMap<String, PortalState>,
    pub cancel_requested: bool,
    pub tenant_id: Option<TenantId>,
    pub tenant_schema_id: Option<SchemaId>,
    pub tenant_schema_name: Option<String>,
    pub session_variables: HashMap<String, String>,
    pub local_session_variables: HashMap<String, String>,
    pub parsed_sql_cache: HashMap<String, ParsedSqlCacheEntry>,
    pub parsed_sql_lru: VecDeque<String>,
    pub parsed_sql_cache_sql_bytes: usize,
    pub describe_sql_cache: HashMap<DescribeSqlCacheKey, crate::prepared::PreparedStatementDesc>,
    pub describe_sql_lru: VecDeque<DescribeSqlCacheKey>,
    pub plan_cache: HashMap<PlanCacheKey, Arc<PhysicalPlan>>,
    pub plan_cache_lru: VecDeque<PlanCacheKey>,
    pub plan_cache_admission_lru: VecDeque<u64>,
    pub sequence_state: Arc<Mutex<SequenceSessionState>>,
    pub shell_types: HashSet<String>,
    pub compat_user_types: Arc<Vec<CompatUserType>>,
    pub compat_user_casts: Arc<Vec<CompatUserCast>>,
    pub domain_defs: Arc<Vec<DomainDef>>,
    pub next_compat_type_oid: i32,
    pub next_compat_cast_oid: i32,
    pub next_compat_function_oid: i32,
    pub plpgsql_prefix_reserved: bool,
    pub pending_notices: Vec<String>,
    pub pending_copy_from: Option<PendingCopyFromState>,
    pub compat_advisory_locks: CompatAdvisorySessionState,
    /// SQL-level PREPARE/EXECUTE prepared statements.
    pub compat_prepared_sql: HashMap<String, CompatPreparedSql>,
    /// Compat rules stored by `CREATE RULE` for view DML rewriting.
    /// Key: `(canonical_view_name_lowercase, event)` where event is "INSERT", "UPDATE", or "DELETE".
    pub compat_rules: HashMap<(String, String), CompatRule>,
    /// Session-scoped compatibility rewrites recorded from `CREATE AGGREGATE`.
    pub compat_aggregate_rewrites: HashMap<String, CompatAggregateRewrite>,
    /// NOTIFY payloads buffered during the current transaction.  Flushed to
    /// the notification bus on COMMIT, discarded on ROLLBACK.
    pub pending_notifications: Vec<crate::engine::async_notify::Notification>,
    /// Security labels assigned via `SECURITY LABEL [FOR provider] ON ...`.
    /// Keyed by `(object_type, canonical_subject)`; the value pairs the
    /// optional provider (default "none") with the label text.
    pub security_labels: HashMap<(String, String), (Option<String>, String)>,
    /// Comments attached via `COMMENT ON object_type name IS '...';`.
    /// Keyed by `(object_type, canonical_subject)` with the comment text.
    pub comments: HashMap<(String, String), String>,
    /// Registry for PG catalog objects that AionDB accepts syntactically but
    /// does not execute at runtime (event triggers, foreign servers/tables,
    /// publications, subscriptions, policies). Keyed by `(kind, name)` with
    /// the raw DDL text as value. Used by pg_catalog views and duplicate-name
    /// detection on subsequent CREATE statements.
    pub compat_misc_objects: HashMap<(String, String), String>,
    /// Per-object attributes updated by `ALTER X …` statements (OWNER TO,
    /// SET SCHEMA, ENABLE/DISABLE, OPTIONS, TABLESPACE). Parallel map to
    /// `compat_misc_objects`, keyed by the same `(kind, canonical_name)`.
    pub compat_misc_attrs: HashMap<(String, String), CompatMiscObjectAttrs>,
    /// Trigger lifecycle state set by `ALTER TABLE <t> {ENABLE|DISABLE}
    /// TRIGGER <name>` or `ALTER TRIGGER <name> ON <t> ...`. Key:
    /// `(table_lowercase, trigger_lowercase)`. Value: "enabled" | "disabled"
    /// | "replica" | "always", or `"depends:<ext>"` for DEPENDS ON EXTENSION.
    pub compat_trigger_state: HashMap<(String, String), String>,
    pub created_at: Instant,
    pub last_active: Instant,
    /// Wall-clock time when the current `active_txn` was stored on this
    /// session.  Set to `Some(Instant::now())` when a transaction begins and
    /// reset to `None` when it commits or rolls back.  Used by
    /// `purge_expired_sessions()` to detect abandoned transactions.
    pub txn_started_at: Option<Instant>,
}

/// Per-object attributes recorded by CREATE/ALTER X statements for compat
/// misc objects (PUBLICATION, SUBSCRIPTION, SERVER, POLICY, EVENT TRIGGER,
/// EXTENSION, LANGUAGE, etc.). Fields are sparse: only populated when a
/// corresponding CREATE/ALTER action set them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompatMiscObjectAttrs {
    /// Role that owns the object, set by `ALTER X OWNER TO <role>`.
    pub owner: Option<String>,
    /// Schema relocation set by `ALTER X SET SCHEMA <schema>`.
    pub schema: Option<String>,
    /// Lifecycle state set by `ALTER X {ENABLE|DISABLE}` or
    /// `ALTER X ENABLE {REPLICA|ALWAYS} [RULE|TRIGGER]`.
    /// Canonical values: "enabled", "disabled", "replica", "always".
    pub state: Option<String>,
    /// `(option_name, option_value)` pairs from `ALTER X OPTIONS (...)`.
    pub options: Vec<(String, String)>,
    /// Tablespace set by `ALTER X SET TABLESPACE <name>`.
    pub tablespace: Option<String>,
    /// Version recorded by `ALTER EXTENSION ... UPDATE TO '<version>'`.
    pub version: Option<String>,
}

impl CompatMiscObjectAttrs {
    /// Returns `true` when at least one scalar field is populated.  Used to
    /// decide whether a newly-created compat object has any metadata worth
    /// persisting beyond an empty `options` list.
    #[must_use]
    pub fn has_any(&self) -> bool {
        self.owner.is_some()
            || self.schema.is_some()
            || self.state.is_some()
            || self.tablespace.is_some()
            || self.version.is_some()
    }
}

/// A PostgreSQL-compatible rewrite rule stored by `CREATE RULE`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompatRule {
    /// The SQL of the replacement action (e.g. `INSERT INTO foo VALUES(new.*, 57) RETURNING f1, f2`).
    pub action_sql: String,
    /// Number of RETURNING columns in the rule action (0 if no RETURNING).
    pub returning_count: usize,
}

/// A SQL-level prepared statement created by `PREPARE name [(types)] AS query`.
#[derive(Clone, Debug)]
pub(crate) struct CompatPreparedSql {
    /// The query SQL text (with `$1`, `$2`, etc. placeholders).
    pub query_sql: String,
    /// Parsed single statement for structured EXECUTE binding.
    pub statement: Statement,
    /// Resolved parameter types from PREPARE analysis.
    pub param_types: Vec<DataType>,
    /// Declared parameter type SQL fragments, kept for EXECUTE-time casts.
    pub declared_param_type_sqls: Vec<String>,
}

impl SessionRecord {
    pub(crate) fn snapshot_savepoint_state(&self) -> SessionSavepointSnapshot {
        SessionSavepointSnapshot {
            transaction_failed: self.transaction_failed,
            pending_notices: self.pending_notices.clone(),
            tenant_id: self.tenant_id,
            tenant_schema_id: self.tenant_schema_id,
            tenant_schema_name: self.tenant_schema_name.clone(),
            session_variables: self.session_variables.clone(),
            local_session_variables: self.local_session_variables.clone(),
            limits: self.info.limits.clone(),
            shell_types: self.shell_types.clone(),
            compat_user_types: self.compat_user_types.clone(),
            compat_user_casts: self.compat_user_casts.clone(),
            domain_defs: self.domain_defs.clone(),
            next_compat_type_oid: self.next_compat_type_oid,
            next_compat_cast_oid: self.next_compat_cast_oid,
            next_compat_function_oid: self.next_compat_function_oid,
            compat_rules: self.compat_rules.clone(),
            compat_aggregate_rewrites: self.compat_aggregate_rewrites.clone(),
            plpgsql_prefix_reserved: self.plpgsql_prefix_reserved,
            compat_misc_objects: self.compat_misc_objects.clone(),
            compat_misc_attrs: self.compat_misc_attrs.clone(),
            compat_trigger_state: self.compat_trigger_state.clone(),
            security_labels: self.security_labels.clone(),
            comments: self.comments.clone(),
        }
    }

    pub(crate) fn restore_savepoint_state(&mut self, snapshot: &SessionSavepointSnapshot) {
        self.transaction_failed = snapshot.transaction_failed;
        self.pending_notices.clone_from(&snapshot.pending_notices);
        self.tenant_id = snapshot.tenant_id;
        self.tenant_schema_id = snapshot.tenant_schema_id;
        self.tenant_schema_name
            .clone_from(&snapshot.tenant_schema_name);
        self.session_variables
            .clone_from(&snapshot.session_variables);
        self.local_session_variables
            .clone_from(&snapshot.local_session_variables);
        self.info.limits = snapshot.limits.clone();
        self.shell_types.clone_from(&snapshot.shell_types);
        self.compat_user_types
            .clone_from(&snapshot.compat_user_types);
        self.compat_user_casts
            .clone_from(&snapshot.compat_user_casts);
        self.domain_defs.clone_from(&snapshot.domain_defs);
        self.next_compat_type_oid = snapshot.next_compat_type_oid;
        self.next_compat_cast_oid = snapshot.next_compat_cast_oid;
        self.next_compat_function_oid = snapshot.next_compat_function_oid;
        self.compat_rules.clone_from(&snapshot.compat_rules);
        self.compat_aggregate_rewrites
            .clone_from(&snapshot.compat_aggregate_rewrites);
        self.plpgsql_prefix_reserved = snapshot.plpgsql_prefix_reserved;
        self.compat_misc_objects
            .clone_from(&snapshot.compat_misc_objects);
        self.compat_misc_attrs
            .clone_from(&snapshot.compat_misc_attrs);
        self.compat_trigger_state
            .clone_from(&snapshot.compat_trigger_state);
        self.security_labels.clone_from(&snapshot.security_labels);
        self.comments.clone_from(&snapshot.comments);
    }

    fn compat_cursor_statement_names(&self) -> Vec<String> {
        self.portals
            .values()
            .filter_map(|portal| {
                portal
                    .statement_name
                    .starts_with("__compat_cursor_")
                    .then_some(portal.statement_name.clone())
            })
            .collect()
    }

    pub(crate) fn clear_compat_cursor_portals(&mut self) {
        let compat_cursor_statement_names = self.compat_cursor_statement_names();
        self.portals
            .retain(|_, portal| !portal.statement_name.starts_with("__compat_cursor_"));
        for statement_name in compat_cursor_statement_names {
            self.prepared_statements.remove(&statement_name);
        }
    }

    pub(crate) fn clear_transaction_scoped_portals(&mut self) {
        let compat_cursor_statement_names = self.compat_cursor_statement_names();
        self.portals.clear();
        for statement_name in compat_cursor_statement_names {
            self.prepared_statements.remove(&statement_name);
        }
    }

    pub(crate) fn clear_transaction_scoped_portals_on_commit(&mut self) {
        let removed_compat_statement_names: Vec<String> = self
            .portals
            .values()
            .filter(|portal| {
                !portal.holdable && portal.statement_name.starts_with("__compat_cursor_")
            })
            .map(|portal| portal.statement_name.clone())
            .collect();
        self.portals.retain(|_, portal| portal.holdable);
        for statement_name in removed_compat_statement_names {
            self.prepared_statements.remove(&statement_name);
        }
    }

    pub(crate) fn clear_transaction_local_state(&mut self) {
        self.local_session_variables.clear();
    }

    pub(crate) fn clear_portals_created_since(&mut self, savepoint_generation: u64) {
        let compat_cursor_statement_names: Vec<String> = self
            .portals
            .values()
            .filter(|portal| {
                portal
                    .created_under_savepoint_generation
                    .is_some_and(|generation| generation >= savepoint_generation)
                    && portal.statement_name.starts_with("__compat_cursor_")
            })
            .map(|portal| portal.statement_name.clone())
            .collect();
        self.portals.retain(|_, portal| {
            portal
                .created_under_savepoint_generation
                .map_or(true, |generation| generation < savepoint_generation)
        });
        for statement_name in compat_cursor_statement_names {
            self.prepared_statements.remove(&statement_name);
        }
    }

    pub(crate) fn new(info: SessionInfo) -> Self {
        let now = Instant::now();
        Self {
            info,
            active_txn: None,
            active_txn_includes_catalog_participant: false,
            active_txn_includes_storage_participant: false,
            transaction_failed: false,
            suppress_next_transaction_failure_mark: false,
            implicit_txn_active: false,
            next_savepoint_generation: 0,
            savepoints: Vec::new(),
            prepared_statements: HashMap::new(),
            portals: HashMap::new(),
            cancel_requested: false,
            tenant_id: None,
            tenant_schema_id: None,
            tenant_schema_name: None,
            session_variables: HashMap::new(),
            local_session_variables: HashMap::new(),
            parsed_sql_cache: HashMap::new(),
            parsed_sql_lru: VecDeque::new(),
            parsed_sql_cache_sql_bytes: 0,
            describe_sql_cache: HashMap::new(),
            describe_sql_lru: VecDeque::new(),
            plan_cache: HashMap::new(),
            plan_cache_lru: VecDeque::new(),
            plan_cache_admission_lru: VecDeque::new(),
            sequence_state: Arc::new(Mutex::new(SequenceSessionState::default())),
            shell_types: HashSet::new(),
            compat_user_types: Arc::new(Vec::new()),
            compat_user_casts: Arc::new(Vec::new()),
            domain_defs: Arc::new(Vec::new()),
            next_compat_type_oid: 120_000,
            next_compat_cast_oid: 121_000,
            next_compat_function_oid: 122_000,
            plpgsql_prefix_reserved: false,
            pending_notices: Vec::new(),
            pending_copy_from: None,
            compat_advisory_locks: CompatAdvisorySessionState::default(),
            compat_prepared_sql: HashMap::new(),
            compat_rules: HashMap::new(),
            compat_aggregate_rewrites: HashMap::new(),
            pending_notifications: Vec::new(),
            security_labels: HashMap::new(),
            comments: HashMap::new(),
            compat_misc_objects: HashMap::new(),
            compat_misc_attrs: HashMap::new(),
            compat_trigger_state: HashMap::new(),
            created_at: now,
            last_active: now,
            txn_started_at: None,
        }
    }

    pub fn push_notice(&mut self, notice: String) {
        if self.pending_notices.len() < MAX_PENDING_NOTICES {
            self.pending_notices.push(notice);
        }
    }

    /// Extend pending notices, capping at [`MAX_PENDING_NOTICES`].
    pub fn extend_notices(&mut self, notices: impl IntoIterator<Item = String>) {
        for notice in notices {
            if self.pending_notices.len() >= MAX_PENDING_NOTICES {
                break;
            }
            self.pending_notices.push(notice);
        }
    }

    fn touch_parsed_sql_cache_entry(&mut self, sql: &str) {
        // Fast path for the dominant pattern of "client repeatedly
        // sends the same SQL" - back-of-deque match means already
        // most-recently-used, skip the linear scan + reinsert.
        if self.parsed_sql_lru.back().map(String::as_str) == Some(sql) {
            return;
        }
        if let Some(position) = self
            .parsed_sql_lru
            .iter()
            .position(|candidate| candidate == sql)
        {
            self.parsed_sql_lru.remove(position);
        }
        self.parsed_sql_lru.push_back(sql.to_owned());
    }

    fn touch_plan_cache_entry(&mut self, key: &PlanCacheKey) {
        // Fast path: when the same prepared statement is executed
        // repeatedly (the dominant OLTP shape), the key is already at
        // the most-recently-used end of the deque. Skip the linear
        // `position` scan + remove + push_back round-trip in that
        // case. The full path stays as a fallback for genuine LRU
        // shuffles.
        if self.plan_cache_lru.back() == Some(key) {
            return;
        }
        if let Some(position) = self
            .plan_cache_lru
            .iter()
            .position(|candidate| candidate == key)
        {
            self.plan_cache_lru.remove(position);
        }
        self.plan_cache_lru.push_back(key.clone());
    }

    fn touch_describe_sql_cache_entry(&mut self, key: &DescribeSqlCacheKey) {
        if self.describe_sql_lru.back() == Some(key) {
            return;
        }
        if let Some(position) = self
            .describe_sql_lru
            .iter()
            .position(|candidate| candidate == key)
        {
            self.describe_sql_lru.remove(position);
        }
        self.describe_sql_lru.push_back(key.clone());
    }

    fn record_admission_candidate(
        lru: &mut VecDeque<u64>,
        fingerprint: u64,
        max_entries: usize,
    ) -> bool {
        if let Some(position) = lru.iter().position(|candidate| *candidate == fingerprint) {
            lru.remove(position);
            return true;
        }
        if lru.len() >= max_entries {
            let _ = lru.pop_front();
        }
        lru.push_back(fingerprint);
        false
    }

    fn plan_cache_key_fingerprint(key: &PlanCacheKey) -> u64 {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    pub(crate) fn cached_sql(&mut self, sql: &str) -> Option<ParsedSqlCacheEntry> {
        let entry = self.parsed_sql_cache.get(sql).cloned();
        if entry.is_some() {
            self.touch_parsed_sql_cache_entry(sql);
        }
        entry
    }

    pub(crate) fn remember_sql(&mut self, sql: String, statements: Arc<Vec<Statement>>) {
        if sql.len() > MAX_PARSED_SQL_CACHE_SQL_BYTES {
            return;
        }
        if !self.parsed_sql_cache.contains_key(&sql) {
            while self.parsed_sql_cache.len() >= MAX_PARSED_SQL_CACHE_ENTRIES
                || self.parsed_sql_cache_sql_bytes.saturating_add(sql.len())
                    > MAX_PARSED_SQL_CACHE_SQL_BYTES
            {
                let Some(evicted) = self.parsed_sql_lru.pop_front() else {
                    self.parsed_sql_cache.clear();
                    self.parsed_sql_cache_sql_bytes = 0;
                    break;
                };
                if self.parsed_sql_cache.remove(&evicted).is_some() {
                    self.parsed_sql_cache_sql_bytes = self
                        .parsed_sql_cache_sql_bytes
                        .saturating_sub(evicted.len());
                }
            }
            self.parsed_sql_cache_sql_bytes =
                self.parsed_sql_cache_sql_bytes.saturating_add(sql.len());
        }
        self.touch_parsed_sql_cache_entry(&sql);
        self.parsed_sql_cache.insert(
            sql,
            ParsedSqlCacheEntry {
                statements,
                plan_fingerprints: None,
            },
        );
    }

    pub(crate) fn remember_sql_plan_fingerprints(
        &mut self,
        sql: &str,
        plan_fingerprints: Arc<Vec<Option<StatementFingerprint>>>,
    ) {
        if let Some(entry) = self.parsed_sql_cache.get_mut(sql) {
            entry.plan_fingerprints = Some(plan_fingerprints);
        }
    }

    pub(crate) fn cached_describe_sql(
        &mut self,
        key: &DescribeSqlCacheKey,
    ) -> Option<crate::prepared::PreparedStatementDesc> {
        let entry = self.describe_sql_cache.get(key).cloned();
        if entry.is_some() {
            self.touch_describe_sql_cache_entry(key);
        }
        entry
    }

    pub(crate) fn remember_describe_sql(
        &mut self,
        key: DescribeSqlCacheKey,
        desc: crate::prepared::PreparedStatementDesc,
    ) {
        if !self.describe_sql_cache.contains_key(&key) {
            while self.describe_sql_cache.len() >= MAX_DESCRIBE_SQL_CACHE_ENTRIES {
                let Some(evicted) = self.describe_sql_lru.pop_front() else {
                    self.describe_sql_cache.clear();
                    break;
                };
                if self.describe_sql_cache.remove(&evicted).is_some() {
                    break;
                }
            }
        }
        self.touch_describe_sql_cache_entry(&key);
        self.describe_sql_cache.insert(key, desc);
    }

    pub(crate) fn cached_plan(&mut self, key: &PlanCacheKey) -> Option<Arc<PhysicalPlan>> {
        let plan = self.plan_cache.get(key).cloned();
        if plan.is_some() {
            self.touch_plan_cache_entry(key);
        }
        plan
    }

    pub(crate) fn remember_plan(&mut self, key: PlanCacheKey, plan: Arc<PhysicalPlan>) {
        if !self.plan_cache.contains_key(&key) {
            if self.plan_cache.len() >= MAX_PLAN_CACHE_ENTRIES
                && !Self::record_admission_candidate(
                    &mut self.plan_cache_admission_lru,
                    Self::plan_cache_key_fingerprint(&key),
                    MAX_PLAN_CACHE_ADMISSION_ENTRIES,
                )
            {
                return;
            }
            while self.plan_cache.len() >= MAX_PLAN_CACHE_ENTRIES {
                let Some(evicted) = self.plan_cache_lru.pop_front() else {
                    self.plan_cache.clear();
                    break;
                };
                if self.plan_cache.remove(&evicted).is_some() {
                    break;
                }
            }
        }
        self.touch_plan_cache_entry(&key);
        self.plan_cache.insert(key, plan);
    }

    pub(crate) fn clear_plan_cache(&mut self) {
        self.plan_cache.clear();
        self.plan_cache_lru.clear();
        self.plan_cache_admission_lru.clear();
    }
}
