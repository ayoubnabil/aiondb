//! Row-level lock table for the storage engine.
//!
//! This module provides the per-row and per-table lock infrastructure that
//! enables concurrent DML operations in the storage engine. It complements
//! the session-level `WaitGraphLockManager` (in `aiondb-tx`) by adding a
//! storage-internal lock table that coordinates access to committed state
//! during split-phase DML operations.
//!
//! # Split-Phase DML
//!
//! Traditional DML in this storage engine acquires the global `RwLock` in
//! write mode for the entire operation. Split-phase DML reduces contention:
//!
//! 1. **Read phase** (global read lock): Validate preconditions, collect
//!    descriptors, determine tuple IDs, check memory pressure.
//! 2. **Row lock phase** (no global lock): Acquire storage-level row locks
//!    to prevent concurrent modifications to the same tuples.
//! 3. **Write phase** (global write lock): Apply the mutation to pending
//!    transaction state. This phase is very short because all validation
//!    was done in the read phase.
//!
//! The row lock table prevents lost updates and write-write conflicts at
//! the storage layer. The session-level `WaitGraphLockManager` handles
//! deadlock detection and cross-session conflict resolution.
//!
//! # Lock Modes
//!
//! The storage-level row lock table uses two modes:
//!
//! - **Shared**: Held by readers during snapshot-isolated reads. Multiple
//!   transactions can hold shared locks on the same row.
//! - **Exclusive**: Held during UPDATE/DELETE. Only one transaction can hold
//!   an exclusive lock on a row. Exclusive locks block shared locks and
//!   vice versa.
//!
//! Table-level intent locks follow the standard IS/IX/S/X hierarchy.

use std::collections::{BTreeMap, BTreeSet};

use parking_lot::Mutex;

use aiondb_core::{DbError, DbResult, IndexId, RelationId, TupleId, TxnId};

/// Lock mode for storage-level row locks.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum RowLockMode {
    /// Shared lock: allows concurrent reads but blocks exclusive locks.
    Shared,
    /// Exclusive lock: blocks all other locks on the same row.
    Exclusive,
}

/// Intent lock mode for table-level locks that signal the type of row-level
/// locks that will be acquired within the table.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum IntentLockMode {
    /// Intent Shared: signals that shared row locks will be acquired.
    IntentShared,
    /// Intent Exclusive: signals that exclusive row locks will be acquired.
    IntentExclusive,
    /// Shared: full table-level shared lock (e.g., for table scans in
    /// serializable isolation).
    Shared,
    /// Exclusive: full table-level exclusive lock (e.g., for DDL operations
    /// like TRUNCATE, ALTER TABLE).
    Exclusive,
}

impl IntentLockMode {
    /// Check if two intent lock modes are compatible.
    ///
    /// Compatibility matrix:
    /// ```text
    ///                 IS     IX     S      X
    ///     IS          Y      Y      Y      N
    ///     IX          Y      Y      N      N
    ///     S           Y      N      Y      N
    ///     X           N      N      N      N
    /// ```
    pub fn compatible(self, other: Self) -> bool {
        use IntentLockMode::{IntentExclusive, IntentShared, Shared};
        matches!(
            (self, other),
            (IntentShared | IntentExclusive | Shared, IntentShared)
                | (IntentShared | IntentExclusive, IntentExclusive)
                | (IntentShared | Shared, Shared)
        )
    }
}

/// A single lock grant on a specific row or table.
#[derive(Clone, Debug)]
struct RowLockGrant {
    txn: TxnId,
    mode: RowLockMode,
}

/// A single intent lock grant on a table.
#[derive(Clone, Debug)]
struct IntentLockGrant {
    txn: TxnId,
    mode: IntentLockMode,
}

/// The lock target: either a specific row or a table-level intent lock.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[allow(dead_code)]
enum LockTarget {
    Table(RelationId),
    Row {
        table_id: RelationId,
        tuple_id: TupleId,
    },
}

/// Internal state of the row lock table.
#[derive(Debug, Default)]
#[allow(clippy::struct_field_names)]
struct LockTableState {
    /// Intent locks on tables.
    table_locks: BTreeMap<RelationId, Vec<IntentLockGrant>>,
    /// Row-level locks keyed by (`table_id`, `tuple_id`).
    row_locks: BTreeMap<(RelationId, TupleId), Vec<RowLockGrant>>,
    /// Reverse index: all locks held by each transaction, for efficient
    /// release on commit/rollback.
    txn_table_locks: BTreeMap<TxnId, BTreeSet<RelationId>>,
    txn_row_locks: BTreeMap<TxnId, BTreeSet<(RelationId, TupleId)>>,
}

impl LockTableState {
    /// Check if a table-level intent lock can be granted.
    fn can_grant_table_lock(&self, txn: TxnId, table_id: RelationId, mode: IntentLockMode) -> bool {
        let Some(grants) = self.table_locks.get(&table_id) else {
            return true;
        };
        grants
            .iter()
            .all(|grant| grant.txn == txn || grant.mode.compatible(mode))
    }

    /// Check if a row-level lock can be granted.
    fn can_grant_row_lock(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: RowLockMode,
    ) -> bool {
        let Some(grants) = self.row_locks.get(&(table_id, tuple_id)) else {
            return true;
        };
        grants.iter().all(|grant| {
            grant.txn == txn
                || matches!(
                    (grant.mode, mode),
                    (RowLockMode::Shared, RowLockMode::Shared)
                )
        })
    }

    /// Grant a table-level intent lock. Upgrades existing locks if the
    /// transaction already holds a weaker lock.
    fn grant_table_lock(&mut self, txn: TxnId, table_id: RelationId, mode: IntentLockMode) {
        let grants = self.table_locks.entry(table_id).or_default();
        if let Some(existing) = grants.iter_mut().find(|g| g.txn == txn) {
            // Upgrade: keep the stronger mode.
            if mode > existing.mode {
                existing.mode = mode;
            }
        } else {
            grants.push(IntentLockGrant { txn, mode });
        }
        self.txn_table_locks
            .entry(txn)
            .or_default()
            .insert(table_id);
    }

    /// Grant a row-level lock. Upgrades existing locks if the transaction
    /// already holds a weaker lock.
    fn grant_row_lock(
        &mut self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: RowLockMode,
    ) {
        let key = (table_id, tuple_id);
        let grants = self.row_locks.entry(key).or_default();
        if let Some(existing) = grants.iter_mut().find(|g| g.txn == txn) {
            if mode > existing.mode {
                existing.mode = mode;
            }
        } else {
            grants.push(RowLockGrant { txn, mode });
        }
        self.txn_row_locks.entry(txn).or_default().insert(key);
    }

    /// Release all locks held by a transaction.
    fn release_txn(&mut self, txn: TxnId) {
        if let Some(tables) = self.txn_table_locks.remove(&txn) {
            for table_id in tables {
                if let Some(grants) = self.table_locks.get_mut(&table_id) {
                    grants.retain(|g| g.txn != txn);
                    if grants.is_empty() {
                        self.table_locks.remove(&table_id);
                    }
                }
            }
        }

        if let Some(rows) = self.txn_row_locks.remove(&txn) {
            for key in rows {
                if let Some(grants) = self.row_locks.get_mut(&key) {
                    grants.retain(|g| g.txn != txn);
                    if grants.is_empty() {
                        self.row_locks.remove(&key);
                    }
                }
            }
        }
    }

    /// Return the set of transactions that would block the given table lock.
    fn table_lock_blockers(
        &self,
        txn: TxnId,
        table_id: RelationId,
        mode: IntentLockMode,
    ) -> BTreeSet<TxnId> {
        let Some(grants) = self.table_locks.get(&table_id) else {
            return BTreeSet::new();
        };
        grants
            .iter()
            .filter(|g| g.txn != txn && !g.mode.compatible(mode))
            .map(|g| g.txn)
            .collect()
    }

    /// Return the set of transactions that would block the given row lock.
    fn row_lock_blockers(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: RowLockMode,
    ) -> BTreeSet<TxnId> {
        let Some(grants) = self.row_locks.get(&(table_id, tuple_id)) else {
            return BTreeSet::new();
        };
        grants
            .iter()
            .filter(|g| {
                g.txn != txn
                    && !matches!((g.mode, mode), (RowLockMode::Shared, RowLockMode::Shared))
            })
            .map(|g| g.txn)
            .collect()
    }
}

/// Storage-internal row lock table for coordinating concurrent DML.
///
/// This is distinct from the session-level `WaitGraphLockManager` in
/// `aiondb-tx`. The session-level manager provides cross-session deadlock
/// detection and SQL-visible lock semantics (`FOR UPDATE`, `FOR SHARE`).
/// This lock table provides the storage-engine-internal coordination needed
/// for split-phase DML under the global `RwLock`.
#[derive(Debug)]
pub struct RowLockTable {
    state: Mutex<LockTableState>,
}

impl Default for RowLockTable {
    fn default() -> Self {
        Self {
            state: Mutex::new(LockTableState::default()),
        }
    }
}

impl RowLockTable {
    /// Create a new, empty row lock table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to acquire a table-level intent lock. Returns `Ok(true)` if
    /// granted immediately, `Ok(false)` if blocked, or `Err` on internal
    /// error.
    pub fn try_acquire_table_lock(
        &self,
        txn: TxnId,
        table_id: RelationId,
        mode: IntentLockMode,
    ) -> DbResult<bool> {
        let mut state = self.state.lock();
        if state.can_grant_table_lock(txn, table_id, mode) {
            state.grant_table_lock(txn, table_id, mode);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Acquire a table-level intent lock, returning an error with the set of
    /// blocking transactions if the lock cannot be granted immediately.
    pub fn acquire_table_lock(
        &self,
        txn: TxnId,
        table_id: RelationId,
        mode: IntentLockMode,
    ) -> DbResult<()> {
        let mut state = self.state.lock();
        if state.can_grant_table_lock(txn, table_id, mode) {
            state.grant_table_lock(txn, table_id, mode);
            Ok(())
        } else {
            let blockers = state.table_lock_blockers(txn, table_id, mode);
            Err(DbError::internal(format!(
                "table lock conflict on {table_id:?}: blocked by transactions {blockers:?}"
            )))
        }
    }

    /// Try to acquire a row-level lock. Returns `Ok(true)` if granted
    /// immediately, `Ok(false)` if blocked.
    pub fn try_acquire_row_lock(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: RowLockMode,
    ) -> DbResult<bool> {
        let mut state = self.state.lock();
        if state.can_grant_row_lock(txn, table_id, tuple_id, mode) {
            state.grant_row_lock(txn, table_id, tuple_id, mode);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Acquire a row-level lock, returning an error if blocked.
    pub fn acquire_row_lock(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: RowLockMode,
    ) -> DbResult<()> {
        let mut state = self.state.lock();
        if state.can_grant_row_lock(txn, table_id, tuple_id, mode) {
            state.grant_row_lock(txn, table_id, tuple_id, mode);
            Ok(())
        } else {
            let blockers = state.row_lock_blockers(txn, table_id, tuple_id, mode);
            Err(DbError::internal(format!(
                "row lock conflict on {table_id:?}/{tuple_id:?}: blocked by transactions {blockers:?}"
            )))
        }
    }

    /// Release all locks held by a transaction. Called on commit or rollback.
    pub fn release_txn(&self, txn: TxnId) -> DbResult<()> {
        let mut state = self.state.lock();
        state.release_txn(txn);
        Ok(())
    }

    /// Return the set of transactions blocking a row lock request.
    pub fn row_lock_blockers(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: RowLockMode,
    ) -> DbResult<BTreeSet<TxnId>> {
        let state = self.state.lock();
        Ok(state.row_lock_blockers(txn, table_id, tuple_id, mode))
    }

    /// Return the number of table-level locks currently held.
    pub fn table_lock_count(&self) -> DbResult<usize> {
        let state = self.state.lock();
        Ok(state.table_locks.values().map(|grants| grants.len()).sum())
    }

    /// Return the number of row-level locks currently held.
    pub fn row_lock_count(&self) -> DbResult<usize> {
        let state = self.state.lock();
        Ok(state.row_locks.values().map(|grants| grants.len()).sum())
    }
}

/// Pre-validated DML parameters collected during the read phase of a
/// split-phase DML operation.
///
/// After collecting these parameters under a read lock, the caller releases
/// the read lock, acquires storage-level row locks, then re-acquires the
/// write lock to apply the mutation. The `generation` field detects if the
/// committed state changed between the read and write phases.
#[derive(Clone, Debug)]
pub struct DmlPrecheck {
    /// The table this operation targets.
    pub table_id: RelationId,
    /// The tuple ID for the operation (assigned during read phase for INSERT,
    /// known for UPDATE/DELETE).
    pub tuple_id: TupleId,
    /// The validated table descriptor snapshot.
    pub descriptor: super::TableStorageDescriptor,
    /// For updates: the current row value before modification.
    pub old_row: Option<super::Row>,
    /// Pre-computed index maintenance plan for split-phase UPDATE, allowing
    /// commit/apply to reuse the already-classified indexed columns.
    pub split_phase_index_update_set: Option<super::index_ops::IndexUpdateSet>,
    /// Precomputed whether any pending (in-transaction-created) index is
    /// affected by the planned split-phase update.
    pub pending_indexed_columns_changed: bool,
    /// Precomputed pending B-tree indexes touched by split-phase UPDATE.
    pub split_phase_pending_btree_index_ids: Vec<IndexId>,
    /// Precomputed pending HNSW indexes touched by split-phase UPDATE.
    pub split_phase_pending_hnsw_index_ids: Vec<IndexId>,
    /// Precomputed pending GIN indexes touched by split-phase UPDATE.
    pub split_phase_pending_gin_index_ids: Vec<IndexId>,
    /// Pre-computed committed HNSW index IDs for this split-phase target table.
    /// Stored as `Some` when split-phase validation observed HNSW indexes;
    /// when known to be absent, `None` avoids repeated reads at apply time.
    pub split_phase_hnsw_index_ids: Option<Vec<IndexId>>,
    /// The base table's next heap position at read-phase time.
    pub base_next_heap_position: u64,
}
