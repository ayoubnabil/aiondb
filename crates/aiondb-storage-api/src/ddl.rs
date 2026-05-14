#![allow(clippy::missing_errors_doc)]

use aiondb_core::{DbResult, IndexId, RelationId, TxnId};

use crate::{IndexStorageDescriptor, TableStorageDescriptor};

#[allow(clippy::missing_errors_doc)]
pub trait StorageDDL: Send + Sync {
    fn create_table_storage(&self, txn: TxnId, table: &TableStorageDescriptor) -> DbResult<()>;
    fn create_index_storage(&self, txn: TxnId, index: &IndexStorageDescriptor) -> DbResult<()>;
    fn alter_table_storage(&self, txn: TxnId, table: &TableStorageDescriptor) -> DbResult<()>;
    fn drop_table_storage(&self, txn: TxnId, table_id: RelationId) -> DbResult<()>;
    fn drop_index_storage(&self, txn: TxnId, index_id: IndexId) -> DbResult<()>;
}
