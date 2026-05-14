#![allow(clippy::doc_markdown, clippy::wildcard_imports)]

use super::*;
use aiondb_core::SqlState;
use tracing::warn;

/// Number of additional attempts after the initial try when an autocommit
/// statement fails with `40001 SerializationFailure`. Total attempts =
/// limit + 1.
///
/// Why **8**: the previous default was 2 (3 attempts), which is enough for
/// light contention but bench traces of `UPDATE posts SET likes = likes + 1
/// WHERE id = $1` at 8 concurrent clients still produced occasional
/// "tuple ... changed concurrently before write" surface to the client.
/// reread+reapply), which AionDB does not yet implement; bumping the retry
/// budget gives us a publishable behavior at OLTP-typical contention while
/// EvalPlanQual is in flight. Each retry is a fresh snapshot + plan
/// execution, not a tight spin, so a small increase here adds at most a
/// handful of milliseconds in the worst case.
///
/// Override at runtime with `AIONDB_IMPLICIT_TXN_SERIALIZATION_RETRY_LIMIT`
/// (parsed once per call). Setting to `0` restores the original "retry
/// once" behavior; values >64 are clamped.
const IMPLICIT_TXN_SERIALIZATION_RETRY_LIMIT_DEFAULT: usize = 8;
const IMPLICIT_TXN_SERIALIZATION_RETRY_LIMIT_MAX: usize = 64;

fn implicit_txn_serialization_retry_limit() -> usize {
    match std::env::var("AIONDB_IMPLICIT_TXN_SERIALIZATION_RETRY_LIMIT") {
        Ok(value) => value
            .parse::<usize>()
            .map(|n| n.min(IMPLICIT_TXN_SERIALIZATION_RETRY_LIMIT_MAX))
            .unwrap_or(IMPLICIT_TXN_SERIALIZATION_RETRY_LIMIT_DEFAULT),
        Err(_) => IMPLICIT_TXN_SERIALIZATION_RETRY_LIMIT_DEFAULT,
    }
}

/// Merge a primary action result with a cleanup result, logging on failure.
/// On dual failure the cleanup error is appended as internal detail to the
/// primary error.
fn merge_with_cleanup(
    primary: DbResult<()>,
    cleanup: DbResult<()>,
    action_label: &str,
) -> DbResult<()> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(e), Ok(())) => Err(e),
        (Ok(()), Err(cleanup_err)) => {
            warn!(
                error = %cleanup_err,
                "failed to clear implicit transaction marker after {action_label}"
            );
            Err(cleanup_err)
        }
        (Err(primary_err), Err(cleanup_err)) => {
            warn!(
                primary_error = %primary_err,
                cleanup_error = %cleanup_err,
                "{action_label} failed and implicit transaction cleanup also failed"
            );
            Err(super::support::with_appended_internal_detail(
                primary_err,
                format!(
                    "implicit transaction cleanup failed after {action_label} error: {cleanup_err}"
                ),
            ))
        }
    }
}

impl Engine {
    fn clear_implicit_txn_marker(&self, session: &SessionHandle) -> DbResult<()> {
        self.with_session_mut(session, |record| {
            record.implicit_txn_active = false;
            Ok(())
        })
    }

    pub(super) fn execute_with_implicit_transaction<T>(
        &self,
        session: &SessionHandle,
        apply: impl FnMut() -> DbResult<T>,
    ) -> DbResult<T> {
        self.execute_with_implicit_transaction_options(session, true, true, apply)
    }

    pub(super) fn execute_with_implicit_transaction_options<T>(
        &self,
        session: &SessionHandle,
        include_catalog_participant: bool,
        include_storage_participant: bool,
        mut apply: impl FnMut() -> DbResult<T>,
    ) -> DbResult<T> {
        let has_active_txn =
            self.with_session(session, |record| Ok(record.active_txn.is_some()))?;
        if has_active_txn {
            // Explicit transactions enroll catalog/storage participants lazily.
            // Portal execution routes through this helper too, so when a
            // transaction is already active we must still ensure required
            // participants are present before executing the statement.
            self.ensure_active_transaction_participants(
                session,
                include_catalog_participant,
                include_storage_participant,
            )?;
            return apply();
        }

        // Autocommit statements still need a real TxnId so MVCC visibility does
        // not collapse to "latest version wins" for concurrent writers.
        let isolation = self.with_session(session, |record| {
            Ok(self::session_vars::default_transaction_isolation_for_record(record))
        })?;

        if !include_catalog_participant
            && !include_storage_participant
            && isolation == aiondb_tx::IsolationLevel::ReadCommitted
        {
            return apply();
        }

        let retry_limit = implicit_txn_serialization_retry_limit();
        for attempt in 0..=retry_limit {
            self.begin_transaction_internal_with_options(
                session,
                isolation,
                include_catalog_participant,
                include_storage_participant,
                false,
            )?;
            self.with_session_mut(session, |record| {
                record.implicit_txn_active = true;
                Ok(())
            })?;
            match apply() {
                Ok(result) => {
                    let commit_result = self.commit_transaction(session);
                    let cleanup_result = self.clear_implicit_txn_marker(session);
                    merge_with_cleanup(commit_result, cleanup_result, "commit")?;
                    return Ok(result);
                }
                Err(statement_error) => {
                    let rollback_result = self.rollback_transaction(session);
                    let cleanup_result = self.clear_implicit_txn_marker(session);
                    // Merge rollback + cleanup failures; if either failed, append
                    // the original statement error as context.
                    if let Err(txn_error) =
                        merge_with_cleanup(rollback_result, cleanup_result, "rollback")
                    {
                        return Err(super::support::with_appended_internal_detail(
                            txn_error,
                            format!(
                                "original implicit transaction statement error: {statement_error}"
                            ),
                        ));
                    }
                    if statement_error.sqlstate() == SqlState::SerializationFailure
                        && attempt < retry_limit
                    {
                        continue;
                    }
                    return Err(statement_error);
                }
            }
        }

        Err(DbError::internal(
            "implicit transaction retry loop exhausted unexpectedly",
        ))
    }
}
