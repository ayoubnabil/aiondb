#![allow(
    clippy::cast_possible_truncation,
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::redundant_closure_for_method_calls,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::too_many_lines,
    clippy::unnecessary_wraps,
    clippy::wildcard_imports
)]

mod bootstrap;
pub mod catalog_wal;
mod reader;
pub mod recovery;
pub mod replication;
mod sequences;
pub mod snapshot;
mod system_tables;
#[cfg(test)]
pub(crate) mod test_support;
mod txn;
mod writer;

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
};

// Catalog state + active_txns are touched on every transaction begin/commit
// and every schema lookup. parking_lot gives a faster uncontended path and
// fairer scheduling under load. The replication export barrier stays on
// `std::sync::RwLock<()>` because it is shared across the engine and
// catalog-store crates and the migration cost outweighs the benefit on the
// gate path.
use parking_lot::{
    RwLock as PlRwLock, RwLockReadGuard as PlRwLockReadGuard,
    RwLockWriteGuard as PlRwLockWriteGuard,
};

use aiondb_catalog::{
    CastDescriptor, CatalogPrivilege, CatalogTxnParticipant, DomainDescriptor, EdgeLabelDescriptor,
    FunctionDescriptor, FunctionPrivilegeTarget, IndexDescriptor, NodeLabelDescriptor,
    PolicyDescriptor, PrivilegeDescriptor, PrivilegeTarget, QualifiedName, RoleDescriptor,
    RuleDescriptor, SchemaDescriptor, SequenceDescriptor, TableDescriptor, TableStatistics,
    TenantDescriptor, TriggerDescriptor, UserTypeDescriptor, ViewDescriptor,
};
use aiondb_core::{
    ColumnId, DbError, DbResult, IndexId, RelationId, SchemaId, SequenceId, SqlState, TenantId,
    TxnId, PG_TEMP_SCHEMA_NAME,
};
use serde::{Deserialize, Serialize};

pub use system_tables::DEFAULT_SCHEMA_NAME;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CatalogStoreOptions;

/// Wraps a `WalWriter` behind a `Mutex` for catalog WAL logging.
///
/// The catalog WAL writer owns the catalog WAL directory and provides
/// thread-safe append + flush.
pub struct CatalogWalHandle {
    writer: Mutex<aiondb_wal::WalWriter>,
    auto_txn_counter: AtomicU64,
    wal_dir: Option<PathBuf>,
}

impl std::fmt::Debug for CatalogWalHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogWalHandle").finish_non_exhaustive()
    }
}

impl CatalogWalHandle {
    /// Create a new handle wrapping the given writer.
    pub fn new(writer: aiondb_wal::WalWriter) -> Self {
        Self {
            writer: Mutex::new(writer),
            auto_txn_counter: AtomicU64::new(CATALOG_AUTO_TXN_BASE),
            wal_dir: None,
        }
    }

    /// Open a catalog WAL writer from config and wrap it in a handle.
    pub fn open(config: aiondb_wal::WalConfig) -> DbResult<Self> {
        let wal_dir = config.dir.clone();
        let writer = aiondb_wal::WalWriter::open(config)?;
        Ok(Self {
            writer: Mutex::new(writer),
            auto_txn_counter: AtomicU64::new(CATALOG_AUTO_TXN_BASE),
            wal_dir: Some(wal_dir),
        })
    }

    fn next_auto_txn_id(&self) -> TxnId {
        TxnId::new(self.auto_txn_counter.fetch_add(1, Ordering::Relaxed))
    }

    /// Append a WAL record and force it durable immediately.
    pub fn log_and_flush(&self, record: &aiondb_wal::WalRecord) -> DbResult<aiondb_wal::Lsn> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| DbError::internal(format!("catalog WAL writer lock poisoned: {e}")))?;
        let lsn = writer.append(record)?;
        writer.flush_durable()?;
        Ok(lsn)
    }

    /// Append a WAL record without flushing.
    pub fn log(&self, record: &aiondb_wal::WalRecord) -> DbResult<aiondb_wal::Lsn> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| DbError::internal(format!("catalog WAL writer lock poisoned: {e}")))?;
        writer.append(record)
    }

    /// Flush pending WAL data to disk.
    pub fn flush(&self) -> DbResult<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| DbError::internal(format!("catalog WAL writer lock poisoned: {e}")))?;
        writer.flush()
    }

    pub fn flush_and_last_lsn(&self) -> DbResult<aiondb_wal::Lsn> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| DbError::internal(format!("catalog WAL writer lock poisoned: {e}")))?;
        writer.flush_durable()?;
        Ok(writer.last_lsn().unwrap_or(aiondb_wal::Lsn::ZERO))
    }

    pub fn log_catalog_record(&self, record: &aiondb_wal::WalRecord) -> DbResult<aiondb_wal::Lsn> {
        match record.txn_id() {
            Some(txn_id) if txn_id == TxnId::default() => self.log_autocommit_record(record),
            _ => self.log_and_flush(record),
        }
    }

    pub fn log_begin_txn(&self, txn_id: TxnId) -> DbResult<aiondb_wal::Lsn> {
        self.log(&aiondb_wal::WalRecord::BeginTxn {
            txn_id,
            isolation: aiondb_wal::IsolationLevel::ReadCommitted,
        })
    }

    pub fn log_commit_txn(&self, txn_id: TxnId) -> DbResult<aiondb_wal::Lsn> {
        self.log_and_flush(&aiondb_wal::WalRecord::CommitTxn {
            txn_id,
            commit_ts: 0,
        })
    }

    pub fn log_abort_txn(&self, txn_id: TxnId) -> DbResult<aiondb_wal::Lsn> {
        self.log_and_flush(&aiondb_wal::WalRecord::AbortTxn { txn_id })
    }

    fn log_autocommit_record(&self, record: &aiondb_wal::WalRecord) -> DbResult<aiondb_wal::Lsn> {
        let txn_id = self.next_auto_txn_id();
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| DbError::internal(format!("catalog WAL writer lock poisoned: {e}")))?;
        writer.append(&aiondb_wal::WalRecord::BeginTxn {
            txn_id,
            isolation: aiondb_wal::IsolationLevel::ReadCommitted,
        })?;
        let lsn = writer.append(&remap_catalog_record_txn_id(record, txn_id))?;
        writer.append(&aiondb_wal::WalRecord::CommitTxn {
            txn_id,
            commit_ts: 0,
        })?;
        writer.flush_durable()?;
        Ok(lsn)
    }

    fn log_autocommit_records(
        &self,
        records: &[aiondb_wal::WalRecord],
    ) -> DbResult<aiondb_wal::Lsn> {
        if records.is_empty() {
            return Ok(aiondb_wal::Lsn::ZERO);
        }

        let txn_id = self.next_auto_txn_id();
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| DbError::internal(format!("catalog WAL writer lock poisoned: {e}")))?;
        writer.append(&aiondb_wal::WalRecord::BeginTxn {
            txn_id,
            isolation: aiondb_wal::IsolationLevel::ReadCommitted,
        })?;

        let mut last_lsn = aiondb_wal::Lsn::ZERO;
        for record in records {
            match record.txn_id() {
                Some(record_txn_id) if record_txn_id == TxnId::default() => {
                    last_lsn = writer.append(&remap_catalog_record_txn_id(record, txn_id))?;
                }
                Some(record_txn_id) => {
                    return Err(DbError::internal(format!(
                        "catalog WAL autocommit batch expected txn id 0, found {}",
                        record_txn_id.get()
                    )));
                }
                None => {
                    return Err(DbError::internal(
                        "catalog WAL autocommit batch received non-transactional record",
                    ));
                }
            }
        }

        writer.append(&aiondb_wal::WalRecord::CommitTxn {
            txn_id,
            commit_ts: 0,
        })?;
        writer.flush_durable()?;
        Ok(last_lsn)
    }

    #[must_use]
    pub fn wal_dir(&self) -> Option<&Path> {
        self.wal_dir.as_deref()
    }
}

#[derive(Debug)]
pub struct CatalogStore {
    pub(crate) state: Arc<PlRwLock<CatalogState>>,
    pub(crate) active_txns: Arc<PlRwLock<BTreeMap<TxnId, PendingCatalogTxn>>>,
    pub(crate) export_barrier: Arc<RwLock<()>>,
    /// Optional WAL handle for durable catalog persistence.
    pub(crate) wal: Option<Arc<CatalogWalHandle>>,
    /// Lock-free mirror of `state.revision` for the autocommit
    /// (TxnId::default()) read path. The plan-cache + search-path
    /// per-query lookups read this once per Execute on the OLTP
    /// hot path, so going through the state RwLock for what is
    /// effectively a `u64` load is wasted. Updated under the same
    /// write lock that bumps `state.revision`.
    pub(crate) cached_revision: Arc<AtomicU64>,
}

pub(crate) struct CatalogStateWriteGuard<'a> {
    _export_guard: std::sync::RwLockReadGuard<'a, ()>,
    state_guard: PlRwLockWriteGuard<'a, CatalogState>,
}

impl Deref for CatalogStateWriteGuard<'_> {
    type Target = CatalogState;

    fn deref(&self) -> &Self::Target {
        &self.state_guard
    }
}

impl DerefMut for CatalogStateWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state_guard
    }
}

pub(crate) struct ActiveTxnsWriteGuard<'a> {
    _export_guard: std::sync::RwLockReadGuard<'a, ()>,
    txns_guard: PlRwLockWriteGuard<'a, BTreeMap<TxnId, PendingCatalogTxn>>,
}

impl Deref for ActiveTxnsWriteGuard<'_> {
    type Target = BTreeMap<TxnId, PendingCatalogTxn>;

    fn deref(&self) -> &Self::Target {
        &self.txns_guard
    }
}

impl DerefMut for ActiveTxnsWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.txns_guard
    }
}

impl Clone for CatalogStore {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            active_txns: self.active_txns.clone(),
            export_barrier: self.export_barrier.clone(),
            wal: self.wal.clone(),
            cached_revision: self.cached_revision.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogState {
    pub(crate) revision: u64,
    pub(crate) next_schema_id: u64,
    pub(crate) next_table_id: u64,
    pub(crate) next_index_id: u64,
    pub(crate) next_sequence_id: u64,
    pub(crate) next_column_id: u64,
    pub(crate) schemas_by_id: BTreeMap<SchemaId, SchemaDescriptor>,
    pub(crate) schema_names: BTreeMap<String, SchemaId>,
    pub(crate) tables_by_id: BTreeMap<RelationId, TableDescriptor>,
    #[serde(default)]
    pub(crate) typed_table_types_by_id: BTreeMap<RelationId, String>,
    #[serde(with = "tuple_key_map")]
    pub(crate) table_names: BTreeMap<(SchemaId, String), RelationId>,
    pub(crate) indexes_by_id: BTreeMap<IndexId, IndexDescriptor>,
    #[serde(with = "tuple_key_map")]
    pub(crate) index_names: BTreeMap<(SchemaId, String), IndexId>,
    pub(crate) indexes_by_table: BTreeMap<RelationId, Vec<IndexId>>,
    pub(crate) sequences_by_id: BTreeMap<SequenceId, SequenceDescriptor>,
    #[serde(with = "tuple_key_map")]
    pub(crate) sequence_names: BTreeMap<(SchemaId, String), SequenceId>,
    pub(crate) sequence_values: BTreeMap<SequenceId, SequenceValueState>,
    pub(crate) views_by_id: BTreeMap<RelationId, ViewDescriptor>,
    #[serde(with = "tuple_key_map")]
    pub(crate) view_names: BTreeMap<(SchemaId, String), RelationId>,
    pub(crate) statistics: BTreeMap<RelationId, TableStatistics>,
    pub(crate) node_labels: BTreeMap<String, NodeLabelDescriptor>,
    pub(crate) edge_labels: BTreeMap<String, EdgeLabelDescriptor>,
    pub(crate) roles: BTreeMap<String, RoleDescriptor>,
    pub(crate) privileges: Vec<PrivilegeDescriptor>,
    pub(crate) next_tenant_id: u64,
    pub(crate) tenants_by_name: BTreeMap<String, TenantDescriptor>,
    pub(crate) functions: BTreeMap<String, Vec<FunctionDescriptor>>,
    pub(crate) triggers: Vec<TriggerDescriptor>,
    /// Persistent registry of `CREATE DOMAIN` types, keyed by the
    /// lowercase-normalised domain name. Recovery rebuilds this map from
    /// the WAL on startup.
    #[serde(default)]
    pub(crate) domains: BTreeMap<String, DomainDescriptor>,
    /// Persistent registry of `CREATE TYPE` enum / composite / shell
    /// definitions. Keyed by the lowercase-normalised type name.
    #[serde(default)]
    pub(crate) user_types: BTreeMap<String, UserTypeDescriptor>,
    /// Persistent registry of `CREATE CAST` entries, keyed by the
    /// (source_type_name, target_type_name) pair after normalisation.
    /// Stored as `Vec` so we can keep the original insertion order for
    /// snapshot replay; lookups are linear but the registry is small in
    /// practice (≤ a few hundred entries even on busy tenants).
    #[serde(default, with = "string_pair_key_map")]
    pub(crate) casts: BTreeMap<(String, String), CastDescriptor>,
    /// Persistent registry of row-level security policies, keyed by
    /// `(policy_name, table_name)` after normalisation. Mirrors PG's
    /// `pg_policy` shape where the same policy name can appear on
    /// different tables.
    #[serde(default, with = "string_pair_key_map")]
    pub(crate) policies: BTreeMap<(String, String), PolicyDescriptor>,
    /// Persistent registry of rewrite rules, keyed by
    /// `(rule_name, table_name)` after normalisation. Mirrors PG's
    /// `pg_rewrite` shape.
    #[serde(default, with = "string_pair_key_map")]
    pub(crate) rules: BTreeMap<(String, String), RuleDescriptor>,
    /// Persistent compatibility comments keyed by the `(object_type,
    /// object_identity)` pair used by the engine compat layer.
    #[serde(default, with = "string_pair_key_map")]
    pub(crate) comments: BTreeMap<(String, String), String>,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingCatalogTxn {
    pub(crate) state: CatalogState,
    pub(crate) base_revision: u64,
    pub(crate) dirty: bool,
    pub(crate) change_seq: u64,
    pub(crate) created_tables: BTreeSet<RelationId>,
    pub(crate) dropped_tables: BTreeMap<RelationId, DroppedTableState>,
    pub(crate) created_indexes: BTreeSet<IndexId>,
    pub(crate) created_sequences: BTreeSet<SequenceId>,
    pub(crate) dropped_indexes: BTreeMap<IndexId, IndexDescriptor>,
    pub(crate) dropped_sequences: BTreeMap<SequenceId, DroppedSequenceState>,
    pub(crate) merge_mode: CatalogTxnMergeMode,
    pub(crate) savepoints: BTreeMap<u64, CatalogSavepointSnapshot>,
    pub(crate) next_savepoint_id: u64,
    /// WAL records emitted by this pending transaction that have NOT yet
    /// been handed to the WAL writer. They are buffered here - rather than
    /// logged eagerly at each DDL - so that `rollback_to_savepoint` can
    /// simply truncate this buffer and recovery will never see the
    /// rolled-back records. At commit time, the surviving records are
    /// flushed in order, then the `CommitTxn` marker is written.
    pub(crate) pending_wal_records: Vec<aiondb_wal::WalRecord>,
}

#[derive(Clone, Debug)]
pub(crate) struct CatalogSavepointSnapshot {
    pub(crate) state: Arc<CatalogState>,
    pub(crate) dirty: bool,
    pub(crate) change_seq: u64,
    pub(crate) created_tables: Arc<BTreeSet<RelationId>>,
    pub(crate) dropped_tables: Arc<BTreeMap<RelationId, DroppedTableState>>,
    pub(crate) created_indexes: Arc<BTreeSet<IndexId>>,
    pub(crate) created_sequences: Arc<BTreeSet<SequenceId>>,
    pub(crate) dropped_indexes: Arc<BTreeMap<IndexId, IndexDescriptor>>,
    pub(crate) dropped_sequences: Arc<BTreeMap<SequenceId, DroppedSequenceState>>,
    pub(crate) merge_mode: CatalogTxnMergeMode,
    /// Position in `PendingCatalogTxn::pending_wal_records` to truncate
    /// back to on `ROLLBACK TO SAVEPOINT`. All records appended after
    /// this index belong to change logs that the user has discarded and
    pub(crate) pending_wal_records_len: usize,
}

#[derive(Clone, Debug)]
pub(crate) enum CatalogTxnChange {
    CreateTable(RelationId),
    DropTable(DroppedTableState),
    CreateIndex(IndexId),
    CreateSequence(SequenceId),
    DropIndex(IndexDescriptor),
    DropSequence(DroppedSequenceState),
    ComplexWrite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SequenceValueState {
    pub(crate) current_value: i64,
    pub(crate) is_called: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DroppedSequenceState {
    pub(crate) descriptor: SequenceDescriptor,
    pub(crate) runtime: SequenceValueState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DroppedTableState {
    pub(crate) descriptor: TableDescriptor,
    pub(crate) statistics: Option<TableStatistics>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CatalogTxnMergeMode {
    Empty,
    CreateOnly,
    DropOnly,
    CreateAndDrop,
    Complex,
}

impl Default for CatalogState {
    fn default() -> Self {
        Self {
            revision: 0,
            next_schema_id: 1,
            next_table_id: 1,
            next_index_id: 1,
            next_sequence_id: 1,
            next_column_id: 1,
            schemas_by_id: BTreeMap::new(),
            schema_names: BTreeMap::new(),
            tables_by_id: BTreeMap::new(),
            typed_table_types_by_id: BTreeMap::new(),
            table_names: BTreeMap::new(),
            indexes_by_id: BTreeMap::new(),
            index_names: BTreeMap::new(),
            indexes_by_table: BTreeMap::new(),
            sequences_by_id: BTreeMap::new(),
            sequence_names: BTreeMap::new(),
            sequence_values: BTreeMap::new(),
            views_by_id: BTreeMap::new(),
            view_names: BTreeMap::new(),
            statistics: BTreeMap::new(),
            node_labels: BTreeMap::new(),
            edge_labels: BTreeMap::new(),
            roles: BTreeMap::new(),
            privileges: Vec::new(),
            next_tenant_id: 1,
            tenants_by_name: BTreeMap::new(),
            functions: BTreeMap::new(),
            triggers: Vec::new(),
            domains: BTreeMap::new(),
            user_types: BTreeMap::new(),
            casts: BTreeMap::new(),
            policies: BTreeMap::new(),
            rules: BTreeMap::new(),
            comments: BTreeMap::new(),
        }
    }
}

impl Default for CatalogStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CatalogStore {
    pub fn new() -> Self {
        Self::with_options(CatalogStoreOptions)
    }

    pub fn with_options(_options: CatalogStoreOptions) -> Self {
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);
        let cached_revision = Arc::new(AtomicU64::new(state.revision));
        Self {
            state: Arc::new(PlRwLock::new(state)),
            active_txns: Arc::new(PlRwLock::new(BTreeMap::new())),
            export_barrier: Arc::new(RwLock::new(())),
            wal: None,
            cached_revision,
        }
    }

    /// Create a catalog store with WAL-backed durability.
    ///
    /// The WAL handle is used to log every catalog mutation so that the
    /// catalog survives crashes. Pass the same `CatalogWalHandle` that
    /// was used (or will be used) during recovery.
    pub fn new_with_wal(wal: Arc<CatalogWalHandle>) -> Self {
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);
        let cached_revision = Arc::new(AtomicU64::new(state.revision));
        Self {
            state: Arc::new(PlRwLock::new(state)),
            active_txns: Arc::new(PlRwLock::new(BTreeMap::new())),
            export_barrier: Arc::new(RwLock::new(())),
            wal: Some(wal),
            cached_revision,
        }
    }

    /// Create a catalog store from a recovered state with WAL durability.
    pub fn from_recovered(state: CatalogState, wal: Arc<CatalogWalHandle>) -> Self {
        let cached_revision = Arc::new(AtomicU64::new(state.revision));
        Self {
            state: Arc::new(PlRwLock::new(state)),
            active_txns: Arc::new(PlRwLock::new(BTreeMap::new())),
            export_barrier: Arc::new(RwLock::new(())),
            wal: Some(wal),
            cached_revision,
        }
    }

    /// Create a catalog store from a recovered state without WAL.
    pub fn from_recovered_no_wal(state: CatalogState) -> Self {
        let cached_revision = Arc::new(AtomicU64::new(state.revision));
        Self {
            state: Arc::new(PlRwLock::new(state)),
            active_txns: Arc::new(PlRwLock::new(BTreeMap::new())),
            export_barrier: Arc::new(RwLock::new(())),
            wal: None,
            cached_revision,
        }
    }

    #[doc(hidden)]
    pub fn set_replication_export_barrier(&mut self, barrier: Arc<RwLock<()>>) {
        self.export_barrier = barrier;
    }

    /// Log a catalog WAL record and force it durable if WAL is configured.
    ///
    /// DDL mutations are infrequent enough that the per-operation fsync has
    /// negligible throughput impact, while the durability guarantee prevents
    /// catalog state from diverging from the WAL after an OS crash.
    pub(crate) fn log_catalog_record(&self, record: &aiondb_wal::WalRecord) -> DbResult<()> {
        if let Some(wal) = &self.wal {
            wal.log_catalog_record(record)?;
        }
        Ok(())
    }

    pub(crate) fn write_autocommit_catalog_batch<R>(
        &self,
        mutate: impl FnOnce(&mut CatalogState) -> DbResult<(R, Vec<aiondb_wal::WalRecord>)>,
    ) -> DbResult<R> {
        let mut state = self.write_state()?;
        let next_revision = state.revision.wrapping_add(1);
        let mut staged_state = state.clone();
        let (result, wal_records) = mutate(&mut staged_state)?;
        if let Some(wal) = &self.wal {
            wal.log_autocommit_records(&wal_records)?;
        }
        staged_state.revision = next_revision;
        *state = staged_state;
        // SECURITY: mirror the new revision into `cached_revision` so the
        // lock-free autocommit reader path (`reader::catalog_revision`)
        // does not return a stale value after autocommit DDL or
        // GRANT/REVOKE. Without this, prepared-statement re-describe and
        // any other revision-keyed cache treats the catalog as unchanged
        // and may reuse stale schema/column metadata.
        self.cached_revision
            .store(state.revision, Ordering::Release);
        Ok(result)
    }

    pub(crate) fn write_catalog_change<R>(
        &self,
        txn: TxnId,
        change: CatalogTxnChange,
        mutate: impl FnOnce(&mut CatalogState) -> DbResult<R>,
        wal_records: impl FnOnce(&R) -> DbResult<Vec<aiondb_wal::WalRecord>>,
    ) -> DbResult<R> {
        if Self::is_autocommit_txn(txn) {
            return self.write_autocommit_catalog_batch(|state| {
                let result = mutate(state)?;
                let records = wal_records(&result)?;
                Ok((result, records))
            });
        }

        let result = self.write_catalog_state(txn, true, change, mutate)?;
        // Buffer the records on the pending transaction instead of
        // logging them eagerly to the WAL. `ROLLBACK TO SAVEPOINT` can
        // then truncate the buffer so discarded DDL is never replayed
        // at recovery, and `commit_txn` flushes everything that survived.
        // If the transaction is not explicitly registered via
        // `begin_txn` (implicit txn path used by some internal call
        // sites and tests), there is no pending entry to buffer into,
        // so we fall back to eager logging - savepoint semantics do not
        // apply to unregistered transactions anyway.
        let records = wal_records(&result)?;
        if !records.is_empty() {
            let mut active_txns = self.write_active_txns()?;
            if let Some(pending) = active_txns.get_mut(&txn) {
                pending.pending_wal_records.extend(records);
            } else {
                drop(active_txns);
                for record in records {
                    self.log_catalog_record(&record)?;
                }
            }
        }
        Ok(result)
    }

    pub(crate) fn write_catalog_state_with_record<R>(
        &self,
        txn: TxnId,
        dirty: bool,
        change: CatalogTxnChange,
        mutate: impl FnOnce(&mut CatalogState) -> DbResult<R>,
        make_record: impl FnOnce(&R) -> DbResult<aiondb_wal::WalRecord>,
    ) -> DbResult<R> {
        if Self::is_autocommit_txn(txn) {
            return self.write_autocommit_catalog_batch(|state| {
                let result = mutate(state)?;
                let record = make_record(&result)?;
                Ok((result, vec![record]))
            });
        }

        let result = self.write_catalog_state(txn, dirty, change, mutate)?;
        let record = make_record(&result)?;
        // Same rationale as `write_catalog_change`: buffer on the pending
        // transaction so `ROLLBACK TO SAVEPOINT` can truncate. Fall back
        // to eager logging for read-only records or for unregistered
        // transactions (implicit-txn path).
        if dirty {
            let mut active_txns = self.write_active_txns()?;
            if let Some(pending) = active_txns.get_mut(&txn) {
                pending.pending_wal_records.push(record);
            } else {
                drop(active_txns);
                self.log_catalog_record(&record)?;
            }
        } else {
            self.log_catalog_record(&record)?;
        }
        Ok(result)
    }

    pub(crate) fn read_state(&self) -> Result<PlRwLockReadGuard<'_, CatalogState>, DbError> {
        Ok(self.state.read())
    }

    pub(crate) fn write_state(&self) -> Result<CatalogStateWriteGuard<'_>, DbError> {
        let export_guard = self.export_barrier.read().map_err(|e| {
            DbError::internal(format!("catalog replication export barrier poisoned: {e}"))
        })?;
        let state_guard = self.state.write();
        Ok(CatalogStateWriteGuard {
            _export_guard: export_guard,
            state_guard,
        })
    }

    pub(crate) fn read_active_txns(
        &self,
    ) -> Result<PlRwLockReadGuard<'_, BTreeMap<TxnId, PendingCatalogTxn>>, DbError> {
        Ok(self.active_txns.read())
    }

    pub(crate) fn write_active_txns(&self) -> Result<ActiveTxnsWriteGuard<'_>, DbError> {
        let export_guard = self.export_barrier.read().map_err(|e| {
            DbError::internal(format!("catalog replication export barrier poisoned: {e}"))
        })?;
        let txns_guard = self.active_txns.write();
        Ok(ActiveTxnsWriteGuard {
            _export_guard: export_guard,
            txns_guard,
        })
    }

    pub(crate) fn is_autocommit_txn(txn: TxnId) -> bool {
        txn == TxnId::default()
    }

    pub(crate) fn read_catalog_state<R>(
        &self,
        txn: TxnId,
        f: impl FnOnce(&CatalogState) -> DbResult<R>,
    ) -> DbResult<R> {
        if !Self::is_autocommit_txn(txn) {
            let active_txns = self.read_active_txns()?;
            if let Some(pending) = active_txns.get(&txn) {
                return f(&pending.state);
            }
        }

        let state = self.read_state()?;
        f(&state)
    }

    pub(crate) fn write_catalog_state<R>(
        &self,
        txn: TxnId,
        dirty: bool,
        change: CatalogTxnChange,
        f: impl FnOnce(&mut CatalogState) -> DbResult<R>,
    ) -> DbResult<R> {
        if !Self::is_autocommit_txn(txn) {
            let mut active_txns = self.write_active_txns()?;
            if let Some(pending) = active_txns.get_mut(&txn) {
                let result = f(&mut pending.state)?;
                if dirty {
                    pending.dirty = true;
                    pending.change_seq = pending.change_seq.wrapping_add(1);
                    pending.state.revision = pending.state.revision.wrapping_add(1);
                    Self::record_txn_change(pending, change);
                }
                return Ok(result);
            }
        }

        let mut state = self.write_state()?;
        let result = f(&mut state)?;
        if dirty {
            state.revision = state.revision.wrapping_add(1);
            // Mirror the new revision so autocommit readers can pick
            // it up without taking the state lock.
            self.cached_revision
                .store(state.revision, Ordering::Release);
        }
        Ok(result)
    }

    pub(crate) fn normalize_identifier(value: &str) -> String {
        value.to_ascii_lowercase()
    }

    pub(crate) fn trigger_table_matches(stored_table_name: &str, lookup_table_name: &str) -> bool {
        let stored = Self::normalize_identifier(stored_table_name);
        let lookup = Self::normalize_identifier(lookup_table_name);
        if stored == lookup {
            return true;
        }

        let stored_qualified = stored.contains('.');
        let lookup_qualified = lookup.contains('.');
        if stored_qualified && lookup_qualified {
            return false;
        }

        let stored_bare = stored
            .rsplit_once('.')
            .map(|(_, tail)| tail)
            .unwrap_or(&stored);
        let lookup_bare = lookup
            .rsplit_once('.')
            .map(|(_, tail)| tail)
            .unwrap_or(&lookup);
        stored_bare == lookup_bare
    }

    pub(crate) fn canonicalize_function_privilege(
        privilege: PrivilegeDescriptor,
    ) -> PrivilegeDescriptor {
        let PrivilegeDescriptor {
            role_name,
            privilege: privilege_kind,
            target,
        } = privilege;
        let target = match (privilege_kind, target) {
            (CatalogPrivilege::Execute, PrivilegeTarget::Table(name)) => {
                PrivilegeTarget::Function(FunctionPrivilegeTarget {
                    name,
                    arg_types: None,
                })
            }
            (_, target) => target,
        };
        PrivilegeDescriptor {
            role_name,
            privilege: privilege_kind,
            target,
        }
    }

    pub(crate) fn canonicalize_privilege_list(privileges: &mut Vec<PrivilegeDescriptor>) {
        let mut canonical = Vec::with_capacity(privileges.len());
        for privilege in privileges.drain(..) {
            let privilege = Self::canonicalize_function_privilege(privilege);
            if !canonical.contains(&privilege) {
                canonical.push(privilege);
            }
        }
        *privileges = canonical;
    }

    fn qualified_name_eq_case_insensitive(left: &QualifiedName, right: &QualifiedName) -> bool {
        Self::normalize_identifier(left.object_name())
            == Self::normalize_identifier(right.object_name())
            && match (left.schema_name(), right.schema_name()) {
                (Some(left_schema), Some(right_schema)) => {
                    Self::normalize_identifier(left_schema)
                        == Self::normalize_identifier(right_schema)
                }
                (None, None) => true,
                _ => false,
            }
    }

    fn qualified_name_matches_for_privilege_target(
        left: &QualifiedName,
        right: &QualifiedName,
    ) -> bool {
        if Self::normalize_identifier(left.object_name())
            != Self::normalize_identifier(right.object_name())
        {
            return false;
        }
        match (left.schema_name(), right.schema_name()) {
            (Some(left_schema), Some(right_schema)) => {
                Self::normalize_identifier(left_schema) == Self::normalize_identifier(right_schema)
            }
            // If either side is unqualified, treat it as a match for cleanup.
            (None, _) | (_, None) => true,
        }
    }

    pub(crate) fn privilege_target_references_role(
        target: &PrivilegeTarget,
        role_lookup: &str,
    ) -> bool {
        matches!(
            target,
            PrivilegeTarget::Role(target_role)
                if Self::normalize_identifier(target_role) == role_lookup
        )
    }

    pub(crate) fn remove_privileges_for_dropped_relation(
        state: &mut CatalogState,
        relation_name: &QualifiedName,
    ) {
        state.privileges.retain(|privilege| {
            !matches!(
                &privilege.target,
                PrivilegeTarget::Table(target_name)
                    if Self::qualified_name_matches_for_privilege_target(target_name, relation_name)
            )
        });
    }

    pub(crate) fn remove_privileges_for_dropped_schema(
        state: &mut CatalogState,
        schema_name: &str,
    ) {
        let dropped_schema = Self::normalize_identifier(schema_name);
        state.privileges.retain(|privilege| {
            !matches!(
                &privilege.target,
                PrivilegeTarget::Schema(target_schema)
                    if Self::normalize_identifier(target_schema) == dropped_schema
            )
        });
    }

    pub(crate) fn remove_privileges_for_dropped_function(
        state: &mut CatalogState,
        function_name: &str,
    ) {
        let dropped = QualifiedName::parse(function_name);
        state.privileges.retain(|privilege| {
            !matches!(
                &privilege.target,
                PrivilegeTarget::Function(target)
                    if Self::qualified_name_matches_for_privilege_target(&target.name, &dropped)
            )
        });
    }

    pub(crate) fn privilege_target_exists(state: &CatalogState, target: &PrivilegeTarget) -> bool {
        match target {
            PrivilegeTarget::Table(name) => {
                let object_lookup = Self::normalize_identifier(name.object_name());
                if let Some(schema_name) = name.schema_name() {
                    let schema_lookup = Self::normalize_identifier(schema_name);
                    let Some(&schema_id) = state.schema_names.get(&schema_lookup) else {
                        return false;
                    };
                    let key = (schema_id, object_lookup);
                    state.table_names.contains_key(&key)
                        || state.view_names.contains_key(&key)
                        || state.sequence_names.contains_key(&key)
                } else {
                    state.table_names.keys().any(|(_, n)| n == &object_lookup)
                        || state.view_names.keys().any(|(_, n)| n == &object_lookup)
                        || state
                            .sequence_names
                            .keys()
                            .any(|(_, n)| n == &object_lookup)
                }
            }
            PrivilegeTarget::Function(func) => {
                let object_lookup = Self::normalize_identifier(func.name.object_name());
                let full_lookup = Self::normalize_identifier(&func.name.to_string());
                state.functions.contains_key(&object_lookup)
                    || state.functions.contains_key(&full_lookup)
            }
            PrivilegeTarget::Schema(schema_name) => {
                let schema_lookup = Self::normalize_identifier(schema_name);
                state.schema_names.contains_key(&schema_lookup)
            }
            PrivilegeTarget::Database(_) => true,
            PrivilegeTarget::Role(role_name) => {
                let role_lookup = Self::normalize_identifier(role_name);
                state.roles.contains_key(&role_lookup)
            }
        }
    }

    pub(crate) fn privilege_matches_revoke(
        existing: &PrivilegeDescriptor,
        requested: &PrivilegeDescriptor,
    ) -> bool {
        if existing == requested {
            return true;
        }
        if existing.privilege != requested.privilege || existing.role_name != requested.role_name {
            return false;
        }
        if requested.privilege != CatalogPrivilege::Execute {
            return false;
        }
        match (&existing.target, &requested.target) {
            (PrivilegeTarget::Table(existing_name), PrivilegeTarget::Function(requested_fn)) => {
                Self::qualified_name_eq_case_insensitive(existing_name, &requested_fn.name)
            }
            (PrivilegeTarget::Function(existing_fn), PrivilegeTarget::Table(requested_name))
                if existing_fn.arg_types.is_none() =>
            {
                Self::qualified_name_eq_case_insensitive(&existing_fn.name, requested_name)
            }
            (PrivilegeTarget::Function(existing_fn), PrivilegeTarget::Function(requested_fn))
                if existing_fn.arg_types.is_none() =>
            {
                Self::qualified_name_eq_case_insensitive(&existing_fn.name, &requested_fn.name)
            }
            (PrivilegeTarget::Function(existing_fn), PrivilegeTarget::Function(requested_fn))
                if existing_fn.arg_types == requested_fn.arg_types =>
            {
                Self::qualified_name_eq_case_insensitive(&existing_fn.name, &requested_fn.name)
                    || Self::qualified_name_matches_for_privilege_target(
                        &existing_fn.name,
                        &requested_fn.name,
                    )
                    || Self::qualified_name_matches_for_privilege_target(
                        &requested_fn.name,
                        &existing_fn.name,
                    )
            }
            _ => false,
        }
    }

    pub(crate) fn schema_lookup_name(name: &QualifiedName) -> String {
        match name.schema_name() {
            Some(schema_name) => Self::normalize_identifier(schema_name),
            None => Self::normalize_identifier(name.object_name()),
        }
    }

    pub(crate) fn object_lookup_name(name: &QualifiedName) -> String {
        Self::normalize_identifier(name.object_name())
    }

    pub(crate) fn public_schema_id(state: &CatalogState) -> Result<SchemaId, DbError> {
        state
            .schema_names
            .get(DEFAULT_SCHEMA_NAME)
            .copied()
            .ok_or_else(|| DbError::internal("catalog bootstrap is missing schema public"))
    }

    pub(crate) fn find_schema_id(state: &CatalogState, name: &QualifiedName) -> Option<SchemaId> {
        let lookup = Self::schema_lookup_name(name);
        state.schema_names.get(&lookup).copied()
    }

    pub(crate) fn resolve_schema_id(
        state: &CatalogState,
        name: &QualifiedName,
    ) -> Result<SchemaId, DbError> {
        match name.schema_name() {
            Some(schema_name) => state
                .schema_names
                .get(&Self::normalize_identifier(schema_name))
                .copied()
                .ok_or_else(|| undefined_schema(schema_name)),
            None => Self::public_schema_id(state),
        }
    }

    /// Resolve a named catalog object (table, sequence, view) by qualified
    /// name, handling schema qualification and temp-schema fallback.
    ///
    /// `names` maps `(schema_id, normalized_name)` to an object key, and
    /// `by_id` maps that key back to the descriptor.
    pub(crate) fn lookup_named_object<K, V>(
        state: &CatalogState,
        name: &QualifiedName,
        names: &std::collections::BTreeMap<(SchemaId, String), K>,
        by_id: &std::collections::BTreeMap<K, V>,
    ) -> DbResult<Option<V>>
    where
        K: Ord + Clone,
        V: Clone,
    {
        let lookup_name = Self::object_lookup_name(name);
        let schema_id = match name.schema_name() {
            Some(schema_name) => match state
                .schema_names
                .get(&Self::normalize_identifier(schema_name))
                .copied()
            {
                Some(id) => id,
                None => return Ok(None),
            },
            None => {
                if let Some(temp_id) = state
                    .schema_names
                    .get(&Self::normalize_identifier(PG_TEMP_SCHEMA_NAME))
                    .copied()
                {
                    if let Some(key) = names.get(&(temp_id, lookup_name.clone())) {
                        return Ok(by_id.get(key).cloned());
                    }
                }
                Self::public_schema_id(state)?
            }
        };
        Ok(names
            .get(&(schema_id, lookup_name))
            .and_then(|key| by_id.get(key).cloned()))
    }

    pub(crate) fn ensure_schema_exists(
        state: &CatalogState,
        schema_id: SchemaId,
    ) -> Result<(), DbError> {
        if state.schemas_by_id.contains_key(&schema_id) {
            Ok(())
        } else {
            Err(undefined_schema_id(schema_id))
        }
    }

    pub(crate) fn schema_name_by_id(
        state: &CatalogState,
        schema_id: SchemaId,
    ) -> Result<String, DbError> {
        state
            .schemas_by_id
            .get(&schema_id)
            .map(|schema| schema.name.clone())
            .ok_or_else(|| undefined_schema_id(schema_id))
    }

    pub(crate) fn next_schema_id(state: &mut CatalogState, requested: SchemaId) -> SchemaId {
        // If a caller pins a specific id (replay / restore), refuse it when the
        // slot is already occupied so a forged catalog command cannot overwrite
        // a bootstrap descriptor (e.g. SchemaId(1) = "public").
        let req = requested.get();
        if req != 0 && state.schemas_by_id.contains_key(&SchemaId::new(req)) {
            return SchemaId::new(state.next_schema_id);
        }
        allocate_id(&mut state.next_schema_id, req, SchemaId::new)
    }

    pub(crate) fn next_table_id(state: &mut CatalogState, requested: RelationId) -> RelationId {
        let req = requested.get();
        if req != 0 && state.tables_by_id.contains_key(&RelationId::new(req)) {
            return RelationId::new(state.next_table_id);
        }
        allocate_id(&mut state.next_table_id, req, RelationId::new)
    }

    pub(crate) fn next_index_id(state: &mut CatalogState, requested: IndexId) -> IndexId {
        let req = requested.get();
        if req != 0 && state.indexes_by_id.contains_key(&IndexId::new(req)) {
            return IndexId::new(state.next_index_id);
        }
        allocate_id(&mut state.next_index_id, req, IndexId::new)
    }

    pub(crate) fn next_sequence_id(state: &mut CatalogState, requested: SequenceId) -> SequenceId {
        let req = requested.get();
        if req != 0 && state.sequences_by_id.contains_key(&SequenceId::new(req)) {
            return SequenceId::new(state.next_sequence_id);
        }
        allocate_id(&mut state.next_sequence_id, req, SequenceId::new)
    }

    pub(crate) fn next_column_id(state: &mut CatalogState, requested: ColumnId) -> ColumnId {
        // Column ids live inside a per-table descriptor, not in a global
        // by-id map; collision reuse is allowed across tables, so keep the
        // pre-existing behaviour.
        allocate_id(&mut state.next_column_id, requested.get(), ColumnId::new)
    }

    pub(crate) fn next_tenant_id(state: &mut CatalogState, requested: TenantId) -> TenantId {
        let req = requested.get();
        if req != 0
            && state
                .tenants_by_name
                .values()
                .any(|t| t.tenant_id.get() == req)
        {
            return TenantId::new(state.next_tenant_id);
        }
        allocate_id(&mut state.next_tenant_id, req, TenantId::new)
    }

    pub(crate) fn default_sequence_state(sequence: &SequenceDescriptor) -> SequenceValueState {
        SequenceValueState {
            current_value: sequence.start_value,
            is_called: false,
        }
    }

    pub(crate) fn same_function_signature(
        left: &FunctionDescriptor,
        right: &FunctionDescriptor,
    ) -> bool {
        left.params.len() == right.params.len()
            && left.params.iter().zip(right.params.iter()).all(|(l, r)| {
                Self::function_param_signature_key(l) == Self::function_param_signature_key(r)
            })
    }

    fn function_param_signature_key(param: &aiondb_catalog::FunctionParamDescriptor) -> String {
        param.raw_type_name.as_deref().map_or_else(
            || param.data_type.to_string().to_ascii_lowercase(),
            str::to_ascii_lowercase,
        )
    }

    pub(crate) fn reserve_schema_id(&self, requested: SchemaId) -> DbResult<SchemaId> {
        let mut state = self.write_state()?;
        Ok(Self::next_schema_id(&mut state, requested))
    }

    pub(crate) fn reserve_table_id(&self, requested: RelationId) -> DbResult<RelationId> {
        let mut state = self.write_state()?;
        Ok(Self::next_table_id(&mut state, requested))
    }

    pub(crate) fn reserve_index_id(&self, requested: IndexId) -> DbResult<IndexId> {
        let mut state = self.write_state()?;
        Ok(Self::next_index_id(&mut state, requested))
    }

    pub(crate) fn reserve_sequence_id(&self, requested: SequenceId) -> DbResult<SequenceId> {
        let mut state = self.write_state()?;
        Ok(Self::next_sequence_id(&mut state, requested))
    }

    pub(crate) fn reserve_column_id(&self, requested: ColumnId) -> DbResult<ColumnId> {
        let mut state = self.write_state()?;
        Ok(Self::next_column_id(&mut state, requested))
    }
}

/// Starting id for synthetic transaction ids generated by the catalog WAL
/// for autocommit DDL. Sits well above the engine's monotonic txn-id counter
/// so a synthetic id can never collide with a real user transaction during
/// recovery replay.
const CATALOG_AUTO_TXN_BASE: u64 = 1 << 48;

fn remap_catalog_record_txn_id(
    record: &aiondb_wal::WalRecord,
    txn_id: TxnId,
) -> aiondb_wal::WalRecord {
    use aiondb_wal::WalRecord;

    match record {
        WalRecord::CatalogCreateSchema {
            descriptor_json, ..
        } => WalRecord::CatalogCreateSchema {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogDropSchema { schema_id_raw, .. } => WalRecord::CatalogDropSchema {
            txn_id,
            schema_id_raw: *schema_id_raw,
        },
        WalRecord::CatalogCreateRole {
            descriptor_json, ..
        } => WalRecord::CatalogCreateRole {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogAlterRole {
            descriptor_json, ..
        } => WalRecord::CatalogAlterRole {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogDropRole { role_name, .. } => WalRecord::CatalogDropRole {
            txn_id,
            role_name: role_name.clone(),
        },
        WalRecord::CatalogCreateView {
            descriptor_json, ..
        } => WalRecord::CatalogCreateView {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogDropView { view_id_raw, .. } => WalRecord::CatalogDropView {
            txn_id,
            view_id_raw: *view_id_raw,
        },
        WalRecord::CatalogCreateSequence {
            descriptor_json, ..
        } => WalRecord::CatalogCreateSequence {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogDropSequence {
            sequence_id_raw, ..
        } => WalRecord::CatalogDropSequence {
            txn_id,
            sequence_id_raw: *sequence_id_raw,
        },
        WalRecord::CatalogAlterSequence {
            descriptor_json, ..
        } => WalRecord::CatalogAlterSequence {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogCreateFunction {
            descriptor_json, ..
        } => WalRecord::CatalogCreateFunction {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogDropFunction { function_name, .. } => WalRecord::CatalogDropFunction {
            txn_id,
            function_name: function_name.clone(),
        },
        WalRecord::CatalogCreateTrigger {
            descriptor_json, ..
        } => WalRecord::CatalogCreateTrigger {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogDropTrigger {
            trigger_name,
            table_name,
            ..
        } => WalRecord::CatalogDropTrigger {
            txn_id,
            trigger_name: trigger_name.clone(),
            table_name: table_name.clone(),
        },
        WalRecord::CatalogGrantPrivilege {
            descriptor_json, ..
        } => WalRecord::CatalogGrantPrivilege {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogRevokePrivilege {
            descriptor_json, ..
        } => WalRecord::CatalogRevokePrivilege {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogSetTableDescriptor {
            descriptor_json, ..
        } => WalRecord::CatalogSetTableDescriptor {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogSetIndexDescriptor {
            descriptor_json, ..
        } => WalRecord::CatalogSetIndexDescriptor {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogCreateTenant {
            descriptor_json, ..
        } => WalRecord::CatalogCreateTenant {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogDropTenant { tenant_name, .. } => WalRecord::CatalogDropTenant {
            txn_id,
            tenant_name: tenant_name.clone(),
        },
        WalRecord::CatalogDropTable { table_id_raw, .. } => WalRecord::CatalogDropTable {
            txn_id,
            table_id_raw: *table_id_raw,
        },
        WalRecord::CatalogDropIndex { index_id_raw, .. } => WalRecord::CatalogDropIndex {
            txn_id,
            index_id_raw: *index_id_raw,
        },
        WalRecord::CatalogUpdateStatistics {
            descriptor_json, ..
        } => WalRecord::CatalogUpdateStatistics {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogCreateNodeLabel {
            descriptor_json, ..
        } => WalRecord::CatalogCreateNodeLabel {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogCreateEdgeLabel {
            descriptor_json, ..
        } => WalRecord::CatalogCreateEdgeLabel {
            txn_id,
            descriptor_json: descriptor_json.clone(),
        },
        WalRecord::CatalogDropNodeLabel { label_name, .. } => WalRecord::CatalogDropNodeLabel {
            txn_id,
            label_name: label_name.clone(),
        },
        WalRecord::CatalogDropEdgeLabel { label_name, .. } => WalRecord::CatalogDropEdgeLabel {
            txn_id,
            label_name: label_name.clone(),
        },
        WalRecord::CatalogSetSequenceValue {
            sequence_id_raw,
            current_value,
            is_called,
            ..
        } => WalRecord::CatalogSetSequenceValue {
            txn_id,
            sequence_id_raw: *sequence_id_raw,
            current_value: *current_value,
            is_called: *is_called,
        },
        other => other.clone(),
    }
}

fn allocate_id<T, F>(next: &mut u64, requested: u64, builder: F) -> T
where
    F: FnOnce(u64) -> T + Copy,
{
    if requested != 0 {
        if requested >= *next {
            *next = requested + 1;
        }
        builder(requested)
    } else {
        let value = *next;
        *next += 1;
        builder(value)
    }
}

pub(crate) fn unique_violation(message: impl Into<String>) -> DbError {
    DbError::constraint_error(SqlState::UniqueViolation, message)
}

pub(crate) fn duplicate_schema(name: &str) -> DbError {
    DbError::storage_error(
        SqlState::DuplicateSchema,
        format!("schema \"{name}\" already exists"),
    )
}

pub(crate) fn undefined_schema(name: &str) -> DbError {
    DbError::storage_error(
        SqlState::InvalidCatalogName,
        format!("schema \"{name}\" does not exist"),
    )
    .with_client_hint("create the schema before referencing it")
}

pub(crate) fn undefined_schema_id(schema_id: SchemaId) -> DbError {
    DbError::storage_error(
        SqlState::InvalidCatalogName,
        format!("schema id {} does not exist", schema_id.get()),
    )
}

pub(crate) fn undefined_table_id(table_id: RelationId) -> DbError {
    DbError::bind_error(
        SqlState::UndefinedTable,
        format!("table id {} does not exist", table_id.get()),
    )
}

pub(crate) fn undefined_index_id(index_id: IndexId) -> DbError {
    DbError::bind_error(
        SqlState::UndefinedObject,
        format!("index id {} does not exist", index_id.get()),
    )
}

pub(crate) fn undefined_sequence_id(sequence_id: SequenceId) -> DbError {
    DbError::bind_error(
        SqlState::UndefinedObject,
        format!("sequence id {} does not exist", sequence_id.get()),
    )
}

pub(crate) fn undefined_tenant(name: &str) -> DbError {
    DbError::bind_error(
        SqlState::UndefinedObject,
        format!("tenant \"{name}\" does not exist"),
    )
}

pub(crate) fn undefined_role(name: &str) -> DbError {
    DbError::bind_error(
        SqlState::UndefinedObject,
        format!("role \"{name}\" does not exist"),
    )
}

pub(crate) fn invalid_sequence_ownership() -> DbError {
    DbError::bind_error(
        SqlState::UndefinedObject,
        "sequence ownership must set both table_id and column_id or neither",
    )
}

pub(crate) fn sequence_exhausted(sequence_name: &QualifiedName) -> DbError {
    DbError::constraint_error(
        SqlState::ProgramLimitExceeded,
        format!("sequence {sequence_name} has reached its limit"),
    )
}

pub(crate) fn serialization_failure(message: impl Into<String>) -> DbError {
    DbError::transaction_error(SqlState::SerializationFailure, message)
        .with_client_hint("retry the transaction")
}

/// Serde helper: serialize/deserialize `BTreeMap<(K, String), V>` as a JSON
/// array of `[key, name, value]` triples, since JSON does not allow
/// non-string map keys.
mod tuple_key_map {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<K, V, S>(
        map: &BTreeMap<(K, String), V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        K: Serialize + Copy,
        V: Serialize,
        S: Serializer,
    {
        let tuples: Vec<(&K, &str, &V)> =
            map.iter().map(|((k, n), v)| (k, n.as_str(), v)).collect();
        tuples.serialize(serializer)
    }

    pub fn deserialize<'de, K, V, D>(deserializer: D) -> Result<BTreeMap<(K, String), V>, D::Error>
    where
        K: Deserialize<'de> + Ord + Copy,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        let tuples: Vec<(K, String, V)> = Vec::deserialize(deserializer)?;
        Ok(tuples.into_iter().map(|(k, n, v)| ((k, n), v)).collect())
    }
}

mod string_pair_key_map {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<V, S>(
        map: &BTreeMap<(String, String), V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        V: Serialize,
        S: Serializer,
    {
        let tuples: Vec<(&str, &str, &V)> = map
            .iter()
            .map(|((left, right), value)| (left.as_str(), right.as_str(), value))
            .collect();
        tuples.serialize(serializer)
    }

    pub fn deserialize<'de, V, D>(
        deserializer: D,
    ) -> Result<BTreeMap<(String, String), V>, D::Error>
    where
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        let tuples: Vec<(String, String, V)> = Vec::deserialize(deserializer)?;
        Ok(tuples
            .into_iter()
            .map(|(left, right, value)| ((left, right), value))
            .collect())
    }
}

#[cfg(test)]
mod tests;
