use super::*;
use crate::bind::coerce_bind_value;
use crate::server::ServerMetrics;
use aiondb_core::{DatabaseId, Value};
use aiondb_engine::{
    engine::streaming::{ReplicationManager, StreamingReplicationState},
    AuthenticatedIdentity, Credential, DbResult, IsolationLevel, PortalBatch, PortalDescription,
    PreparedStatementDesc, QueryEngine, ReplicationIdentity, ResultColumn, Row, ScramVerifier,
    SecretBytes, SessionHandle, SessionInfo, SessionLimits, StartupAuthentication, StatementResult,
};
use aiondb_wal::replication::{ReplicaRegistry, ReplicationMessage, WalNotifier};
use aiondb_wal::{Lsn, WalConfig, WalRecord, WalWriter};
use base64::Engine as _;
use bytes::BytesMut;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::{
    io::Cursor,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{duplex, split, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

mod audit_0day;
mod lifecycle;
mod replication;
mod unit;

fn unique_temp_test_dir(scope: &str, name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aiondb-pgwire-{scope}-{name}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("test temp dir should be creatable");
    dir
}

// Minimal mock engine for connection tests.
struct MockEngine {
    active_txn: Mutex<bool>,
    startup_auth_error: Option<DbError>,
    startup_error: Option<DbError>,
    last_transport: Mutex<Option<TransportInfo>>,
    bind_calls: Mutex<usize>,
    describe_statement_calls: Mutex<usize>,
    block_queries_until_cancel: bool,
    cancel_requested: AtomicBool,
}

impl MockEngine {
    fn new() -> Self {
        Self {
            active_txn: Mutex::new(false),
            startup_auth_error: None,
            startup_error: None,
            last_transport: Mutex::new(None),
            bind_calls: Mutex::new(0),
            describe_statement_calls: Mutex::new(0),
            block_queries_until_cancel: false,
            cancel_requested: AtomicBool::new(false),
        }
    }

    fn with_startup_auth_error(error: DbError) -> Self {
        Self {
            active_txn: Mutex::new(false),
            startup_auth_error: Some(error),
            startup_error: None,
            last_transport: Mutex::new(None),
            bind_calls: Mutex::new(0),
            describe_statement_calls: Mutex::new(0),
            block_queries_until_cancel: false,
            cancel_requested: AtomicBool::new(false),
        }
    }

    fn with_startup_error(error: DbError) -> Self {
        Self {
            active_txn: Mutex::new(false),
            startup_auth_error: None,
            startup_error: Some(error),
            last_transport: Mutex::new(None),
            bind_calls: Mutex::new(0),
            describe_statement_calls: Mutex::new(0),
            block_queries_until_cancel: false,
            cancel_requested: AtomicBool::new(false),
        }
    }

    fn with_blocking_query() -> Self {
        Self {
            active_txn: Mutex::new(false),
            startup_auth_error: None,
            startup_error: None,
            last_transport: Mutex::new(None),
            bind_calls: Mutex::new(0),
            describe_statement_calls: Mutex::new(0),
            block_queries_until_cancel: true,
            cancel_requested: AtomicBool::new(false),
        }
    }

    fn last_transport(&self) -> Option<TransportInfo> {
        self.last_transport.lock().expect("transport state").clone()
    }

    fn bind_calls(&self) -> usize {
        *self.bind_calls.lock().expect("bind calls")
    }

    fn describe_statement_calls(&self) -> usize {
        *self
            .describe_statement_calls
            .lock()
            .expect("describe statement calls")
    }

    fn dummy_session_info() -> SessionInfo {
        SessionInfo {
            identity: AuthenticatedIdentity {
                user: "test".to_string(),
                database_id: DatabaseId::new(0),
                roles: vec!["test".to_string()],
            },
            is_superuser: false,
            limits: SessionLimits::default(),
            database_name: "test".to_string(),
            active_database: aiondb_cluster::DatabaseId::DEFAULT,
        }
    }
}

impl QueryEngine for MockEngine {
    fn requires_password(&self) -> bool {
        false
    }
    fn startup_authentication(
        &self,
        _user: &str,
        _database: &str,
        _transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication> {
        if let Some(error) = &self.startup_auth_error {
            return Err(error.clone());
        }
        Ok(StartupAuthentication::Trust)
    }
    fn startup(&self, params: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
        *self.last_transport.lock().expect("transport state") = Some(params.transport.clone());
        if let Some(error) = &self.startup_error {
            return Err(error.clone());
        }
        Ok((SessionHandle::test_handle(), Self::dummy_session_info()))
    }
    fn has_active_transaction(&self, _session: &SessionHandle) -> DbResult<bool> {
        Ok(*self.active_txn.lock().expect("txn state"))
    }
    fn begin_transaction(
        &self,
        _session: &SessionHandle,
        _isolation: IsolationLevel,
    ) -> DbResult<()> {
        *self.active_txn.lock().expect("txn state") = true;
        Ok(())
    }
    fn commit_transaction(&self, _session: &SessionHandle) -> DbResult<()> {
        *self.active_txn.lock().expect("txn state") = false;
        Ok(())
    }
    fn rollback_transaction(&self, _session: &SessionHandle) -> DbResult<()> {
        *self.active_txn.lock().expect("txn state") = false;
        Ok(())
    }
    fn execute_sql(&self, _session: &SessionHandle, sql: &str) -> DbResult<Vec<StatementResult>> {
        let upper = sql.trim().to_uppercase();
        if self.block_queries_until_cancel && upper.starts_with("BLOCK") {
            for _ in 0..500 {
                if self.cancel_requested.load(Ordering::Relaxed) {
                    return Err(DbError::query_canceled("session canceled"));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            return Err(DbError::protocol("blocking query was not canceled"));
        }
        if upper.starts_with("BEGIN") {
            *self.active_txn.lock().expect("txn state") = true;
            Ok(vec![StatementResult::Command {
                tag: "BEGIN".to_string(),
                rows_affected: 0,
            }])
        } else if upper.starts_with("COMMIT") {
            *self.active_txn.lock().expect("txn state") = false;
            Ok(vec![StatementResult::Command {
                tag: "COMMIT".to_string(),
                rows_affected: 0,
            }])
        } else if upper.starts_with("ROLLBACK") {
            *self.active_txn.lock().expect("txn state") = false;
            Ok(vec![StatementResult::Command {
                tag: "ROLLBACK".to_string(),
                rows_affected: 0,
            }])
        } else if upper.starts_with("ERROR") {
            Err(DbError::protocol("mock error"))
        } else if upper.starts_with("NOTICE") {
            Ok(vec![StatementResult::Notice {
                message: "compatibility notice".to_string(),
            }])
        } else {
            Ok(vec![StatementResult::Query {
                columns: vec![ResultColumn {
                    name: "col".to_string(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                }],
                rows: vec![],
            }])
        }
    }
    fn prepare(
        &self,
        _session: &SessionHandle,
        _name: String,
        sql: String,
    ) -> DbResult<PreparedStatementDesc> {
        if sql.trim().to_uppercase().starts_with("ERROR") {
            return Err(DbError::protocol("mock parse error"));
        }
        Ok(PreparedStatementDesc {
            name: String::new(),
            param_types: vec![],
            result_columns: vec![],
            result_column_origins: vec![],
        })
    }
    fn describe_statement(
        &self,
        _session: &SessionHandle,
        _name: &str,
    ) -> DbResult<PreparedStatementDesc> {
        *self
            .describe_statement_calls
            .lock()
            .expect("describe statement calls") += 1;
        Ok(PreparedStatementDesc {
            name: String::new(),
            param_types: vec![],
            result_columns: vec![],
            result_column_origins: vec![],
        })
    }
    fn bind(
        &self,
        _session: &SessionHandle,
        _portal: String,
        _stmt: String,
        _params: Vec<Value>,
    ) -> DbResult<()> {
        *self.bind_calls.lock().expect("bind calls") += 1;
        Ok(())
    }
    fn describe_portal(
        &self,
        _session: &SessionHandle,
        _name: &str,
    ) -> DbResult<PortalDescription> {
        Ok(PortalDescription {
            result_columns: vec![],
            result_column_origins: vec![],
        })
    }
    fn execute_portal(
        &self,
        _session: &SessionHandle,
        _name: &str,
        _max: usize,
    ) -> DbResult<PortalBatch> {
        Ok(PortalBatch {
            columns: vec![],
            rows: vec![],
            tag: "SELECT".to_string(),
            rows_affected: 0,
            exhausted: true,
        })
    }
    fn close_statement(&self, _session: &SessionHandle, _name: &str) -> DbResult<()> {
        Ok(())
    }
    fn close_portal(&self, _session: &SessionHandle, _name: &str) -> DbResult<()> {
        Ok(())
    }
    fn execute_copy_from(
        &self,
        _session: &SessionHandle,
        _table_id: aiondb_core::RelationId,
        _data: &str,
    ) -> DbResult<StatementResult> {
        Ok(StatementResult::Command {
            tag: "COPY".to_owned(),
            rows_affected: 0,
        })
    }
    fn cancel_session(&self, _session: &SessionHandle) -> DbResult<()> {
        self.cancel_requested.store(true, Ordering::Relaxed);
        Ok(())
    }
    fn terminate(&self, _session: SessionHandle) -> DbResult<()> {
        Ok(())
    }
    fn session_count(&self) -> DbResult<usize> {
        Ok(0)
    }
}

struct ReplicationTestEngine {
    inner: MockEngine,
    manager: Arc<ReplicationManager>,
    identity: ReplicationIdentity,
    replication_allowed: bool,
}

impl ReplicationTestEngine {
    fn new(wal_dir: PathBuf, system_identifier: &str) -> Self {
        let registry = Arc::new(ReplicaRegistry::new());
        let notifier = Arc::new(WalNotifier::new(Lsn::ZERO));
        let state = StreamingReplicationState::new_primary_shared(
            wal_dir,
            aiondb_config::ReplicationConfig {
                role: aiondb_config::ReplicationRole::Primary,
                ..Default::default()
            },
            registry,
            notifier,
            1,
        );
        Self {
            inner: MockEngine::new(),
            manager: Arc::new(ReplicationManager::new(Arc::new(state))),
            identity: ReplicationIdentity {
                system_identifier: system_identifier.to_owned(),
                timeline: 1,
            },
            replication_allowed: true,
        }
    }

    fn without_replication_privilege(mut self) -> Self {
        self.replication_allowed = false;
        self
    }
}

impl QueryEngine for ReplicationTestEngine {
    fn startup_authentication(
        &self,
        user: &str,
        database: &str,
        transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication> {
        <MockEngine as QueryEngine>::startup_authentication(&self.inner, user, database, transport)
    }

    fn replication_manager(&self) -> Option<Arc<ReplicationManager>> {
        Some(Arc::clone(&self.manager))
    }

    fn replication_identity(&self) -> Option<ReplicationIdentity> {
        Some(self.identity.clone())
    }

    fn authorize_replication_connection(
        &self,
        _session: &SessionHandle,
        _info: &SessionInfo,
    ) -> DbResult<()> {
        if self.replication_allowed {
            Ok(())
        } else {
            Err(DbError::insufficient_privilege(
                "must be superuser to use replication mode",
            ))
        }
    }

    fn startup(
        &self,
        params: aiondb_engine::StartupParams,
    ) -> DbResult<(SessionHandle, SessionInfo)> {
        <MockEngine as QueryEngine>::startup(&self.inner, params)
    }

    fn has_active_transaction(&self, session: &SessionHandle) -> DbResult<bool> {
        <MockEngine as QueryEngine>::has_active_transaction(&self.inner, session)
    }

    fn begin_transaction(
        &self,
        session: &SessionHandle,
        isolation: IsolationLevel,
    ) -> DbResult<()> {
        <MockEngine as QueryEngine>::begin_transaction(&self.inner, session, isolation)
    }

    fn commit_transaction(&self, session: &SessionHandle) -> DbResult<()> {
        <MockEngine as QueryEngine>::commit_transaction(&self.inner, session)
    }

    fn rollback_transaction(&self, session: &SessionHandle) -> DbResult<()> {
        <MockEngine as QueryEngine>::rollback_transaction(&self.inner, session)
    }

    fn execute_sql(&self, session: &SessionHandle, sql: &str) -> DbResult<Vec<StatementResult>> {
        <MockEngine as QueryEngine>::execute_sql(&self.inner, session, sql)
    }

    fn prepare(
        &self,
        session: &SessionHandle,
        statement_name: String,
        sql: String,
    ) -> DbResult<PreparedStatementDesc> {
        <MockEngine as QueryEngine>::prepare(&self.inner, session, statement_name, sql)
    }

    fn describe_statement(
        &self,
        session: &SessionHandle,
        statement_name: &str,
    ) -> DbResult<PreparedStatementDesc> {
        <MockEngine as QueryEngine>::describe_statement(&self.inner, session, statement_name)
    }

    fn bind(
        &self,
        session: &SessionHandle,
        portal_name: String,
        statement_name: String,
        params: Vec<Value>,
    ) -> DbResult<()> {
        <MockEngine as QueryEngine>::bind(&self.inner, session, portal_name, statement_name, params)
    }

    fn describe_portal(
        &self,
        session: &SessionHandle,
        portal_name: &str,
    ) -> DbResult<PortalDescription> {
        <MockEngine as QueryEngine>::describe_portal(&self.inner, session, portal_name)
    }

    fn execute_portal(
        &self,
        session: &SessionHandle,
        portal_name: &str,
        max_rows: usize,
    ) -> DbResult<PortalBatch> {
        <MockEngine as QueryEngine>::execute_portal(&self.inner, session, portal_name, max_rows)
    }

    fn close_statement(&self, session: &SessionHandle, statement_name: &str) -> DbResult<()> {
        <MockEngine as QueryEngine>::close_statement(&self.inner, session, statement_name)
    }

    fn close_portal(&self, session: &SessionHandle, portal_name: &str) -> DbResult<()> {
        <MockEngine as QueryEngine>::close_portal(&self.inner, session, portal_name)
    }

    fn execute_copy_from(
        &self,
        session: &SessionHandle,
        table_id: aiondb_core::RelationId,
        data: &str,
    ) -> DbResult<StatementResult> {
        <MockEngine as QueryEngine>::execute_copy_from(&self.inner, session, table_id, data)
    }

    fn cancel_session(&self, session: &SessionHandle) -> DbResult<()> {
        <MockEngine as QueryEngine>::cancel_session(&self.inner, session)
    }

    fn terminate(&self, session: SessionHandle) -> DbResult<()> {
        <MockEngine as QueryEngine>::terminate(&self.inner, session)
    }

    fn session_count(&self) -> DbResult<usize> {
        <MockEngine as QueryEngine>::session_count(&self.inner)
    }
}

struct ScramMockEngine {
    verifier: ScramVerifier,
    proof_token: Vec<u8>,
}

impl ScramMockEngine {
    fn new(verifier: ScramVerifier, proof_token: Vec<u8>) -> Self {
        Self {
            verifier,
            proof_token,
        }
    }
}

impl QueryEngine for ScramMockEngine {
    fn startup_authentication(
        &self,
        _user: &str,
        _database: &str,
        _transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication> {
        Ok(StartupAuthentication::ScramSha256 {
            verifier: self.verifier.clone(),
            proof_token: SecretBytes::new(self.proof_token.clone()),
        })
    }

    fn startup(&self, params: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
        match params.credential {
            Credential::Token { user, token }
                if user == "test" && token.as_bytes() == self.proof_token.as_slice() =>
            {
                Ok((
                    SessionHandle::test_handle(),
                    MockEngine::dummy_session_info(),
                ))
            }
            _ => Err(DbError::invalid_authorization("invalid SCRAM proof token")),
        }
    }

    fn has_active_transaction(&self, _session: &SessionHandle) -> DbResult<bool> {
        Ok(false)
    }

    fn begin_transaction(
        &self,
        _session: &SessionHandle,
        _isolation: IsolationLevel,
    ) -> DbResult<()> {
        Ok(())
    }

    fn commit_transaction(&self, _session: &SessionHandle) -> DbResult<()> {
        Ok(())
    }

    fn rollback_transaction(&self, _session: &SessionHandle) -> DbResult<()> {
        Ok(())
    }

    fn execute_sql(&self, _session: &SessionHandle, _sql: &str) -> DbResult<Vec<StatementResult>> {
        Ok(vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "col".to_string(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![],
        }])
    }

    fn prepare(
        &self,
        _session: &SessionHandle,
        _name: String,
        _sql: String,
    ) -> DbResult<PreparedStatementDesc> {
        Ok(PreparedStatementDesc {
            name: String::new(),
            param_types: vec![],
            result_columns: vec![],
            result_column_origins: vec![],
        })
    }

    fn describe_statement(
        &self,
        _session: &SessionHandle,
        _name: &str,
    ) -> DbResult<PreparedStatementDesc> {
        Ok(PreparedStatementDesc {
            name: String::new(),
            param_types: vec![],
            result_columns: vec![],
            result_column_origins: vec![],
        })
    }

    fn bind(
        &self,
        _session: &SessionHandle,
        _portal: String,
        _stmt: String,
        _params: Vec<Value>,
    ) -> DbResult<()> {
        Ok(())
    }

    fn describe_portal(
        &self,
        _session: &SessionHandle,
        _name: &str,
    ) -> DbResult<PortalDescription> {
        Ok(PortalDescription {
            result_columns: vec![],
            result_column_origins: vec![],
        })
    }

    fn execute_portal(
        &self,
        _session: &SessionHandle,
        _name: &str,
        _max: usize,
    ) -> DbResult<PortalBatch> {
        Ok(PortalBatch {
            columns: vec![],
            rows: vec![],
            tag: "SELECT".to_string(),
            rows_affected: 0,
            exhausted: true,
        })
    }

    fn close_statement(&self, _session: &SessionHandle, _name: &str) -> DbResult<()> {
        Ok(())
    }

    fn close_portal(&self, _session: &SessionHandle, _name: &str) -> DbResult<()> {
        Ok(())
    }

    fn execute_copy_from(
        &self,
        _session: &SessionHandle,
        _table_id: aiondb_core::RelationId,
        _data: &str,
    ) -> DbResult<StatementResult> {
        Ok(StatementResult::Command {
            tag: "COPY".to_owned(),
            rows_affected: 0,
        })
    }

    fn cancel_session(&self, _session: &SessionHandle) -> DbResult<()> {
        Ok(())
    }

    fn terminate(&self, _session: SessionHandle) -> DbResult<()> {
        Ok(())
    }

    fn session_count(&self) -> DbResult<usize> {
        Ok(0)
    }
}

struct CleartextMockEngine {
    password: &'static str,
}

impl CleartextMockEngine {
    fn new(password: &'static str) -> Self {
        Self { password }
    }
}

impl QueryEngine for CleartextMockEngine {
    fn startup_authentication(
        &self,
        _user: &str,
        _database: &str,
        _transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication> {
        Ok(StartupAuthentication::CleartextPassword)
    }

    fn startup(&self, params: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
        match params.credential {
            Credential::CleartextPassword { user, password }
                if user == "test" && password.as_str() == self.password =>
            {
                Ok((
                    SessionHandle::test_handle(),
                    MockEngine::dummy_session_info(),
                ))
            }
            _ => Err(DbError::invalid_authorization("invalid cleartext password")),
        }
    }

    fn has_active_transaction(&self, _session: &SessionHandle) -> DbResult<bool> {
        Ok(false)
    }

    fn begin_transaction(
        &self,
        _session: &SessionHandle,
        _isolation: IsolationLevel,
    ) -> DbResult<()> {
        Ok(())
    }

    fn commit_transaction(&self, _session: &SessionHandle) -> DbResult<()> {
        Ok(())
    }

    fn rollback_transaction(&self, _session: &SessionHandle) -> DbResult<()> {
        Ok(())
    }

    fn execute_sql(&self, _session: &SessionHandle, _sql: &str) -> DbResult<Vec<StatementResult>> {
        Ok(vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "col".to_string(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![],
        }])
    }

    fn prepare(
        &self,
        _session: &SessionHandle,
        _name: String,
        _sql: String,
    ) -> DbResult<PreparedStatementDesc> {
        Ok(PreparedStatementDesc {
            name: String::new(),
            param_types: vec![],
            result_columns: vec![],
            result_column_origins: vec![],
        })
    }

    fn describe_statement(
        &self,
        _session: &SessionHandle,
        _name: &str,
    ) -> DbResult<PreparedStatementDesc> {
        Ok(PreparedStatementDesc {
            name: String::new(),
            param_types: vec![],
            result_columns: vec![],
            result_column_origins: vec![],
        })
    }

    fn bind(
        &self,
        _session: &SessionHandle,
        _portal: String,
        _stmt: String,
        _params: Vec<Value>,
    ) -> DbResult<()> {
        Ok(())
    }

    fn describe_portal(
        &self,
        _session: &SessionHandle,
        _name: &str,
    ) -> DbResult<PortalDescription> {
        Ok(PortalDescription {
            result_columns: vec![],
            result_column_origins: vec![],
        })
    }

    fn execute_portal(
        &self,
        _session: &SessionHandle,
        _name: &str,
        _max: usize,
    ) -> DbResult<PortalBatch> {
        Ok(PortalBatch {
            columns: vec![],
            rows: vec![],
            tag: "SELECT".to_string(),
            rows_affected: 0,
            exhausted: true,
        })
    }

    fn close_statement(&self, _session: &SessionHandle, _name: &str) -> DbResult<()> {
        Ok(())
    }

    fn close_portal(&self, _session: &SessionHandle, _name: &str) -> DbResult<()> {
        Ok(())
    }

    fn execute_copy_from(
        &self,
        _session: &SessionHandle,
        _table_id: aiondb_core::RelationId,
        _data: &str,
    ) -> DbResult<StatementResult> {
        Ok(StatementResult::Command {
            tag: "COPY".to_owned(),
            rows_affected: 0,
        })
    }

    fn cancel_session(&self, _session: &SessionHandle) -> DbResult<()> {
        Ok(())
    }

    fn terminate(&self, _session: SessionHandle) -> DbResult<()> {
        Ok(())
    }

    fn session_count(&self) -> DbResult<usize> {
        Ok(0)
    }
}

/// Helper: build a startup message (v3) for the mock engine.
fn build_startup_bytes() -> Vec<u8> {
    build_startup_bytes_with_params(&[("user", "test")])
}

fn build_startup_bytes_with_params(params: &[(&str, &str)]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&codec::PROTOCOL_V3.to_be_bytes());
    for (key, value) in params {
        payload.extend_from_slice(key.as_bytes());
        payload.push(0);
        payload.extend_from_slice(value.as_bytes());
        payload.push(0);
    }
    payload.push(0);
    let len = (payload.len() as u32) + 4;
    let mut data = Vec::new();
    data.extend_from_slice(&len.to_be_bytes());
    data.extend_from_slice(&payload);
    data
}

fn build_startup_bytes_with_user(user: &str) -> Vec<u8> {
    build_startup_bytes_with_params(&[("user", user)])
}

/// Helper: build a simple query message.
fn build_query_bytes(sql: &str) -> Vec<u8> {
    let mut data = Vec::new();
    data.push(b'Q');
    let payload_len = sql.len() + 1; // +1 for null terminator
    let msg_len = (payload_len as u32) + 4;
    data.extend_from_slice(&msg_len.to_be_bytes());
    data.extend_from_slice(sql.as_bytes());
    data.push(0);
    data
}

/// Helper: build a Terminate message.
fn build_terminate_bytes() -> Vec<u8> {
    let mut data = Vec::new();
    data.push(b'X');
    data.extend_from_slice(&4u32.to_be_bytes());
    data
}

fn build_raw_message(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    data.push(tag);
    let msg_len = (payload.len() as u32) + 4;
    data.extend_from_slice(&msg_len.to_be_bytes());
    data.extend_from_slice(payload);
    data
}

fn build_sasl_initial_response_bytes(mechanism: &str, initial_response: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(mechanism.as_bytes());
    payload.push(0);
    payload.extend_from_slice(&(initial_response.len() as i32).to_be_bytes());
    payload.extend_from_slice(initial_response.as_bytes());
    build_raw_message(b'p', &payload)
}

fn build_sasl_response_bytes(response: &str) -> Vec<u8> {
    build_raw_message(b'p', response.as_bytes())
}

fn build_parse_bytes(stmt_name: &str, query: &str, param_oids: &[u32]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(stmt_name.as_bytes());
    payload.push(0);
    payload.extend_from_slice(query.as_bytes());
    payload.push(0);
    payload.extend_from_slice(&(param_oids.len() as i16).to_be_bytes());
    for &oid in param_oids {
        payload.extend_from_slice(&oid.to_be_bytes());
    }
    build_raw_message(b'P', &payload)
}

fn build_bind_bytes(
    portal: &str,
    statement: &str,
    param_formats: &[i16],
    param_values: &[Option<&[u8]>],
    result_formats: &[i16],
) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(portal.as_bytes());
    payload.push(0);
    payload.extend_from_slice(statement.as_bytes());
    payload.push(0);
    payload.extend_from_slice(&(param_formats.len() as i16).to_be_bytes());
    for &format in param_formats {
        payload.extend_from_slice(&format.to_be_bytes());
    }
    payload.extend_from_slice(&(param_values.len() as i16).to_be_bytes());
    for value in param_values {
        match value {
            None => payload.extend_from_slice(&(-1_i32).to_be_bytes()),
            Some(value) => {
                payload.extend_from_slice(&(value.len() as i32).to_be_bytes());
                payload.extend_from_slice(value);
            }
        }
    }
    payload.extend_from_slice(&(result_formats.len() as i16).to_be_bytes());
    for &format in result_formats {
        payload.extend_from_slice(&format.to_be_bytes());
    }
    build_raw_message(b'B', &payload)
}

fn build_execute_bytes(portal: &str, max_rows: i32) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(portal.as_bytes());
    payload.push(0);
    payload.extend_from_slice(&max_rows.to_be_bytes());
    build_raw_message(b'E', &payload)
}

fn build_sync_bytes() -> Vec<u8> {
    build_raw_message(b'S', &[])
}

struct BackendMessage {
    tag: u8,
    payload: BytesMut,
}

async fn read_backend_message<S>(stream: &mut S) -> DbResult<BackendMessage>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut tag = [0u8; 1];
    stream
        .read_exact(&mut tag)
        .await
        .map_err(|error| DbError::protocol(format!("read backend tag: {error}")))?;

    let mut len = [0u8; 4];
    stream
        .read_exact(&mut len)
        .await
        .map_err(|error| DbError::protocol(format!("read backend length: {error}")))?;
    let payload_len = u32::from_be_bytes(len) as usize - 4;
    let mut payload = BytesMut::zeroed(payload_len);
    if payload_len > 0 {
        stream
            .read_exact(&mut payload)
            .await
            .map_err(|error| DbError::protocol(format!("read backend payload: {error}")))?;
    }
    Ok(BackendMessage {
        tag: tag[0],
        payload,
    })
}

fn parse_auth_request(mut payload: BytesMut) -> DbResult<i32> {
    codec::read_i32_from_buf(&mut payload)
}

fn parse_auth_sasl_continue(mut payload: BytesMut) -> DbResult<String> {
    let auth_type = codec::read_i32_from_buf(&mut payload)?;
    if auth_type != 11 {
        return Err(DbError::protocol(format!(
            "expected AuthenticationSASLContinue, got auth type {auth_type}"
        )));
    }
    String::from_utf8(payload.to_vec())
        .map_err(|_| DbError::protocol("invalid UTF-8 in SASL continue payload"))
}

fn parse_auth_sasl_final(mut payload: BytesMut) -> DbResult<String> {
    let auth_type = codec::read_i32_from_buf(&mut payload)?;
    if auth_type != 12 {
        return Err(DbError::protocol(format!(
            "expected AuthenticationSASLFinal, got auth type {auth_type}"
        )));
    }
    String::from_utf8(payload.to_vec())
        .map_err(|_| DbError::protocol("invalid UTF-8 in SASL final payload"))
}

fn build_scram_client_final(password: &str, client_first: &str, server_first: &str) -> String {
    let combined_nonce = server_first
        .split(',')
        .find_map(|part| part.strip_prefix("r="))
        .expect("server-first nonce");
    let salt_b64 = server_first
        .split(',')
        .find_map(|part| part.strip_prefix("s="))
        .expect("server-first salt");
    let iterations = server_first
        .split(',')
        .find_map(|part| part.strip_prefix("i="))
        .expect("server-first iterations")
        .parse::<u32>()
        .expect("iterations parse");

    let salt = base64::engine::general_purpose::STANDARD
        .decode(salt_b64)
        .expect("salt decode");
    let mut salted_password = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, iterations, &mut salted_password);

    let mut client_key = hmac_sha256(&salted_password, b"Client Key");
    let stored_key = sha256(&client_key);
    let client_final_without_proof = format!("c=biws,r={combined_nonce}");
    let auth_message = format!(
        "{},{},{}",
        client_first
            .strip_prefix("n,,")
            .expect("client-first prefix"),
        server_first,
        client_final_without_proof
    );
    let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
    for (key_byte, signature_byte) in client_key.iter_mut().zip(client_signature) {
        *key_byte ^= signature_byte;
    }
    let proof_b64 = base64::engine::general_purpose::STANDARD.encode(client_key);
    format!("{client_final_without_proof},p={proof_b64}")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    type HmacSha256 = Hmac<Sha256>;

    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

fn make_connection(input: Vec<u8>) -> Connection<MockEngine, Cursor<Vec<u8>>, Vec<u8>> {
    let engine = Arc::new(MockEngine::new());
    let reader = Cursor::new(input);
    let writer = Vec::new();
    Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new())
}

fn make_connection_with_engine(
    input: Vec<u8>,
    engine: Arc<MockEngine>,
) -> Connection<MockEngine, Cursor<Vec<u8>>, Vec<u8>> {
    let reader = Cursor::new(input);
    let writer = Vec::new();
    Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new())
}

fn make_connection_with_query_engine<E>(
    input: Vec<u8>,
    engine: Arc<E>,
) -> Connection<E, Cursor<Vec<u8>>, Vec<u8>>
where
    E: QueryEngine + 'static,
{
    let reader = Cursor::new(input);
    let writer = Vec::new();
    Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new())
}

const STARTUP_RESPONSE_TAG_COUNT: usize = 17;

fn backend_message_tags(bytes: &[u8]) -> Vec<u8> {
    let mut tags = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        tags.push(bytes[offset]);
        let len =
            u32::from_be_bytes(bytes[offset + 1..offset + 5].try_into().expect("length")) as usize;
        offset += 1 + len;
    }
    tags
}

fn backend_messages(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut messages = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let tag = bytes[offset];
        let len =
            u32::from_be_bytes(bytes[offset + 1..offset + 5].try_into().expect("length")) as usize;
        let payload_start = offset + 5;
        let payload_end = offset + 1 + len;
        messages.push((tag, bytes[payload_start..payload_end].to_vec()));
        offset += 1 + len;
    }
    messages
}

fn parameter_status_values(bytes: &[u8]) -> Vec<(String, String)> {
    backend_messages(bytes)
        .into_iter()
        .filter_map(|(tag, payload)| {
            if tag != b'S' {
                return None;
            }
            let mut parts = payload.split(|byte| *byte == 0);
            let key = parts.next()?;
            let value = parts.next()?;
            if key.is_empty() {
                return None;
            }
            Some((
                String::from_utf8_lossy(key).into_owned(),
                String::from_utf8_lossy(value).into_owned(),
            ))
        })
        .collect()
}

fn ready_for_query_statuses(bytes: &[u8]) -> Vec<u8> {
    let mut statuses = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let tag = bytes[offset];
        let len =
            u32::from_be_bytes(bytes[offset + 1..offset + 5].try_into().expect("length")) as usize;
        if tag == b'Z' {
            statuses.push(bytes[offset + 5]);
        }
        offset += 1 + len;
    }
    statuses
}

fn error_response_codes(bytes: &[u8]) -> Vec<String> {
    let mut codes = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let tag = bytes[offset];
        let len =
            u32::from_be_bytes(bytes[offset + 1..offset + 5].try_into().expect("length")) as usize;
        if tag == b'E' {
            let mut payload = &bytes[offset + 5..offset + 1 + len];
            while let Some((&field, rest)) = payload.split_first() {
                if field == 0 {
                    break;
                }
                let Some(end) = rest.iter().position(|byte| *byte == 0) else {
                    break;
                };
                if field == b'C' {
                    codes.push(String::from_utf8_lossy(&rest[..end]).into_owned());
                    break;
                }
                payload = &rest[end + 1..];
            }
        }
        offset += 1 + len;
    }
    codes
}
