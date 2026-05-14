use aiondb_core::{DbResult, RelationId, TupleId, TxnId};

use crate::ActiveTransaction;

#[allow(clippy::missing_errors_doc)]
pub trait SerializableCoordinator: Send + Sync {
    fn record_relation_read(&self, txn: TxnId, relation_id: RelationId) -> DbResult<()>;
    fn record_relation_write(&self, txn: TxnId, relation_id: RelationId) -> DbResult<()>;
    fn record_tuple_write(
        &self,
        _txn: TxnId,
        _relation_id: RelationId,
        _tuple_id: TupleId,
    ) -> DbResult<()> {
        Ok(())
    }
    fn validate_commit(&self, txn: &ActiveTransaction) -> DbResult<()>;
    fn finish_commit(&self, txn: TxnId, commit_ts: u64) -> DbResult<()>;
    fn rollback_txn(&self, txn: TxnId) -> DbResult<()>;
}

#[derive(Debug, Default)]
pub struct NoopSerializableCoordinator;

impl SerializableCoordinator for NoopSerializableCoordinator {
    fn record_relation_read(&self, _txn: TxnId, _relation_id: RelationId) -> DbResult<()> {
        Ok(())
    }

    fn record_relation_write(&self, _txn: TxnId, _relation_id: RelationId) -> DbResult<()> {
        Ok(())
    }

    fn validate_commit(&self, _txn: &ActiveTransaction) -> DbResult<()> {
        Ok(())
    }

    fn finish_commit(&self, _txn: TxnId, _commit_ts: u64) -> DbResult<()> {
        Ok(())
    }

    fn rollback_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }
}
