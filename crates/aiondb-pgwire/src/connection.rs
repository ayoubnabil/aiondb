//! Per-client connection handler for the `PostgreSQL` wire protocol.
//! Handles startup, authentication, simple/extended query protocol, and termination.

use std::collections::HashMap;
use std::{str, sync::Arc, time::Duration};

use aiondb_core::{
    compat_server_version_num_string, compat_timezone, DataType, DbError, SqlState, Value,
    COMPAT_CLIENT_ENCODING, COMPAT_DATE_STYLE, COMPAT_DEFAULT_TRANSACTION_READ_ONLY,
    COMPAT_INTEGER_DATETIMES, COMPAT_INTERVAL_STYLE, COMPAT_SERVER_ENCODING, COMPAT_SERVER_VERSION,
    COMPAT_STANDARD_CONFORMING_STRINGS,
};
use aiondb_engine::{
    Credential, DbResult, PgWireEngine, PortalBatch, PortalDescription, PreparedStatementDesc,
    SecretString, SessionHandle, SqlStatementWireMetadata, StartupAuthentication, StartupParams,
    StatementResult, TransportInfo, TransportKind, WireStateCleanupHint,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tracing::{debug, error, info, warn};

use std::sync::atomic::Ordering;

use crate::bind::coerce_bind_params_dispatched;
use crate::codec::{self, MessageWriter, StartupPayload};
use crate::engine_pool::EnginePool;
use crate::messages::{
    self, CloseTarget, DescribeTarget, FieldDescription, FrontendMessage, TransactionStatus,
};
use crate::replication::{ReplicationCommand, ReplicationStreamHandler};
use crate::server::{CancelRegistry, ServerMetrics};

const FAILED_TRANSACTION_MESSAGE: &str =
    "current transaction is aborted, commands ignored until end of transaction block";

/// Fallback portal limit used when no explicit value is configured.
const DEFAULT_MAX_PORTALS_FALLBACK: usize = 1000;

mod copy;
mod extended_query;
mod helpers;
mod replication_mode;
mod result_wire;
mod scram_auth;
mod startup;
mod state;

#[cfg(test)]
use helpers::{data_type_to_pg, result_column_to_field_fmt, validate_format_code};
use helpers::{
    pg_oid_to_data_type, validate_bind_formats, validate_result_formats, write_stmt_description,
};

#[derive(Clone, Debug, Default)]
struct StatementWireState {
    param_oids: Vec<u32>,
    query: String,
    prepared_desc: Option<PreparedStatementDesc>,
    deferred_describe_response_cache: HashMap<Vec<i16>, Arc<[u8]>>,
    direct_param_result_alias_slots: Option<Arc<[Option<usize>]>>,
    parsed_statement: Option<aiondb_parser::Statement>,
    parsed_statement_kind: ParsedStatementKind,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ParsedStatementKind {
    #[default]
    Other,
    Begin,
    Commit,
    Rollback,
    Savepoint,
    RollbackToSavepoint,
    ReleaseSavepoint,
    ExecuteNoop,
}

impl ParsedStatementKind {
    fn from_parsed_statement(statement: Option<&aiondb_parser::Statement>) -> Self {
        match statement {
            Some(aiondb_parser::Statement::Begin { .. }) => Self::Begin,
            Some(aiondb_parser::Statement::Commit { .. }) => Self::Commit,
            Some(aiondb_parser::Statement::Rollback { .. }) => Self::Rollback,
            Some(aiondb_parser::Statement::Savepoint { .. }) => Self::Savepoint,
            Some(aiondb_parser::Statement::RollbackToSavepoint { .. }) => Self::RollbackToSavepoint,
            Some(aiondb_parser::Statement::ReleaseSavepoint { .. }) => Self::ReleaseSavepoint,
            Some(statement) if is_sql_execute_statement(statement) => Self::ExecuteNoop,
            _ => Self::Other,
        }
    }

    fn opens_explicit_transaction(self) -> bool {
        matches!(self, Self::Begin)
    }

    fn is_execute_noop(self) -> bool {
        matches!(self, Self::ExecuteNoop)
    }

    fn touches_savepoint_state(self) -> bool {
        matches!(
            self,
            Self::Savepoint
                | Self::RollbackToSavepoint
                | Self::ReleaseSavepoint
                | Self::Commit
                | Self::Rollback
        )
    }

    fn is_commit_or_rollback(self) -> bool {
        matches!(self, Self::Commit | Self::Rollback)
    }
}

#[derive(Clone, Debug, Default)]
struct PortalWireState {
    result_formats: Arc<[i16]>,
    statement_name: String,
    rows_sent: u64,
    created_under_savepoint_generation: Option<u64>,
    deferred_bind_params: Option<Vec<Value>>,
    deferred_describe_response: Option<Arc<[u8]>>,
}

#[derive(Clone, Debug, Default)]
struct SavepointWireState {
    name: String,
    generation: u64,
}

/// Per-connection state.
pub struct Connection<E: PgWireEngine + 'static, R, W> {
    engine: Arc<E>,
    reader: R,
    writer: W,
    session: Option<SessionHandle>,
    txn_status: TransactionStatus,
    skip_until_sync: bool,
    /// Process ID for `BackendKeyData` (used for cancellation).
    pid: u32,
    /// Secret key for cancellation.
    secret_key: u32,
    /// Shared cancel registry for cross-connection `CancelRequest` handling.
    cancel_registry: CancelRegistry,
    /// Whether this connection is over TLS.
    tls: bool,
    /// Shared server metrics.
    metrics: Option<Arc<ServerMetrics>>,
    /// Delay applied before returning authentication failures during startup.
    auth_failure_backoff: Duration,
    /// Maximum time allowed to complete startup negotiation.
    startup_timeout: Duration,
    /// Absolute deadline for the pre-authentication handshake.
    startup_deadline: Option<tokio::time::Instant>,
    /// Best-effort peer address propagated to the engine for auth/audit decisions.
    peer_addr: Option<String>,
    /// Optional bounded blocking dispatcher for engine work.
    engine_pool: Option<EnginePool<E>>,
    /// Original Parse parameter OIDs per prepared statement, used to preserve
    /// `PostgreSQL` aliases like VARCHAR/BPCHAR in `ParameterDescription`.
    statement_wire_state: HashMap<String, StatementWireState>,
    /// Result format codes and statement ownership per portal, set during Bind.
    portal_wire_state: HashMap<String, PortalWireState>,
    /// User-visible savepoint generations tracked so pgwire can purge portal
    /// slots invalidated by `ROLLBACK TO SAVEPOINT`.
    savepoint_wire_state: Vec<SavepointWireState>,
    /// Maximum time to wait for a client message before closing the connection.
    idle_timeout: Duration,
    /// Maximum number of concurrently open portals per connection.
    max_portals: usize,
    /// Whether startup requested `PostgreSQL` replication protocol mode.
    replication_mode: bool,
    /// Database name advertised during replication handshake responses.
    replication_database: Option<String>,
    /// Startup application_name for physical replication state tracking.
    replication_application_name: Option<String>,
    /// Marks that the connection should close after the current handler returns.
    close_requested: bool,
}

impl<E, R, W> Connection<E, R, W>
where
    E: PgWireEngine + 'static,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    fn frontend_message_name(msg: &FrontendMessage) -> &'static str {
        match msg {
            FrontendMessage::Query(_) => "Query",
            FrontendMessage::Parse { .. } => "Parse",
            FrontendMessage::Bind { .. } => "Bind",
            FrontendMessage::Describe { .. } => "Describe",
            FrontendMessage::Execute { .. } => "Execute",
            FrontendMessage::Sync => "Sync",
            FrontendMessage::Flush => "Flush",
            FrontendMessage::Close { .. } => "Close",
            FrontendMessage::Terminate => "Terminate",
            FrontendMessage::Password(_) => "Password",
            FrontendMessage::CopyData(_) => "CopyData",
            FrontendMessage::CopyDone => "CopyDone",
            FrontendMessage::CopyFail(_) => "CopyFail",
        }
    }

    pub fn new(
        engine: Arc<E>,
        reader: R,
        writer: W,
        pid: u32,
        secret_key: u32,
        cancel_registry: CancelRegistry,
    ) -> Self {
        Self {
            engine,
            reader,
            writer,
            session: None,
            txn_status: TransactionStatus::Idle,
            skip_until_sync: false,
            pid,
            secret_key,
            cancel_registry,
            tls: false,
            metrics: None,
            auth_failure_backoff: Duration::ZERO,
            startup_timeout: Duration::ZERO,
            startup_deadline: None,
            peer_addr: None,
            engine_pool: None,
            statement_wire_state: HashMap::new(),
            portal_wire_state: HashMap::new(),
            savepoint_wire_state: Vec::new(),
            idle_timeout: Duration::ZERO,
            max_portals: DEFAULT_MAX_PORTALS_FALLBACK,
            replication_mode: false,
            replication_database: None,
            replication_application_name: None,
            close_requested: false,
        }
    }

    /// Attach server metrics to this connection for query counting.
    pub fn set_metrics(&mut self, metrics: Arc<ServerMetrics>) {
        self.metrics = Some(metrics);
    }

    /// Set whether this connection is using TLS.
    pub fn set_tls(&mut self, tls: bool) {
        self.tls = tls;
    }

    /// Set the startup authentication failure backoff for this connection.
    pub fn set_auth_failure_backoff(&mut self, backoff: Duration) {
        self.auth_failure_backoff = backoff;
    }

    /// Set the startup timeout for this connection. `Duration::ZERO` disables it.
    pub fn set_startup_timeout(&mut self, timeout: Duration) {
        self.startup_timeout = timeout;
    }

    /// Set an absolute startup deadline shared with outer handshake phases.
    pub fn set_startup_deadline(&mut self, deadline: Option<tokio::time::Instant>) {
        self.startup_deadline = deadline;
    }

    /// Set the idle timeout for this connection. `Duration::ZERO` disables it.
    pub fn set_idle_timeout(&mut self, timeout: Duration) {
        self.idle_timeout = timeout;
    }

    /// Attach the remote peer address for startup auth/audit decisions.
    pub fn set_peer_addr(&mut self, peer_addr: Option<String>) {
        self.peer_addr = peer_addr;
    }

    /// Attach the configured engine dispatcher used to keep blocking engine
    /// calls off the async runtime.
    pub fn set_engine_pool(&mut self, engine_pool: EnginePool<E>) {
        self.engine_pool = Some(engine_pool);
    }

    /// Override the maximum number of concurrently open portals for this
    /// connection.  When not called the fallback value (1000) is used.
    pub fn set_max_portals(&mut self, limit: usize) {
        self.max_portals = limit;
    }

    #[cfg(test)]
    pub(crate) fn writer_ref(&self) -> &W {
        &self.writer
    }

    #[allow(clippy::unused_async)]
    async fn run_engine<T, F>(&self, work: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<E>) -> DbResult<T> + Send + 'static,
    {
        if let Some(engine_pool) = &self.engine_pool {
            engine_pool.run(work).await
        } else {
            work(Arc::clone(&self.engine))
        }
    }

    /// Run the full connection lifecycle.
    ///
    /// Session cleanup always runs regardless of whether the message loop
    /// completed normally or was interrupted by a protocol/IO error.  This
    /// guarantees that any active transaction held by this session is rolled
    /// back and its locks released even when the client disconnects abruptly
    /// (TCP RST, process kill, etc.).
    pub async fn run(&mut self) -> Result<(), DbError> {
        let result = self.run_inner().await;

        // Safety-net cleanup: if `run_inner` returned an error before
        // reaching its own Phase 3 cleanup (e.g. a write error propagated
        // via `?`), we still need to terminate the session so that any
        // active transaction is rolled back and locks are released.
        self.cleanup_session().await;

        match result {
            Ok(()) => {
                debug!(pid = self.pid, "connection closed normally");
                Ok(())
            }
            Err(e) => {
                debug!(pid = self.pid, error = %e, "connection error");
                Err(e)
            }
        }
    }

    /// Unconditionally clean up this connection's session.  Safe to call
    /// `None` after the first.
    async fn cleanup_session(&mut self) {
        self.cancel_registry.unregister(self.pid, self.secret_key);
        if let Some(session) = self.session.take() {
            if let Err(e) = self
                .run_engine(move |engine| engine.terminate(session))
                .await
            {
                warn!(
                    pid = self.pid,
                    error = %e,
                    "failed to terminate session during cleanup"
                );
            }
        }
    }

    async fn run_inner(&mut self) -> Result<(), DbError> {
        // Phase 1: Startup
        self.handle_startup().await?;

        // Phase 2: Message loop
        loop {
            let raw = if self.idle_timeout.is_zero() {
                match codec::read_frontend_message(&mut self.reader).await {
                    Ok(msg) => msg,
                    Err(e) => {
                        debug!(pid = self.pid, "read error, closing: {e}");
                        break;
                    }
                }
            } else {
                match tokio::time::timeout(
                    self.idle_timeout,
                    codec::read_frontend_message(&mut self.reader),
                )
                .await
                {
                    Ok(Ok(msg)) => msg,
                    Ok(Err(e)) => {
                        debug!(pid = self.pid, "read error, closing: {e}");
                        break;
                    }
                    Err(_) => {
                        warn!(pid = self.pid, "idle timeout, closing connection");
                        break;
                    }
                }
            };

            let msg = match FrontendMessage::parse(raw.tag, raw.payload) {
                Ok(msg) => msg,
                Err(e) => {
                    warn!(pid = self.pid, "failed to parse message: {e}");
                    let requires_sync = Self::should_skip_until_sync_on_parse_error(raw.tag);
                    if requires_sync {
                        self.enter_extended_query_error_state().await?;
                    }
                    let mut w = MessageWriter::new();
                    messages::write_error_response(&mut w, &e);
                    if !requires_sync {
                        messages::write_ready_for_query(&mut w, self.txn_status);
                    }
                    w.flush(&mut self.writer).await?;
                    continue;
                }
            };
            debug!(
                pid = self.pid,
                message = Self::frontend_message_name(&msg),
                "received frontend message"
            );

            if self.replication_mode {
                match msg {
                    FrontendMessage::Query(sql) => {
                        self.handle_replication_query(&sql).await?;
                    }
                    FrontendMessage::Terminate => {
                        info!(pid = self.pid, "client sent Terminate");
                        break;
                    }
                    FrontendMessage::Flush => {
                        self.writer
                            .flush()
                            .await
                            .map_err(|e| DbError::protocol(format!("flush: {e}")))?;
                    }
                    _ => {
                        let mut w = MessageWriter::new();
                        let error =
                            DbError::protocol("unexpected frontend message in replication mode");
                        warn!(
                            pid = self.pid,
                            error = %error,
                            "unexpected frontend message in replication mode"
                        );
                        messages::write_error_response(&mut w, &error);
                        messages::write_ready_for_query(&mut w, TransactionStatus::Idle);
                        w.flush(&mut self.writer).await?;
                    }
                }
                if self.close_requested {
                    break;
                }
                continue;
            }

            if self.skip_until_sync {
                match msg {
                    FrontendMessage::Sync => {
                        self.handle_sync().await?;
                    }
                    FrontendMessage::Terminate => {
                        info!(pid = self.pid, "client sent Terminate");
                        break;
                    }
                    _ => {}
                }
                continue;
            }

            if self.txn_status == TransactionStatus::Failed
                && !self.message_allowed_in_failed_transaction(&msg)
            {
                self.write_failed_transaction_response().await?;
                continue;
            }

            match msg {
                FrontendMessage::Query(sql) => {
                    self.handle_simple_query(&sql).await?;
                }
                FrontendMessage::Parse {
                    name,
                    query,
                    param_types,
                } => {
                    self.handle_parse(&name, &query, &param_types).await?;
                }
                FrontendMessage::Bind {
                    portal,
                    statement,
                    param_formats,
                    param_values,
                    result_formats,
                } => {
                    self.handle_bind(
                        &portal,
                        &statement,
                        &param_formats,
                        &param_values,
                        &result_formats,
                    )
                    .await?;
                }
                FrontendMessage::Describe { target, name } => {
                    self.handle_describe(target, &name).await?;
                }
                FrontendMessage::Execute { portal, max_rows } => {
                    self.handle_execute(&portal, max_rows).await?;
                }
                FrontendMessage::Sync => {
                    self.handle_sync().await?;
                }
                FrontendMessage::Flush => {
                    self.writer
                        .flush()
                        .await
                        .map_err(|e| DbError::protocol(format!("flush: {e}")))?;
                }
                FrontendMessage::Close { target, name } => {
                    self.handle_close(target, &name).await?;
                }
                FrontendMessage::Terminate => {
                    info!(pid = self.pid, "client sent Terminate");
                    break;
                }
                FrontendMessage::Password(_) => {
                    warn!(
                        pid = self.pid,
                        "unexpected password message outside startup"
                    );
                }
                FrontendMessage::CopyData(_)
                | FrontendMessage::CopyDone
                | FrontendMessage::CopyFail(_) => {
                    let mut w = MessageWriter::new();
                    let error =
                        DbError::protocol("unexpected COPY message outside COPY sub-protocol");
                    warn!(
                        pid = self.pid,
                        error = %error,
                        "unexpected COPY message outside COPY sub-protocol"
                    );
                    messages::write_error_response(&mut w, &error);
                    messages::write_ready_for_query(&mut w, self.txn_status);
                    w.flush(&mut self.writer).await?;
                }
            }

            if self.close_requested {
                break;
            }
        }

        // Phase 3: Cleanup -- terminate the session (rollback any active
        // transaction, release locks, remove from session map).  The outer
        // `run()` method also calls `cleanup_session()` as a safety net, so
        // even if this code is unreachable due to an early `?` return, the
        // session will still be cleaned up.
        self.cleanup_session().await;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Simple Query Protocol
    // -----------------------------------------------------------------------

    async fn handle_simple_query(&mut self, sql: &str) -> Result<(), DbError> {
        let session = self.require_session()?.clone();

        debug!(
            pid = self.pid,
            sql_kind = %Self::sql_debug_kind(sql),
            sql_len = sql.len(),
            "simple query"
        );

        self.reset_unnamed_statement_state(&session).await?;

        let mut w = MessageWriter::new();

        if sql.trim().is_empty() {
            messages::write_empty_query_response(&mut w);
            messages::write_ready_for_query(&mut w, self.txn_status);
            w.flush(&mut self.writer).await?;
            return Ok(());
        }

        if let Some(m) = &self.metrics {
            m.total_queries.fetch_add(1, Ordering::Relaxed);
        }
        let txn_status_before = self.txn_status;
        let fast_simple_query_metadata = fast_simple_query_metadata_enabled();
        let pre_parsed_statements = if fast_simple_query_metadata {
            if simple_query_may_be_execute(sql) {
                parse_wire_metadata_statements(sql)
            } else {
                None
            }
        } else {
            parse_wire_metadata_statements(sql)
        };
        let pre_statement_wire_metadata =
            if pre_parsed_statements.as_ref().is_some_and(|statements| {
                statements.len() == 1 && is_sql_execute_statement(&statements[0])
            }) {
                if let Some(statement) = pre_parsed_statements
                    .as_ref()
                    .and_then(|statements| statements.first())
                {
                    let statement_sql =
                        statement_sql_fragment(sql, statement.span()).unwrap_or(sql.trim());
                    vec![self
                        .run_engine({
                            let session = session.clone();
                            let statement = statement.clone();
                            let statement_sql = statement_sql.to_owned();
                            move |engine| {
                                engine.sql_statement_wire_metadata(
                                    &session,
                                    &statement_sql,
                                    &statement,
                                )
                            }
                        })
                        .await
                        .ok()]
                } else {
                    Vec::<Option<SqlStatementWireMetadata>>::new()
                }
            } else {
                Vec::<Option<SqlStatementWireMetadata>>::new()
            };
        if let Some(portal_name) = simple_close_portal_target(sql) {
            if let Err(e) = self
                .materialize_deferred_portal_if_needed(&session, &portal_name)
                .await
            {
                self.refresh_txn_status_after_error().await?;
                messages::write_error_response(&mut w, &e);
                messages::write_ready_for_query(&mut w, self.txn_status);
                w.flush(&mut self.writer).await?;
                return Ok(());
            }
        }
        match self
            .run_engine({
                let session = session.clone();
                let sql = sql.to_owned();
                move |engine| engine.execute_sql(&session, &sql)
            })
            .await
        {
            Ok(results) => {
                if simple_query_may_change_prepared_desc_cache(sql) {
                    self.invalidate_prepared_desc_cache();
                }
                let parsed_statements = if should_parse_simple_query_for_wire_metadata(
                    sql,
                    &results,
                    fast_simple_query_metadata,
                ) {
                    pre_parsed_statements
                        .clone()
                        .or_else(|| parse_wire_metadata_statements(sql))
                } else {
                    None
                };
                let result_statements = parsed_statements.as_ref().map_or_else(
                    || vec![None; results.len()],
                    |statements| {
                        let mut statement_index = 0usize;
                        results
                            .iter()
                            .map(|result| {
                                if matches!(result, StatementResult::Notice { .. }) {
                                    None
                                } else {
                                    let statement = statements
                                        .get(statement_index)
                                        .cloned()
                                        .map(|statement| (statement_index, statement));
                                    if statement.is_some() {
                                        statement_index += 1;
                                    }
                                    statement
                                }
                            })
                            .collect::<Vec<_>>()
                    },
                );
                let statement_wire_metadata = if let Some(statements) = parsed_statements.as_ref() {
                    let mut metadata = Vec::with_capacity(statements.len());
                    for statement in statements {
                        let statement_sql =
                            statement_sql_fragment(sql, statement.span()).unwrap_or(sql.trim());
                        let precomputed = pre_statement_wire_metadata
                            .get(metadata.len())
                            .cloned()
                            .flatten();
                        let statement_metadata = if precomputed.is_some() {
                            precomputed
                        } else {
                            self.run_engine({
                                let session = session.clone();
                                let statement = statement.clone();
                                let statement_sql = statement_sql.to_owned();
                                move |engine| {
                                    engine.sql_statement_wire_metadata(
                                        &session,
                                        &statement_sql,
                                        &statement,
                                    )
                                }
                            })
                            .await
                            .ok()
                        };
                        metadata.push(statement_metadata);
                    }
                    metadata
                } else {
                    Vec::<Option<SqlStatementWireMetadata>>::new()
                };
                let mut should_clear_portal_wire_state = false;
                for (result, result_statement) in results.iter().zip(result_statements.iter()) {
                    let statement = result_statement.as_ref().map(|(_, statement)| statement);
                    let wire_metadata = result_statement
                        .as_ref()
                        .and_then(|(index, _)| statement_wire_metadata.get(*index))
                        .and_then(Option::as_ref);
                    // Simple protocol can stream query metadata directly from the
                    // execution result; avoid a second describe roundtrip.
                    let effective_statement = wire_metadata
                        .and_then(|metadata| metadata.effective_statement.as_ref())
                        .or(statement);
                    let statement_desc =
                        wire_metadata.and_then(|metadata| metadata.description.as_ref());
                    match result {
                        StatementResult::CopyOut { .. } => {
                            if let Err(e) = result_wire::write_statement_result(
                                &mut w,
                                result,
                                effective_statement,
                                statement_desc.map(|desc| desc.result_column_origins.as_slice()),
                            ) {
                                self.refresh_txn_status_after_error().await?;
                                error!(pid = self.pid, error = %e, "query serialization error");
                                messages::write_error_response(&mut w, &e);
                                messages::write_ready_for_query(&mut w, self.txn_status);
                                w.flush(&mut self.writer).await?;
                                return Ok(());
                            }
                        }
                        StatementResult::CopyIn {
                            table_id, columns, ..
                        } => {
                            if let Err(e) = messages::write_copy_in_response(&mut w, columns.len())
                            {
                                self.refresh_txn_status_after_error().await?;
                                error!(pid = self.pid, error = %e, "query serialization error");
                                messages::write_error_response(&mut w, &e);
                                messages::write_ready_for_query(&mut w, self.txn_status);
                                w.flush(&mut self.writer).await?;
                                return Ok(());
                            }
                            w.flush(&mut self.writer).await?;
                            w = MessageWriter::new();
                            match self.handle_copy_in_data(*table_id).await {
                                Ok(r) => {
                                    match self
                                        .run_engine({
                                            let session = session.clone();
                                            move |engine| engine.drain_pending_notices(&session)
                                        })
                                        .await
                                    {
                                        Ok(notices) => {
                                            for message in &notices {
                                                messages::write_notice_response(&mut w, message);
                                            }
                                        }
                                        Err(e) => {
                                            self.refresh_txn_status_after_error().await?;
                                            error!(
                                                pid = self.pid,
                                                error = %e,
                                                "COPY FROM notice drain error"
                                            );
                                            messages::write_error_response(&mut w, &e);
                                            messages::write_ready_for_query(
                                                &mut w,
                                                self.txn_status,
                                            );
                                            w.flush(&mut self.writer).await?;
                                            return Ok(());
                                        }
                                    }
                                    if let Err(e) =
                                        result_wire::write_copy_in_completion_result(&mut w, &r)
                                    {
                                        self.refresh_txn_status_after_error().await?;
                                        error!(
                                            pid = self.pid,
                                            error = %e,
                                            "query serialization error"
                                        );
                                        messages::write_error_response(&mut w, &e);
                                        messages::write_ready_for_query(&mut w, self.txn_status);
                                        w.flush(&mut self.writer).await?;
                                        return Ok(());
                                    }
                                }
                                Err(e) => {
                                    self.refresh_txn_status_after_error().await?;
                                    error!(pid = self.pid, error = %e, "COPY FROM error");
                                    messages::write_error_response(&mut w, &e);
                                    messages::write_ready_for_query(&mut w, self.txn_status);
                                    w.flush(&mut self.writer).await?;
                                    return Ok(());
                                }
                            }
                        }
                        _ => {
                            if let Err(e) = result_wire::write_statement_result(
                                &mut w,
                                result,
                                effective_statement,
                                statement_desc.map(|desc| desc.result_column_origins.as_slice()),
                            ) {
                                self.refresh_txn_status_after_error().await?;
                                error!(pid = self.pid, error = %e, "query serialization error");
                                messages::write_error_response(&mut w, &e);
                                messages::write_ready_for_query(&mut w, self.txn_status);
                                w.flush(&mut self.writer).await?;
                                return Ok(());
                            }
                        }
                    }
                }
                if results.is_empty() {
                    messages::write_empty_query_response(&mut w);
                }
                let can_assume_idle_after_success =
                    if !matches!(txn_status_before, TransactionStatus::Idle) {
                        false
                    } else if let Some(statements) = parsed_statements.as_ref() {
                        statements.len() == 1
                            && !Self::statement_opens_explicit_transaction(&statements[0])
                    } else {
                        // Fast path for simple-protocol autocommit SELECT:
                        // avoid an extra engine roundtrip (`has_active_transaction`)
                        // when we can safely infer ReadyForQuery='I'.
                        results.len() == 1
                            && matches!(results[0], StatementResult::Query { .. })
                            && first_sql_keyword(sql)
                                .is_some_and(|keyword| keyword.eq_ignore_ascii_case("SELECT"))
                    };
                if can_assume_idle_after_success {
                    self.txn_status = TransactionStatus::Idle;
                } else {
                    self.refresh_txn_status_after_success().await?;
                }
                if let Some(statements) = parsed_statements.as_ref() {
                    for (statement_index, statement) in statements.iter().enumerate() {
                        let wire_metadata = statement_wire_metadata
                            .get(statement_index)
                            .and_then(Option::as_ref);
                        if let Some(hint) =
                            wire_metadata.and_then(|metadata| metadata.cleanup_hint.as_ref())
                        {
                            self.apply_wire_state_cleanup(&hint);
                        }
                        if wire_metadata.is_some_and(|metadata| metadata.changes_result_metadata) {
                            self.invalidate_prepared_desc_cache();
                        }
                        let effective_statement = wire_metadata
                            .and_then(|metadata| metadata.effective_statement.as_ref())
                            .unwrap_or(statement);
                        if !matches!(txn_status_before, TransactionStatus::Idle)
                            && matches!(
                                effective_statement,
                                aiondb_parser::Statement::Commit { .. }
                                    | aiondb_parser::Statement::Rollback { .. }
                            )
                        {
                            should_clear_portal_wire_state = true;
                        }
                        if let Err(e) = self
                            .sync_savepoint_wire_state_after_success(&session, effective_statement)
                            .await
                        {
                            self.refresh_txn_status_after_error().await?;
                            error!(
                                pid = self.pid,
                                error = %e,
                                "simple query savepoint wire-state sync error"
                            );
                            messages::write_error_response(&mut w, &e);
                            messages::write_ready_for_query(&mut w, self.txn_status);
                            w.flush(&mut self.writer).await?;
                            return Ok(());
                        }
                    }
                }
                if should_clear_portal_wire_state {
                    self.portal_wire_state.clear();
                    self.savepoint_wire_state.clear();
                }
                messages::write_ready_for_query(&mut w, self.txn_status);
            }
            Err(e) => {
                self.refresh_txn_status_after_error().await?;
                error!(pid = self.pid, error = %e, "query error");
                messages::write_error_response(&mut w, &e);
                messages::write_ready_for_query(&mut w, self.txn_status);
            }
        }

        w.flush(&mut self.writer).await?;
        Ok(())
    }

    fn should_skip_until_sync_on_parse_error(tag: u8) -> bool {
        matches!(tag, b'P' | b'B' | b'D' | b'E' | b'C')
    }

    fn sql_debug_kind(sql: &str) -> &str {
        first_sql_keyword(sql).unwrap_or("<empty>")
    }

    fn statement_opens_explicit_transaction(statement: &aiondb_parser::Statement) -> bool {
        matches!(statement, aiondb_parser::Statement::Begin { .. })
    }

    fn require_session(&self) -> Result<&SessionHandle, DbError> {
        self.session
            .as_ref()
            .ok_or_else(|| DbError::protocol("no active session"))
    }

    async fn handle_sync(&mut self) -> Result<(), DbError> {
        self.skip_until_sync = false;
        if !matches!(self.txn_status, TransactionStatus::Idle) {
            self.txn_status = self.ready_for_query_status().await?;
        }
        let mut w = MessageWriter::new();
        messages::write_ready_for_query(&mut w, self.txn_status);
        w.flush(&mut self.writer).await?;
        Ok(())
    }

    fn apply_wire_state_cleanup(&mut self, hint: &WireStateCleanupHint) {
        match hint {
            WireStateCleanupHint::DeallocateAll => {
                self.statement_wire_state.retain(|name, _| name.is_empty());
                self.portal_wire_state
                    .retain(|_, state| state.statement_name.is_empty());
            }
            WireStateCleanupHint::DeallocateName(statement_name) => {
                self.statement_wire_state.remove(statement_name);
                self.portal_wire_state
                    .retain(|_, portal| portal.statement_name != *statement_name);
            }
            WireStateCleanupHint::ClosePortal(portal_name) => {
                self.portal_wire_state.remove(portal_name);
            }
        }
    }

    fn invalidate_prepared_desc_cache(&mut self) {
        for state in self.statement_wire_state.values_mut() {
            state.prepared_desc = None;
            state.deferred_describe_response_cache.clear();
        }
        for portal in self.portal_wire_state.values_mut() {
            portal.deferred_describe_response = None;
        }
    }
}

fn is_sql_execute_statement(statement: &aiondb_parser::Statement) -> bool {
    match statement {
        aiondb_parser::Statement::ExecuteStmt { .. } => true,
        aiondb_parser::Statement::CompatParserStub { tag, .. } => tag == "EXECUTE",
        _ => false,
    }
}

fn parse_wire_metadata_statements(sql: &str) -> Option<Vec<aiondb_parser::Statement>> {
    aiondb_parser::parse_sql(sql).ok().or_else(|| {
        aiondb_parser::parse_prepared_statement(sql)
            .ok()
            .map(|stmt| vec![stmt])
    })
}

fn statement_sql_fragment(sql: &str, span: aiondb_parser::Span) -> Option<&str> {
    let mut end = span.end.min(sql.len());
    if let Some(suffix) = sql.get(end..) {
        if let Some(semicolon_offset) = suffix.find(';') {
            end += semicolon_offset;
        } else {
            end = sql.len();
        }
    }
    sql.get(span.start..end).map(str::trim)
}

fn fast_simple_query_metadata_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("AIONDB_PGWIRE_FAST_SIMPLE_QUERY_METADATA").is_some())
}

fn simple_query_may_be_execute(sql: &str) -> bool {
    first_sql_keyword(sql).is_some_and(|keyword| keyword.eq_ignore_ascii_case("execute"))
}

fn simple_close_portal_target(sql: &str) -> Option<String> {
    if !first_sql_keyword(sql).is_some_and(|keyword| keyword.eq_ignore_ascii_case("close")) {
        return None;
    }
    let mut parts = sql.trim().split_ascii_whitespace();
    parts.next()?;
    let target = parts.next()?.trim_end_matches(';');
    if target.eq_ignore_ascii_case("all") {
        None
    } else {
        Some(target.trim_matches('"').to_owned())
    }
}

fn simple_query_may_change_prepared_desc_cache(sql: &str) -> bool {
    first_sql_keyword(sql).is_some_and(|keyword| {
        matches!(
            keyword.to_ascii_lowercase().as_str(),
            "alter"
                | "close"
                | "create"
                | "deallocate"
                | "declare"
                | "drop"
                | "prepare"
                | "reset"
                | "set"
        )
    })
}

fn should_parse_simple_query_for_wire_metadata(
    sql: &str,
    results: &[StatementResult],
    fast_simple_query_metadata: bool,
) -> bool {
    if results.len() != 1 {
        return true;
    }
    if !fast_simple_query_metadata {
        return !matches!(results[0], StatementResult::Notice { .. });
    }
    if matches!(results[0], StatementResult::Notice { .. }) {
        return false;
    }
    if !matches!(results[0], StatementResult::Query { .. }) {
        return false;
    }
    first_sql_keyword(sql).is_some_and(|keyword| {
        matches!(
            keyword.to_ascii_lowercase().as_str(),
            "execute" | "explain" | "show" | "fetch" | "insert" | "update" | "delete"
        )
    })
}

fn first_sql_keyword(sql: &str) -> Option<&str> {
    let sql = sql.trim_start();
    let mut end = 0usize;
    for ch in sql.chars() {
        if ch.is_ascii_alphabetic() {
            end += ch.len_utf8();
        } else {
            break;
        }
    }
    (end > 0).then(|| &sql[..end])
}

fn checked_deadline_after(timeout: Duration, timeout_name: &str) -> Option<tokio::time::Instant> {
    if timeout.is_zero() {
        return None;
    }
    match tokio::time::Instant::now().checked_add(timeout) {
        Some(deadline) => Some(deadline),
        None => {
            warn!(
                timeout_name,
                timeout_ms = timeout.as_millis(),
                "timeout too large to schedule safely; disabling deadline"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests;
