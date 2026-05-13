use aiondb_core::{DbError, DbResult, SqlState};

use super::support::command_ok;
use super::Engine;
use crate::{prepared::StatementResult, session::SessionHandle};

impl Engine {
    /// Execute a `CHECKPOINT` statement.
    ///
    /// Forces a WAL checkpoint via [`StorageTxnParticipant::checkpoint`],
    /// which writes a checkpoint record to the WAL, persists a base snapshot,
    /// and prunes obsolete WAL segments. Requires superuser when the catalog
    /// block (PostgreSQL-compatible semantics).
    pub(super) fn execute_checkpoint(&self, session: &SessionHandle) -> DbResult<StatementResult> {
        let session_info = self.session_info(session)?;
        if crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?
            && !crate::catalog_authorizer::is_superuser_checked(
                self.catalog_reader.as_ref(),
                &session_info.identity,
            )?
        {
            return Err(DbError::insufficient_privilege(
                "must be superuser to CHECKPOINT",
            ));
        }

        let inside_explicit_txn = self.with_session(session, |record| {
            Ok(record.active_txn.is_some() && !record.implicit_txn_active)
        })?;
        if inside_explicit_txn {
            return Err(DbError::transaction_error(
                SqlState::ObjectNotInPrerequisiteState,
                "CHECKPOINT cannot run inside a transaction block",
            ));
        }

        self.storage_txn.checkpoint()?;
        Ok(command_ok("CHECKPOINT"))
    }
}
