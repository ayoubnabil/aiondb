use aiondb_core::{DbError, DbResult};
use aiondb_executor::ExecutionResult;
use aiondb_parser::TransactionMode;
use aiondb_tx::IsolationLevel;
use getrandom::fill as fill_random;

use super::Engine;
use crate::{
    prepared::{ResultColumn, StatementResult},
    session::SessionHandle,
};

mod acl_audit;
mod errors;
mod explain;

pub(crate) use acl_audit::is_graph_ddl_statement;
pub(super) use errors::{
    mark_commit_outcome_ambiguous, merge_with_lock_release_error, preserves_outer_transaction,
    with_appended_internal_detail,
};
pub(super) use explain::{collect_table_ids, explain_result_rows_pg, ExplainAnalyzeSummary};

impl Engine {
    /// Return a point-in-time snapshot of engine-level query metrics.
    pub fn query_metrics(&self) -> super::metrics::EngineMetricsSnapshot {
        self.metrics.snapshot()
    }
}

pub(super) fn next_session_handle() -> DbResult<SessionHandle> {
    Ok(SessionHandle::from_token(random_session_token()?))
}

/// Convert a `u64` to `f64` without using `f64::from_bits`/`as` casts.
///
/// Splits the `u64` into two `u32` halves so the conversion only touches the
/// `f64::from(u32)` path. Used by metrics/explain/timing code to avoid
/// `clippy::cast_precision_loss` warnings while still tolerating values
/// above `2^53` (callers downstream are already loss-tolerant: histogram
/// quantiles, plan-row estimates, notification queue depths).
pub(super) fn u64_to_f64(value: u64) -> f64 {
    let upper = u32::try_from(value >> 32).unwrap_or(u32::MAX);
    let lower = u32::try_from(value & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(upper) * 4_294_967_296.0 + f64::from(lower)
}

/// Convert an `i64` to `f64` going through [`u64_to_f64`] for the magnitude,
/// preserving sign explicitly.
pub(super) fn i64_to_f64(value: i64) -> f64 {
    if value.is_negative() {
        -u64_to_f64(value.unsigned_abs())
    } else {
        u64_to_f64(value.unsigned_abs())
    }
}

pub(super) fn map_transaction_mode(mode: Option<TransactionMode>) -> IsolationLevel {
    match mode.unwrap_or(TransactionMode::ReadCommitted) {
        TransactionMode::ReadCommitted => IsolationLevel::ReadCommitted,
        TransactionMode::SnapshotIsolation => IsolationLevel::SnapshotIsolation,
        TransactionMode::Serializable => IsolationLevel::Serializable,
    }
}

pub(super) fn command_ok(tag: &str) -> StatementResult {
    StatementResult::Command {
        tag: tag.to_owned(),
        rows_affected: 0,
    }
}

pub(super) fn map_execution_result(result: ExecutionResult) -> StatementResult {
    match result {
        ExecutionResult::Query { columns, rows } => StatementResult::Query {
            columns: columns
                .into_iter()
                .map(|column| ResultColumn {
                    name: column.name,
                    data_type: column.data_type,
                    text_type_modifier: column.text_type_modifier,
                    nullable: column.nullable,
                })
                .collect(),
            rows,
        },
        ExecutionResult::Command { tag, rows_affected } => {
            StatementResult::Command { tag, rows_affected }
        }
        ExecutionResult::CopyIn { table_id, columns } => StatementResult::CopyIn {
            table_id,
            columns: columns
                .into_iter()
                .map(|c| ResultColumn {
                    name: c.name,
                    data_type: c.data_type,
                    text_type_modifier: c.text_type_modifier,
                    nullable: c.nullable,
                })
                .collect(),
        },
        ExecutionResult::CopyOut { data, column_count } => {
            StatementResult::CopyOut { data, column_count }
        }
    }
}

/// Convert `usize` to `f64` via [`u64_to_f64`], saturating to `u64::MAX`
/// on overflow so callers reading metrics never observe `as`-cast UB.
#[inline]
pub(super) fn usize_to_f64(value: usize) -> f64 {
    u64_to_f64(u64::try_from(value).unwrap_or(u64::MAX))
}

fn random_session_token() -> DbResult<[u8; 32]> {
    let mut token = [0u8; 32];
    fill_random(&mut token).map_err(|error| {
        DbError::internal(format!(
            "failed to generate session token securely: {error}"
        ))
    })?;
    Ok(token)
}
