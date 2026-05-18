#![allow(clippy::doc_markdown, clippy::missing_errors_doc)]

use std::{collections::BTreeMap, sync::Arc};

use aiondb_core::{DataType, DbError, DbResult, Value};
use aiondb_parser::Statement;
use aiondb_security::{Credential, ScramVerifier, SecretBytes, TransportInfo};
use aiondb_tx::IsolationLevel;

use crate::{
    prepared::{PortalBatch, PortalDescription, PreparedStatementDesc, StatementResult},
    session::{SessionHandle, SessionInfo},
};

pub enum StartupAuthentication {
    Trust,
    CleartextPassword,
    ScramSha256 {
        verifier: ScramVerifier,
        proof_token: SecretBytes,
    },
}

impl std::fmt::Debug for StartupAuthentication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Trust => f.write_str("StartupAuthentication::Trust"),
            Self::CleartextPassword => f.write_str("StartupAuthentication::CleartextPassword"),
            Self::ScramSha256 { verifier, .. } => f
                .debug_struct("StartupAuthentication::ScramSha256")
                .field("verifier", verifier)
                .field("proof_token", &"**redacted**")
                .finish(),
        }
    }
}

#[derive(Debug)]
pub struct StartupParams {
    pub database: String,
    pub application_name: Option<String>,
    pub options: BTreeMap<String, String>,
    pub credential: Credential,
    pub transport: TransportInfo,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplicationIdentity {
    pub system_identifier: String,
    pub timeline: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WireStateCleanupHint {
    DeallocateAll,
    DeallocateName(String),
    ClosePortal(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlStatementWireMetadata {
    pub description: Option<PreparedStatementDesc>,
    pub effective_statement: Option<Statement>,
    pub cleanup_hint: Option<WireStateCleanupHint>,
    pub changes_result_metadata: bool,
}

/// Startup/authentication facet of [`QueryEngine`].
///
/// New protocol consumers should depend on this narrower trait when they only
/// need connection establishment, rather than taking the full engine surface.
pub trait QueryStartup: Send + Sync {
    fn requires_password(&self) -> bool;
    fn startup_authentication(
        &self,
        user: &str,
        database: &str,
        transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication>;
    fn startup_rate_limit_check(&self, principal: &str, transport: &TransportInfo) -> DbResult<()>;
    fn startup_rate_limit_record_failure(
        &self,
        principal: &str,
        transport: &TransportInfo,
    ) -> DbResult<()>;
    fn startup(&self, params: StartupParams) -> DbResult<(SessionHandle, SessionInfo)>;
}

impl<T: QueryEngine + ?Sized> QueryStartup for T {
    fn requires_password(&self) -> bool {
        QueryEngine::requires_password(self)
    }

    fn startup_authentication(
        &self,
        user: &str,
        database: &str,
        transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication> {
        QueryEngine::startup_authentication(self, user, database, transport)
    }

    fn startup_rate_limit_check(&self, principal: &str, transport: &TransportInfo) -> DbResult<()> {
        QueryEngine::startup_rate_limit_check(self, principal, transport)
    }

    fn startup_rate_limit_record_failure(
        &self,
        principal: &str,
        transport: &TransportInfo,
    ) -> DbResult<()> {
        QueryEngine::startup_rate_limit_record_failure(self, principal, transport)
    }

    fn startup(&self, params: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
        QueryEngine::startup(self, params)
    }
}

/// Transaction control facet of [`QueryEngine`].
pub trait QueryTransactions: Send + Sync {
    fn has_active_transaction(&self, session: &SessionHandle) -> DbResult<bool>;
    fn begin_transaction(&self, session: &SessionHandle, isolation: IsolationLevel)
        -> DbResult<()>;
    fn commit_transaction(&self, session: &SessionHandle) -> DbResult<()>;
    fn rollback_transaction(&self, session: &SessionHandle) -> DbResult<()>;
    fn savepoint_generation(&self, session: &SessionHandle, name: &str) -> DbResult<Option<u64>>;
}

impl<T: QueryEngine + ?Sized> QueryTransactions for T {
    fn has_active_transaction(&self, session: &SessionHandle) -> DbResult<bool> {
        QueryEngine::has_active_transaction(self, session)
    }

    fn begin_transaction(
        &self,
        session: &SessionHandle,
        isolation: IsolationLevel,
    ) -> DbResult<()> {
        QueryEngine::begin_transaction(self, session, isolation)
    }

    fn commit_transaction(&self, session: &SessionHandle) -> DbResult<()> {
        QueryEngine::commit_transaction(self, session)
    }

    fn rollback_transaction(&self, session: &SessionHandle) -> DbResult<()> {
        QueryEngine::rollback_transaction(self, session)
    }

    fn savepoint_generation(&self, session: &SessionHandle, name: &str) -> DbResult<Option<u64>> {
        QueryEngine::savepoint_generation(self, session, name)
    }
}

/// Simple-query SQL execution facet of [`QueryEngine`].
pub trait QuerySimpleSql: Send + Sync {
    fn execute_sql(&self, session: &SessionHandle, sql: &str) -> DbResult<Vec<StatementResult>>;
    fn try_execute_check_estimated_rows_query(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<Option<Vec<StatementResult>>>;
}

impl<T: QueryEngine + ?Sized> QuerySimpleSql for T {
    fn execute_sql(&self, session: &SessionHandle, sql: &str) -> DbResult<Vec<StatementResult>> {
        QueryEngine::execute_sql(self, session, sql)
    }

    fn try_execute_check_estimated_rows_query(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        QueryEngine::try_execute_check_estimated_rows_query(self, session, sql)
    }
}

/// Extended-query protocol facet of [`QueryEngine`].
pub trait QueryExtendedProtocol: Send + Sync {
    fn prepare(
        &self,
        session: &SessionHandle,
        statement_name: String,
        sql: String,
    ) -> DbResult<PreparedStatementDesc>;
    fn prepare_with_param_hints(
        &self,
        session: &SessionHandle,
        statement_name: String,
        sql: String,
        param_type_hints: Vec<Option<DataType>>,
    ) -> DbResult<PreparedStatementDesc>;
    fn describe_statement(
        &self,
        session: &SessionHandle,
        statement_name: &str,
    ) -> DbResult<PreparedStatementDesc>;
    fn bind(
        &self,
        session: &SessionHandle,
        portal_name: String,
        statement_name: String,
        params: Vec<Value>,
    ) -> DbResult<()>;
    fn describe_portal(
        &self,
        session: &SessionHandle,
        portal_name: &str,
    ) -> DbResult<PortalDescription>;
    fn execute_portal(
        &self,
        session: &SessionHandle,
        portal_name: &str,
        max_rows: usize,
    ) -> DbResult<PortalBatch>;
    fn execute_portal_with_notices(
        &self,
        session: &SessionHandle,
        portal_name: &str,
        max_rows: usize,
    ) -> DbResult<(PortalBatch, Vec<String>)>;
    fn execute_prepared_statement_with_notices(
        &self,
        session: &SessionHandle,
        statement_name: String,
        params: Vec<Value>,
        max_rows: usize,
    ) -> DbResult<(PortalBatch, Vec<String>)>;
    fn close_statement(&self, session: &SessionHandle, statement_name: &str) -> DbResult<()>;
    fn close_portal(&self, session: &SessionHandle, portal_name: &str) -> DbResult<()>;
}

impl<T: QueryEngine + ?Sized> QueryExtendedProtocol for T {
    fn prepare(
        &self,
        session: &SessionHandle,
        statement_name: String,
        sql: String,
    ) -> DbResult<PreparedStatementDesc> {
        QueryEngine::prepare(self, session, statement_name, sql)
    }

    fn prepare_with_param_hints(
        &self,
        session: &SessionHandle,
        statement_name: String,
        sql: String,
        param_type_hints: Vec<Option<DataType>>,
    ) -> DbResult<PreparedStatementDesc> {
        QueryEngine::prepare_with_param_hints(self, session, statement_name, sql, param_type_hints)
    }

    fn describe_statement(
        &self,
        session: &SessionHandle,
        statement_name: &str,
    ) -> DbResult<PreparedStatementDesc> {
        QueryEngine::describe_statement(self, session, statement_name)
    }

    fn bind(
        &self,
        session: &SessionHandle,
        portal_name: String,
        statement_name: String,
        params: Vec<Value>,
    ) -> DbResult<()> {
        QueryEngine::bind(self, session, portal_name, statement_name, params)
    }

    fn describe_portal(
        &self,
        session: &SessionHandle,
        portal_name: &str,
    ) -> DbResult<PortalDescription> {
        QueryEngine::describe_portal(self, session, portal_name)
    }

    fn execute_portal(
        &self,
        session: &SessionHandle,
        portal_name: &str,
        max_rows: usize,
    ) -> DbResult<PortalBatch> {
        QueryEngine::execute_portal(self, session, portal_name, max_rows)
    }

    fn execute_portal_with_notices(
        &self,
        session: &SessionHandle,
        portal_name: &str,
        max_rows: usize,
    ) -> DbResult<(PortalBatch, Vec<String>)> {
        QueryEngine::execute_portal_with_notices(self, session, portal_name, max_rows)
    }

    fn execute_prepared_statement_with_notices(
        &self,
        session: &SessionHandle,
        statement_name: String,
        params: Vec<Value>,
        max_rows: usize,
    ) -> DbResult<(PortalBatch, Vec<String>)> {
        QueryEngine::execute_prepared_statement_with_notices(
            self,
            session,
            statement_name,
            params,
            max_rows,
        )
    }

    fn close_statement(&self, session: &SessionHandle, statement_name: &str) -> DbResult<()> {
        QueryEngine::close_statement(self, session, statement_name)
    }

    fn close_portal(&self, session: &SessionHandle, portal_name: &str) -> DbResult<()> {
        QueryEngine::close_portal(self, session, portal_name)
    }
}

/// PG wire compatibility hooks that are not part of core SQL execution.
pub trait QueryWireCompatibility: Send + Sync {
    fn describe_sql_statement(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<PreparedStatementDesc>>;
    fn sql_statement_wire_metadata(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<SqlStatementWireMetadata>;
    fn sql_statement_wire_cleanup_hint(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<WireStateCleanupHint>>;
    fn sql_statement_wire_effective_statement(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Statement>>;
    fn statement_wire_cleanup_hint(
        &self,
        session: &SessionHandle,
        statement_name: &str,
    ) -> DbResult<Option<WireStateCleanupHint>>;
    fn statement_wire_effective_statement(
        &self,
        session: &SessionHandle,
        statement_name: &str,
    ) -> DbResult<Option<Statement>>;
    fn execute_copy_from(
        &self,
        session: &SessionHandle,
        table_id: aiondb_core::RelationId,
        data: &str,
    ) -> DbResult<StatementResult>;
    fn drain_pending_notices(&self, session: &SessionHandle) -> DbResult<Vec<String>>;
}

impl<T: QueryEngine + ?Sized> QueryWireCompatibility for T {
    fn describe_sql_statement(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<PreparedStatementDesc>> {
        QueryEngine::describe_sql_statement(self, session, statement_sql, statement)
    }

    fn sql_statement_wire_metadata(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<SqlStatementWireMetadata> {
        QueryEngine::sql_statement_wire_metadata(self, session, statement_sql, statement)
    }

    fn sql_statement_wire_cleanup_hint(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<WireStateCleanupHint>> {
        QueryEngine::sql_statement_wire_cleanup_hint(self, session, statement_sql, statement)
    }

    fn sql_statement_wire_effective_statement(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Statement>> {
        QueryEngine::sql_statement_wire_effective_statement(self, session, statement_sql, statement)
    }

    fn statement_wire_cleanup_hint(
        &self,
        session: &SessionHandle,
        statement_name: &str,
    ) -> DbResult<Option<WireStateCleanupHint>> {
        QueryEngine::statement_wire_cleanup_hint(self, session, statement_name)
    }

    fn statement_wire_effective_statement(
        &self,
        session: &SessionHandle,
        statement_name: &str,
    ) -> DbResult<Option<Statement>> {
        QueryEngine::statement_wire_effective_statement(self, session, statement_name)
    }

    fn execute_copy_from(
        &self,
        session: &SessionHandle,
        table_id: aiondb_core::RelationId,
        data: &str,
    ) -> DbResult<StatementResult> {
        QueryEngine::execute_copy_from(self, session, table_id, data)
    }

    fn drain_pending_notices(&self, session: &SessionHandle) -> DbResult<Vec<String>> {
        QueryEngine::drain_pending_notices(self, session)
    }
}

/// Replication facet of [`QueryEngine`].
pub trait QueryReplication: Send + Sync {
    fn replication_manager(&self) -> Option<Arc<crate::engine::streaming::ReplicationManager>>;
    fn replication_identity(&self) -> Option<ReplicationIdentity>;
    fn replication_timeline_history(&self, timeline: u32) -> DbResult<Option<String>>;
    fn authorize_replication_connection(
        &self,
        session: &SessionHandle,
        info: &SessionInfo,
    ) -> DbResult<()>;
    /// Hand back the engine's `StorageDML` handle so the replica runtime
    /// can wire it into a [`aiondb_storage_api::StorageDML::apply_replicated_wal_entry`]
    /// pump. Returns `None` for engines that do not expose a storage
    /// backend (e.g. shim test engines).
    fn storage_dml_for_replication(&self) -> Option<Arc<dyn aiondb_storage_api::StorageDML>>;
}

impl<T: QueryEngine + ?Sized> QueryReplication for T {
    fn replication_manager(&self) -> Option<Arc<crate::engine::streaming::ReplicationManager>> {
        QueryEngine::replication_manager(self)
    }

    fn replication_identity(&self) -> Option<ReplicationIdentity> {
        QueryEngine::replication_identity(self)
    }

    fn replication_timeline_history(&self, timeline: u32) -> DbResult<Option<String>> {
        QueryEngine::replication_timeline_history(self, timeline)
    }

    fn authorize_replication_connection(
        &self,
        session: &SessionHandle,
        info: &SessionInfo,
    ) -> DbResult<()> {
        QueryEngine::authorize_replication_connection(self, session, info)
    }

    fn storage_dml_for_replication(&self) -> Option<Arc<dyn aiondb_storage_api::StorageDML>> {
        QueryEngine::storage_dml_for_replication(self)
    }
}

/// Session lifecycle/control facet of [`QueryEngine`].
pub trait QuerySessionControl: Send + Sync {
    fn check_session_cancellation(&self, session: &SessionHandle) -> DbResult<()>;
    fn cancel_session(&self, session: &SessionHandle) -> DbResult<()>;
    fn terminate(&self, session: SessionHandle) -> DbResult<()>;
    fn session_count(&self) -> DbResult<usize>;
}

impl<T: QueryEngine + ?Sized> QuerySessionControl for T {
    fn check_session_cancellation(&self, session: &SessionHandle) -> DbResult<()> {
        QueryEngine::check_session_cancellation(self, session)
    }

    fn cancel_session(&self, session: &SessionHandle) -> DbResult<()> {
        QueryEngine::cancel_session(self, session)
    }

    fn terminate(&self, session: SessionHandle) -> DbResult<()> {
        QueryEngine::terminate(self, session)
    }

    fn session_count(&self) -> DbResult<usize> {
        QueryEngine::session_count(self)
    }
}

/// Narrow engine contract required by the PostgreSQL wire frontend.
///
/// This intentionally composes protocol-facing facets instead of exposing the
/// full historical [`QueryEngine`] surface as a dependency of pgwire types.
pub trait PgWireEngine:
    QueryStartup
    + QueryTransactions
    + QuerySimpleSql
    + QueryExtendedProtocol
    + QueryWireCompatibility
    + QueryReplication
    + QuerySessionControl
{
}

impl<T> PgWireEngine for T where
    T: QueryStartup
        + QueryTransactions
        + QuerySimpleSql
        + QueryExtendedProtocol
        + QueryWireCompatibility
        + QueryReplication
        + QuerySessionControl
        + ?Sized
{
}

/// Legacy aggregate trait used by pgwire and embedded call sites.
///
/// Prefer the narrower facet traits above for new code. `QueryEngine` stays as
/// a compatibility super-surface while engine internals are split up.
pub trait QueryEngine: Send + Sync {
    /// Whether the engine requires a cleartext password during PG wire startup.
    fn requires_password(&self) -> bool {
        false
    }
    fn startup_authentication(
        &self,
        _user: &str,
        _database: &str,
        _transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication> {
        Ok(if self.requires_password() {
            StartupAuthentication::CleartextPassword
        } else {
            StartupAuthentication::Trust
        })
    }
    fn startup_rate_limit_check(
        &self,
        _principal: &str,
        _transport: &TransportInfo,
    ) -> DbResult<()> {
        Ok(())
    }
    fn startup_rate_limit_record_failure(
        &self,
        _principal: &str,
        _transport: &TransportInfo,
    ) -> DbResult<()> {
        Ok(())
    }
    fn replication_manager(&self) -> Option<Arc<crate::engine::streaming::ReplicationManager>> {
        None
    }
    fn replication_identity(&self) -> Option<ReplicationIdentity> {
        None
    }
    fn replication_timeline_history(&self, _timeline: u32) -> DbResult<Option<String>> {
        Ok(None)
    }
    fn authorize_replication_connection(
        &self,
        _session: &SessionHandle,
        _info: &SessionInfo,
    ) -> DbResult<()> {
        Err(DbError::insufficient_privilege(
            "must be superuser to use replication mode",
        ))
    }
    fn storage_dml_for_replication(&self) -> Option<Arc<dyn aiondb_storage_api::StorageDML>> {
        None
    }
    fn startup(&self, params: StartupParams) -> DbResult<(SessionHandle, SessionInfo)>;
    fn has_active_transaction(&self, session: &SessionHandle) -> DbResult<bool>;
    fn begin_transaction(&self, session: &SessionHandle, isolation: IsolationLevel)
        -> DbResult<()>;
    fn commit_transaction(&self, session: &SessionHandle) -> DbResult<()>;
    fn rollback_transaction(&self, session: &SessionHandle) -> DbResult<()>;
    fn execute_sql(&self, session: &SessionHandle, sql: &str) -> DbResult<Vec<StatementResult>>;
    fn execute_explain_graph_summary_json(
        &self,
        session: &SessionHandle,
        sql: &str,
        analyze: bool,
    ) -> DbResult<serde_json::Value> {
        let explain_sql = if analyze {
            format!("EXPLAIN ANALYZE {sql}")
        } else {
            format!("EXPLAIN {sql}")
        };
        let results = self.execute_sql(session, &explain_sql)?;
        let result = results
            .first()
            .ok_or_else(|| DbError::internal("EXPLAIN did not return a query result"))?;
        let StatementResult::Query { rows, .. } = result else {
            return Err(DbError::internal("EXPLAIN did not return a query result"));
        };
        let lines = rows
            .iter()
            .filter_map(|row| match row.values.as_slice() {
                [Value::Text(line)] => Some(line.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let payload = lines
            .iter()
            .find_map(|line| {
                line.strip_prefix("Graph Summary JSON:")
                    .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload.trim()).ok())
            })
            .ok_or_else(|| {
                DbError::internal("EXPLAIN output did not contain Graph Summary JSON payload")
            })?;
        Ok(payload)
    }
    fn execute_explain_graph_detail_json(
        &self,
        session: &SessionHandle,
        sql: &str,
        analyze: bool,
    ) -> DbResult<serde_json::Value> {
        let explain_sql = if analyze {
            format!("EXPLAIN ANALYZE {sql}")
        } else {
            format!("EXPLAIN {sql}")
        };
        let results = self.execute_sql(session, &explain_sql)?;
        let result = results
            .first()
            .ok_or_else(|| DbError::internal("EXPLAIN did not return a query result"))?;
        let StatementResult::Query { rows, .. } = result else {
            return Err(DbError::internal("EXPLAIN did not return a query result"));
        };
        let lines = rows
            .iter()
            .filter_map(|row| match row.values.as_slice() {
                [Value::Text(line)] => Some(line.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let payload = lines
            .iter()
            .find_map(|line| {
                line.strip_prefix("Graph Detail JSON:")
                    .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload.trim()).ok())
            })
            .ok_or_else(|| {
                DbError::internal("EXPLAIN output did not contain Graph Detail JSON payload")
            })?;
        Ok(payload)
    }
    /// Optional fast-path used by EXPLAIN-driven check_estimated_rows
    /// regression helpers. Default returns `Ok(None)` so engines that
    /// don't model EXPLAIN output fall back to standard execution.
    fn try_execute_check_estimated_rows_query(
        &self,
        _session: &SessionHandle,
        _sql: &str,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        Ok(None)
    }
    fn describe_sql_statement(
        &self,
        _session: &SessionHandle,
        _statement_sql: &str,
        _statement: &Statement,
    ) -> DbResult<Option<PreparedStatementDesc>> {
        Ok(None)
    }
    fn sql_statement_wire_metadata(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<SqlStatementWireMetadata> {
        Ok(SqlStatementWireMetadata {
            description: self.describe_sql_statement(session, statement_sql, statement)?,
            effective_statement: self.sql_statement_wire_effective_statement(
                session,
                statement_sql,
                statement,
            )?,
            cleanup_hint: self.sql_statement_wire_cleanup_hint(
                session,
                statement_sql,
                statement,
            )?,
            changes_result_metadata: false,
        })
    }
    fn sql_statement_wire_cleanup_hint(
        &self,
        _session: &SessionHandle,
        _statement_sql: &str,
        _statement: &Statement,
    ) -> DbResult<Option<WireStateCleanupHint>> {
        Ok(None)
    }
    fn sql_statement_wire_effective_statement(
        &self,
        _session: &SessionHandle,
        _statement_sql: &str,
        _statement: &Statement,
    ) -> DbResult<Option<Statement>> {
        Ok(None)
    }
    fn prepare(
        &self,
        session: &SessionHandle,
        statement_name: String,
        sql: String,
    ) -> DbResult<PreparedStatementDesc>;
    fn prepare_with_param_hints(
        &self,
        session: &SessionHandle,
        statement_name: String,
        sql: String,
        param_type_hints: Vec<Option<DataType>>,
    ) -> DbResult<PreparedStatementDesc> {
        let _ = param_type_hints;
        self.prepare(session, statement_name, sql)
    }
    fn describe_statement(
        &self,
        session: &SessionHandle,
        statement_name: &str,
    ) -> DbResult<PreparedStatementDesc>;
    fn bind(
        &self,
        session: &SessionHandle,
        portal_name: String,
        statement_name: String,
        params: Vec<Value>,
    ) -> DbResult<()>;
    fn describe_portal(
        &self,
        session: &SessionHandle,
        portal_name: &str,
    ) -> DbResult<PortalDescription>;
    fn execute_portal(
        &self,
        session: &SessionHandle,
        portal_name: &str,
        max_rows: usize,
    ) -> DbResult<PortalBatch>;
    fn execute_portal_with_notices(
        &self,
        session: &SessionHandle,
        portal_name: &str,
        max_rows: usize,
    ) -> DbResult<(PortalBatch, Vec<String>)> {
        let batch = self.execute_portal(session, portal_name, max_rows)?;
        let notices = self.drain_pending_notices(session)?;
        Ok((batch, notices))
    }
    fn bind_and_execute_portal(
        &self,
        session: &SessionHandle,
        portal_name: String,
        statement_name: String,
        params: Vec<Value>,
        max_rows: usize,
    ) -> DbResult<PortalBatch> {
        self.bind(session, portal_name.clone(), statement_name, params)?;
        self.execute_portal(session, &portal_name, max_rows)
    }
    fn bind_and_execute_portal_with_notices(
        &self,
        session: &SessionHandle,
        portal_name: String,
        statement_name: String,
        params: Vec<Value>,
        max_rows: usize,
    ) -> DbResult<(PortalBatch, Vec<String>)> {
        let batch =
            self.bind_and_execute_portal(session, portal_name, statement_name, params, max_rows)?;
        let notices = self.drain_pending_notices(session)?;
        Ok((batch, notices))
    }
    fn execute_prepared_statement_with_notices(
        &self,
        session: &SessionHandle,
        statement_name: String,
        params: Vec<Value>,
        max_rows: usize,
    ) -> DbResult<(PortalBatch, Vec<String>)> {
        self.bind_and_execute_portal_with_notices(
            session,
            String::new(),
            statement_name,
            params,
            max_rows,
        )
    }
    fn statement_wire_cleanup_hint(
        &self,
        _session: &SessionHandle,
        _statement_name: &str,
    ) -> DbResult<Option<WireStateCleanupHint>> {
        Ok(None)
    }
    fn statement_wire_effective_statement(
        &self,
        _session: &SessionHandle,
        _statement_name: &str,
    ) -> DbResult<Option<Statement>> {
        Ok(None)
    }
    fn close_statement(&self, session: &SessionHandle, statement_name: &str) -> DbResult<()>;
    fn close_portal(&self, session: &SessionHandle, portal_name: &str) -> DbResult<()>;
    fn execute_copy_from(
        &self,
        session: &SessionHandle,
        table_id: aiondb_core::RelationId,
        data: &str,
    ) -> DbResult<StatementResult>;
    fn drain_pending_notices(&self, _session: &SessionHandle) -> DbResult<Vec<String>> {
        Ok(Vec::new())
    }
    fn savepoint_generation(&self, _session: &SessionHandle, _name: &str) -> DbResult<Option<u64>> {
        Ok(None)
    }
    fn check_session_cancellation(&self, _session: &SessionHandle) -> DbResult<()> {
        Ok(())
    }
    fn cancel_session(&self, session: &SessionHandle) -> DbResult<()>;
    fn terminate(&self, session: SessionHandle) -> DbResult<()>;
    /// Return the number of currently active sessions.
    fn session_count(&self) -> DbResult<usize>;
}
