#![allow(clippy::missing_errors_doc)]

use aiondb_core::{DbError, DbResult, IndexId, RelationId, TxnId};

use crate::{IndexStorageDescriptor, TableStorageDescriptor};

#[allow(clippy::missing_errors_doc)]
pub trait StorageDDL: Send + Sync {
    fn create_table_storage(&self, txn: TxnId, table: &TableStorageDescriptor) -> DbResult<()>;
    fn create_index_storage(&self, txn: TxnId, index: &IndexStorageDescriptor) -> DbResult<()>;
    fn alter_table_storage(&self, txn: TxnId, table: &TableStorageDescriptor) -> DbResult<()>;
    fn drop_table_storage(&self, txn: TxnId, table_id: RelationId) -> DbResult<()>;
    fn drop_index_storage(&self, txn: TxnId, index_id: IndexId) -> DbResult<()>;

    /// Rebuild a vector index in place from the table's currently visible
    /// rows. Storage-API equivalent of `REINDEX INDEX <name>` for HNSW /
    /// vector indexes.
    ///
    /// # Optional capability
    ///
    /// The default implementation returns
    /// [`DbError::feature_not_supported`]. Backends that maintain vector
    /// indexes override this to retrain SQ / PQ codebooks against the
    /// current data distribution.
    fn reindex_vector_index_storage(&self, _txn: TxnId, _index_id: IndexId) -> DbResult<()> {
        Err(DbError::feature_not_supported(
            "REINDEX VECTOR is not supported by this storage backend",
        ))
    }
}
