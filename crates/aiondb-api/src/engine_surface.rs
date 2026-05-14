//! `DatabaseSurface` adapter for the concrete engine crate.
//!
//! Keeping this impl in `aiondb-api` prevents `aiondb-engine` from depending on
//! its public facade crate while still giving API consumers an executable
//! surface over the production engine.

use aiondb_core::DbResult;
use aiondb_engine::{Engine, QueryEngine, SessionHandle, StatementResult};

use crate::{DatabaseCapability, DatabaseSurface, ExecutionOutcome};

impl DatabaseSurface for Engine {
    type Session = SessionHandle;

    fn execute_sql_outline(
        &self,
        session: &Self::Session,
        sql: &str,
    ) -> DbResult<Vec<ExecutionOutcome>> {
        Ok(self
            .execute_sql(session, sql)?
            .into_iter()
            .map(statement_result_to_outcome)
            .collect())
    }

    fn has_capability(&self, capability: DatabaseCapability) -> bool {
        match capability {
            DatabaseCapability::PreparedStatements => true,
            DatabaseCapability::Copy => true,
            DatabaseCapability::Notify => true,
            DatabaseCapability::Vector => true,
            DatabaseCapability::Graph => true,
            // Backup and replication are intentionally reported as
            // unsupported here. Facades that probe these capabilities must
            // see `false` so they can route around them statically rather
            // than discover the gap at execution time.
            DatabaseCapability::Replication => false,
            DatabaseCapability::Backup => false,
        }
    }

    fn terminate_session(&self, session: Self::Session) -> DbResult<()> {
        QueryEngine::terminate(self, session)
    }
}

fn statement_result_to_outcome(result: StatementResult) -> ExecutionOutcome {
    match result {
        StatementResult::Query { columns, rows } => ExecutionOutcome::Rows {
            tag: format!("SELECT {}", rows.len()),
            columns: columns.iter().map(|c| c.name.clone()).collect(),
            row_count: rows.len() as u64,
        },
        StatementResult::Command { tag, rows_affected } => {
            ExecutionOutcome::Command { tag, rows_affected }
        }
        StatementResult::Notice { message } => ExecutionOutcome::Command {
            tag: format!("NOTICE: {message}"),
            rows_affected: 0,
        },
        StatementResult::CopyIn { .. } => ExecutionOutcome::Command {
            tag: "COPY".to_owned(),
            rows_affected: 0,
        },
        StatementResult::CopyOut { data, .. } => {
            let row_count = data.lines().count() as u64;
            ExecutionOutcome::Command {
                tag: format!("COPY {row_count}"),
                rows_affected: row_count,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_engine::EngineBuilder;

    fn build_engine() -> Engine {
        EngineBuilder::for_testing().build().unwrap()
    }

    /// Capabilities documented as unsupported in the v0.1 product
    /// contract must report `false` so facades can route around them at
    /// static probe time.
    #[test]
    fn has_capability_reports_unsupported_features_as_false() {
        let engine = build_engine();
        assert!(
            !engine.has_capability(DatabaseCapability::Backup),
            "Backup is documented as out-of-scope for v0.1; capability probe must return false"
        );
        assert!(
            !engine.has_capability(DatabaseCapability::Replication),
            "Replication / clustering is out-of-scope for v0.1; capability probe must return false"
        );
    }

    #[test]
    fn has_capability_reports_supported_features_as_true() {
        let engine = build_engine();
        assert!(engine.has_capability(DatabaseCapability::PreparedStatements));
        assert!(engine.has_capability(DatabaseCapability::Copy));
        assert!(engine.has_capability(DatabaseCapability::Notify));
        assert!(engine.has_capability(DatabaseCapability::Vector));
        assert!(engine.has_capability(DatabaseCapability::Graph));
    }
}
