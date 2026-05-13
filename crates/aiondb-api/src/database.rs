//! Minimal `DatabaseSurface` contract.
//!
//! Describes the core operations that a host (engine) must expose so a
//! consumer (pgwire, embedded, dashboard) can serve SQL clients. This
//! contract is deliberately **minimal**: for now it covers `execute` on a
//! SQL string. Extension to Parse / Bind / Execute / Describe will happen
//! by adding associated methods, and each addition requires review (ADR).
//!
//! The trait uses associated types to stay decoupled from the concrete
//! engine: `Session` (opaque session handle) and `Result` (statement
//! result). Consumers that need structured results should rely on the
//! `into_command_tag` / `rows` methods defined here.

use aiondb_core::DbResult;

/// Declarative capability of an engine.
///
/// Lets facades statically detect whether an optional feature is supported
/// without calling the API and receiving a
/// `FeatureNotSupported`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DatabaseCapability {
    PreparedStatements,
    Copy,
    Notify,
    Replication,
    Backup,
    Vector,
    Graph,
}

/// Canonical execution result, interpretable by the facade to format the
/// client response (CommandComplete tag, row count, rows returned for
/// SELECT).
#[derive(Debug, Clone)]
pub enum ExecutionOutcome {
    /// Utility command without rows (DDL, DCL, BEGIN, etc.).
    Command { tag: String, rows_affected: u64 },
    /// Query returning rows (SELECT, DML with RETURNING).
    Rows {
        tag: String,
        columns: Vec<String>,
        row_count: u64,
    },
}

impl ExecutionOutcome {
    pub fn tag(&self) -> &str {
        match self {
            Self::Command { tag, .. } | Self::Rows { tag, .. } => tag.as_str(),
        }
    }

    pub fn rows_affected(&self) -> u64 {
        match self {
            Self::Command { rows_affected, .. } => *rows_affected,
            Self::Rows { row_count, .. } => *row_count,
        }
    }
}

/// Minimal contract for a SQL engine exposed to facades.
///
/// Methods are intentionally limited: adding a method requires an ADR.
/// For richer APIs (startup, transactions, prepare/bind/execute,
/// replication), consumers should prefer the narrow facade traits exposed
/// by `aiondb-engine` (`QuerySimpleSql`, `QueryExtendedProtocol`, etc.)
/// rather than the historical aggregator trait `QueryEngine`.
pub trait DatabaseSurface: Send + Sync {
    /// Opaque session handle.
    type Session;

    /// Executes an SQL string in a session and returns a per-statement
    /// execution summary.
    fn execute_sql_outline(
        &self,
        session: &Self::Session,
        sql: &str,
    ) -> DbResult<Vec<ExecutionOutcome>>;

    /// Returns `true` if the named capability is enabled on this host.
    fn has_capability(&self, capability: DatabaseCapability) -> bool;

    /// Terminates a session cleanly (rollback any active transaction,
    /// cleanup prepared statements and portals).
    fn terminate_session(&self, session: Self::Session) -> DbResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_tag_and_rows_accessors() {
        let cmd = ExecutionOutcome::Command {
            tag: "CREATE TABLE".into(),
            rows_affected: 0,
        };
        assert_eq!(cmd.tag(), "CREATE TABLE");
        assert_eq!(cmd.rows_affected(), 0);

        let rows = ExecutionOutcome::Rows {
            tag: "SELECT 3".into(),
            columns: vec!["id".into()],
            row_count: 3,
        };
        assert_eq!(rows.tag(), "SELECT 3");
        assert_eq!(rows.rows_affected(), 3);
    }
}
