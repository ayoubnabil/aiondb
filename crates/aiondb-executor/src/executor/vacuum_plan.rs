use super::*;

impl Executor {
    /// Execute a VACUUM command: remove dead tuple versions from the table.
    pub(super) fn execute_vacuum(
        &self,
        table_id: RelationId,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        context.check_deadline()?;
        // Bare `VACUUM` (no table) lowers to RelationId(0). PG treats that
        // form as whole-database vacuum; AionDB has no global vacuum entry
        // point yet, so report success and move on instead of refusing.
        if table_id.get() == 0 {
            return Ok(ExecutionResult::Command {
                tag: "VACUUM".to_owned(),
                rows_affected: 0,
            });
        }
        self.lock_table(context, table_id, LockMode::AccessExclusive)?;
        let dead_removed = self.storage_dml.vacuum_table(table_id)?;
        Ok(ExecutionResult::Command {
            tag: "VACUUM".to_owned(),
            rows_affected: dead_removed,
        })
    }

    /// Execute a CHECKPOINT command: write a checkpoint record to the WAL,
    /// flush the buffer pool, and prune obsolete WAL segments.
    pub(super) fn execute_checkpoint(
        &self,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        context.check_deadline()?;
        let _info = self.storage_txn.checkpoint()?;
        Ok(ExecutionResult::Command {
            tag: "CHECKPOINT".to_owned(),
            rows_affected: 0,
        })
    }

    /// Execute a `LOCK TABLE ... IN lockmode MODE [NOWAIT]` command.
    /// Acquires the chosen mode on each listed table for the active txn;
    /// held until COMMIT/ROLLBACK like any other txn-scoped lock.
    pub(super) fn execute_lock(
        &self,
        table_ids: &[RelationId],
        mode: aiondb_plan::PgLockMode,
        nowait: bool,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        context.check_deadline()?;
        let engine_mode = map_pg_lock_mode_to_engine(mode);
        for table_id in table_ids {
            context.acquire_table_lock_with_nowait(*table_id, engine_mode, nowait)?;
        }
        Ok(ExecutionResult::Command {
            tag: "LOCK TABLE".to_owned(),
            rows_affected: 0,
        })
    }
}

fn map_pg_lock_mode_to_engine(mode: aiondb_plan::PgLockMode) -> LockMode {
    match mode {
        aiondb_plan::PgLockMode::AccessShare => LockMode::AccessShare,
        aiondb_plan::PgLockMode::RowShare => LockMode::KeyShare,
        aiondb_plan::PgLockMode::RowExclusive => LockMode::RowExclusive,
        aiondb_plan::PgLockMode::ShareUpdateExclusive => LockMode::Update,
        aiondb_plan::PgLockMode::Share => LockMode::PredicateRead,
        aiondb_plan::PgLockMode::ShareRowExclusive => LockMode::Update,
        aiondb_plan::PgLockMode::Exclusive => LockMode::AccessExclusive,
        aiondb_plan::PgLockMode::AccessExclusive => LockMode::AccessExclusive,
    }
}
