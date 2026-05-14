#![allow(clippy::missing_errors_doc)]

use aiondb_core::{DbError, DbResult, TxnId};
use aiondb_tx::IsolationLevel;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointInfo {
    pub checkpoint_lsn: u64,
    pub dirty_pages_flushed: u64,
}

/// Storage layer interface for transaction lifecycle (begin, commit,
/// rollback, checkpoint) and optional savepoint management.
///
/// # Required vs optional methods
///
/// The core transaction methods ([`begin_txn`](Self::begin_txn),
/// [`commit_txn`](Self::commit_txn), [`rollback_txn`](Self::rollback_txn),
/// [`checkpoint`](Self::checkpoint)) are **required**.
///
/// The savepoint methods are **optional** and default to returning
/// [`DbError::feature_not_supported`].  A backend that supports
/// savepoints should override all three and return `true` from
/// [`StorageCapabilities::supports_savepoints`](crate::StorageCapabilities::supports_savepoints).
#[allow(clippy::missing_errors_doc)]
pub trait StorageTxnParticipant: Send + Sync {
    // ─── Required methods ───────────────────────────────────────

    /// Begin a new transaction. **Required.**
    ///
    /// The `isolation` parameter specifies the isolation level that will be
    /// recorded in the WAL so that recovery and replication can faithfully
    /// replay transaction boundaries.
    fn begin_txn(&self, txn: TxnId, isolation: IsolationLevel) -> DbResult<()>;

    /// Validate that the transaction can commit without publishing changes.
    ///
    /// This hook is used by the engine to fail fast before a coordinated
    /// multi-subsystem commit starts to publish durable state.
    fn validate_commit_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }

    /// Commit a transaction at the given timestamp. **Required.**
    fn commit_txn(&self, txn: TxnId, commit_ts: u64) -> DbResult<()>;

    /// Rollback (abort) a transaction. **Required.**
    fn rollback_txn(&self, txn: TxnId) -> DbResult<()>;

    /// Perform a checkpoint. **Required.**
    fn checkpoint(&self) -> DbResult<CheckpointInfo>;

    // ─── Optional methods (have default impls) ──────────────────

    /// Create a savepoint for the given transaction, returning an opaque id.
    ///
    /// The returned id is later passed to
    /// [`rollback_to_savepoint`](Self::rollback_to_savepoint) or
    /// [`release_savepoint`](Self::release_savepoint).
    ///
    /// # Optional capability
    ///
    /// The default implementation returns
    /// [`DbError::feature_not_supported`].  Backends that support
    /// savepoints should override this method and return `true` from
    /// [`StorageCapabilities::supports_savepoints`](crate::StorageCapabilities::supports_savepoints).
    fn create_savepoint(&self, _txn: TxnId) -> DbResult<u64> {
        Err(DbError::feature_not_supported(
            "savepoints are not supported by this storage engine",
        ))
    }

    /// Rollback the transaction to the given savepoint.
    ///
    /// All mutations performed after the savepoint was created are undone.
    /// The savepoint itself remains valid and may be rolled back to again.
    ///
    /// # Optional capability
    ///
    /// The default implementation returns
    /// [`DbError::feature_not_supported`].  See
    /// [`create_savepoint`](Self::create_savepoint) and
    /// [`StorageCapabilities::supports_savepoints`](crate::StorageCapabilities::supports_savepoints).
    fn rollback_to_savepoint(&self, _txn: TxnId, _savepoint_id: u64) -> DbResult<()> {
        Err(DbError::feature_not_supported(
            "savepoints are not supported by this storage engine",
        ))
    }

    /// Release the given savepoint.
    ///
    /// After release, the savepoint id is no longer valid and any attempt to
    /// roll back to it will fail.
    ///
    /// # Optional capability
    ///
    /// The default implementation returns
    /// [`DbError::feature_not_supported`].  See
    /// [`create_savepoint`](Self::create_savepoint) and
    /// [`StorageCapabilities::supports_savepoints`](crate::StorageCapabilities::supports_savepoints).
    fn release_savepoint(&self, _txn: TxnId, _savepoint_id: u64) -> DbResult<()> {
        Err(DbError::feature_not_supported(
            "savepoints are not supported by this storage engine",
        ))
    }
}
