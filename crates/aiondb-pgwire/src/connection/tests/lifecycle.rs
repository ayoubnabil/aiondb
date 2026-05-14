use super::*;
use aiondb_core::VectorValue;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

struct VectorParamExtendedEngine {
    inner: MockEngine,
    bound_params: Mutex<Vec<Value>>,
}

impl VectorParamExtendedEngine {
    fn new() -> Self {
        Self {
            inner: MockEngine::new(),
            bound_params: Mutex::new(Vec::new()),
        }
    }

    fn bound_params(&self) -> Vec<Value> {
        self.bound_params.lock().expect("bound params").clone()
    }
}

impl QueryEngine for VectorParamExtendedEngine {
    fn requires_password(&self) -> bool {
        <MockEngine as QueryEngine>::requires_password(&self.inner)
    }

    fn startup_authentication(
        &self,
        user: &str,
        database: &str,
        transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication> {
        <MockEngine as QueryEngine>::startup_authentication(&self.inner, user, database, transport)
    }

    fn startup(&self, params: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
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
        _session: &SessionHandle,
        _name: String,
        _sql: String,
    ) -> DbResult<PreparedStatementDesc> {
        Ok(PreparedStatementDesc {
            name: String::new(),
            param_types: vec![DataType::Vector {
                dims: 3,
                element_type: aiondb_core::VectorElementType::Float32,
            }],
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
            param_types: vec![DataType::Vector {
                dims: 3,
                element_type: aiondb_core::VectorElementType::Float32,
            }],
            result_columns: vec![],
            result_column_origins: vec![],
        })
    }

    fn bind(
        &self,
        _session: &SessionHandle,
        _portal: String,
        _stmt: String,
        params: Vec<Value>,
    ) -> DbResult<()> {
        *self.bound_params.lock().expect("bound params") = params;
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

struct StartupRoleEngine {
    inner: MockEngine,
    is_superuser: bool,
}

impl StartupRoleEngine {
    fn new(is_superuser: bool) -> Self {
        Self {
            inner: MockEngine::new(),
            is_superuser,
        }
    }
}

impl QueryEngine for StartupRoleEngine {
    fn requires_password(&self) -> bool {
        <MockEngine as QueryEngine>::requires_password(&self.inner)
    }

    fn startup_authentication(
        &self,
        user: &str,
        database: &str,
        transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication> {
        <MockEngine as QueryEngine>::startup_authentication(&self.inner, user, database, transport)
    }

    fn startup(&self, params: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
        let (session, mut info) = <MockEngine as QueryEngine>::startup(&self.inner, params)?;
        info.is_superuser = self.is_superuser;
        Ok((session, info))
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
        name: String,
        sql: String,
    ) -> DbResult<PreparedStatementDesc> {
        <MockEngine as QueryEngine>::prepare(&self.inner, session, name, sql)
    }

    fn describe_statement(
        &self,
        session: &SessionHandle,
        name: &str,
    ) -> DbResult<PreparedStatementDesc> {
        <MockEngine as QueryEngine>::describe_statement(&self.inner, session, name)
    }

    fn bind(
        &self,
        session: &SessionHandle,
        portal: String,
        stmt: String,
        params: Vec<Value>,
    ) -> DbResult<()> {
        <MockEngine as QueryEngine>::bind(&self.inner, session, portal, stmt, params)
    }

    fn describe_portal(&self, session: &SessionHandle, name: &str) -> DbResult<PortalDescription> {
        <MockEngine as QueryEngine>::describe_portal(&self.inner, session, name)
    }

    fn execute_portal(
        &self,
        session: &SessionHandle,
        name: &str,
        max: usize,
    ) -> DbResult<PortalBatch> {
        <MockEngine as QueryEngine>::execute_portal(&self.inner, session, name, max)
    }

    fn close_statement(&self, session: &SessionHandle, name: &str) -> DbResult<()> {
        <MockEngine as QueryEngine>::close_statement(&self.inner, session, name)
    }

    fn close_portal(&self, session: &SessionHandle, name: &str) -> DbResult<()> {
        <MockEngine as QueryEngine>::close_portal(&self.inner, session, name)
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

struct CancelableCopyInEngine {
    canceled: AtomicBool,
}

impl CancelableCopyInEngine {
    fn new() -> Self {
        Self {
            canceled: AtomicBool::new(false),
        }
    }
}

impl QueryEngine for CancelableCopyInEngine {
    fn startup_authentication(
        &self,
        _user: &str,
        _database: &str,
        _transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication> {
        Ok(StartupAuthentication::Trust)
    }

    fn startup(&self, _params: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
        Ok((
            SessionHandle::test_handle(),
            MockEngine::dummy_session_info(),
        ))
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

    fn execute_sql(&self, _session: &SessionHandle, sql: &str) -> DbResult<Vec<StatementResult>> {
        if sql.trim().eq_ignore_ascii_case("COPY t FROM STDIN") {
            return Ok(vec![StatementResult::CopyIn {
                table_id: aiondb_core::RelationId::new(42),
                columns: vec![ResultColumn {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                }],
            }]);
        }
        Ok(vec![])
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
        if self.canceled.load(Ordering::Relaxed) {
            return Err(DbError::query_canceled("session canceled"));
        }
        Ok(StatementResult::Command {
            tag: "COPY".to_owned(),
            rows_affected: 0,
        })
    }

    fn check_session_cancellation(&self, _session: &SessionHandle) -> DbResult<()> {
        if self.canceled.load(Ordering::Relaxed) {
            return Err(DbError::query_canceled("session canceled"));
        }
        Ok(())
    }

    fn cancel_session(&self, _session: &SessionHandle) -> DbResult<()> {
        self.canceled.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn terminate(&self, _session: SessionHandle) -> DbResult<()> {
        Ok(())
    }

    fn session_count(&self) -> DbResult<usize> {
        Ok(0)
    }
}

// -----------------------------------------------------------------------
// Transaction status tracking tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn query_error_outside_transaction_returns_idle_ready() {
    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("ERROR"));
    input.extend(build_terminate_bytes());

    let mut conn = make_connection(input);
    conn.run().await.unwrap();

    assert_eq!(ready_for_query_statuses(&conn.writer), vec![b'I', b'I']);
}

#[tokio::test]
async fn sync_preserves_failed_transaction_until_explicit_rollback() {
    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes("ERROR"));
    input.push(b'S');
    input.extend_from_slice(&4u32.to_be_bytes());
    input.extend(build_query_bytes("ROLLBACK"));
    input.extend(build_terminate_bytes());

    let mut conn = make_connection(input);
    conn.run().await.unwrap();

    assert_eq!(
        ready_for_query_statuses(&conn.writer),
        vec![b'I', b'T', b'E', b'E', b'I']
    );
}

#[tokio::test]
async fn startup_auth_failure_writes_error_response_before_closing() {
    let input = build_startup_bytes();
    let engine = Arc::new(MockEngine::with_startup_error(
        DbError::invalid_authorization("bad login"),
    ));
    let mut conn = make_connection_with_engine(input, engine);

    let error = conn.run().await.expect_err("startup should fail");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
    assert_eq!(error_response_codes(&conn.writer), vec!["28000"]);
}

#[tokio::test]
async fn startup_rejects_empty_user_parameter() {
    let input = build_startup_bytes_with_user("");
    let mut conn = make_connection(input);

    let error = conn.run().await.expect_err("startup should fail");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
    assert_eq!(error_response_codes(&conn.writer), vec!["28000"]);
}

#[tokio::test]
async fn startup_rejects_empty_database_parameter() {
    let input = build_startup_bytes_with_params(&[("user", "test"), ("database", "")]);
    let mut conn = make_connection(input);

    let error = conn.run().await.expect_err("startup should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::InvalidCatalogName);
    assert_eq!(error_response_codes(&conn.writer), vec!["3D000"]);
}

#[tokio::test]
async fn startup_rejects_overlong_parameter_name() {
    let long_name = "x".repeat(65);
    let input = build_startup_bytes_with_params(&[("user", "test"), (long_name.as_str(), "1")]);
    let mut conn = make_connection(input);

    let error = conn.run().await.expect_err("startup should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::InternalError);
    assert!(error.report().message.contains("parameter name exceeds"));
    assert_eq!(error_response_codes(&conn.writer), vec!["XX000"]);
}

#[tokio::test]
async fn startup_rejects_invalid_replication_parameter_value() {
    let input = build_startup_bytes_with_params(&[("user", "test"), ("replication", "maybe")]);
    let mut conn = make_connection(input);

    let error = conn.run().await.expect_err("startup should fail");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidParameterValue
    );
    assert!(error.report().message.contains("replication"));
    assert_eq!(error_response_codes(&conn.writer), vec!["22023"]);
}

#[tokio::test]
async fn startup_success_increments_metrics_snapshot() {
    let input = build_startup_bytes();
    let engine = Arc::new(MockEngine::new());
    let mut conn = make_connection_with_engine(input, engine);
    let metrics = Arc::new(ServerMetrics::new());
    conn.set_metrics(metrics.clone());

    conn.run().await.expect("startup should succeed");

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.successful_startups, 1);
    assert_eq!(snapshot.failed_startups, 0);
    assert_eq!(snapshot.authentication_failures, 0);
}

#[tokio::test]
async fn startup_auth_failure_increments_auth_metrics() {
    let input = build_startup_bytes();
    let engine = Arc::new(MockEngine::with_startup_error(
        DbError::invalid_authorization("bad login"),
    ));
    let mut conn = make_connection_with_engine(input, engine);
    let metrics = Arc::new(ServerMetrics::new());
    conn.set_metrics(metrics.clone());

    let _ = conn.run().await.expect_err("startup should fail");

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.successful_startups, 0);
    assert_eq!(snapshot.failed_startups, 1);
    assert_eq!(snapshot.authentication_failures, 1);
}

#[tokio::test]
async fn startup_protocol_failure_increments_failed_startups_without_auth_failure() {
    let input = build_startup_bytes();
    let engine = Arc::new(MockEngine::with_startup_auth_error(DbError::protocol(
        "malformed startup parameters",
    )));
    let mut conn = make_connection_with_engine(input, engine);
    let metrics = Arc::new(ServerMetrics::new());
    conn.set_metrics(metrics.clone());

    let _ = conn.run().await.expect_err("startup should fail");

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.successful_startups, 0);
    assert_eq!(snapshot.failed_startups, 1);
    assert_eq!(snapshot.authentication_failures, 0);
}

#[tokio::test]
async fn startup_parameter_status_reports_non_superuser_off() {
    let input = build_startup_bytes();
    let engine = Arc::new(StartupRoleEngine::new(false));
    let mut conn = make_connection_with_query_engine(input, engine);

    conn.run().await.expect("startup should succeed");

    let statuses = parameter_status_values(&conn.writer);
    assert!(
        statuses
            .iter()
            .any(|(key, value)| key == "is_superuser" && value == "off"),
        "expected startup ParameterStatus is_superuser=off, got {statuses:?}"
    );
}

#[tokio::test]
async fn startup_parameter_status_reports_superuser_on() {
    let input = build_startup_bytes();
    let engine = Arc::new(StartupRoleEngine::new(true));
    let mut conn = make_connection_with_query_engine(input, engine);

    conn.run().await.expect("startup should succeed");

    let statuses = parameter_status_values(&conn.writer);
    assert!(
        statuses
            .iter()
            .any(|(key, value)| key == "is_superuser" && value == "on"),
        "expected startup ParameterStatus is_superuser=on, got {statuses:?}"
    );
}

#[tokio::test]
async fn startup_auth_negotiation_failure_writes_error_response_before_closing() {
    let input = build_startup_bytes();
    let engine = Arc::new(MockEngine::with_startup_auth_error(
        DbError::invalid_authorization("bad pre-auth"),
    ));
    let mut conn = make_connection_with_engine(input, engine);

    let error = conn.run().await.expect_err("startup should fail");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
    assert_eq!(error_response_codes(&conn.writer), vec!["28000"]);
}

#[tokio::test]
async fn simple_query_increments_total_queries_metric() {
    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("SELECT 1"));
    input.extend(build_terminate_bytes());

    let mut conn = make_connection(input);
    let metrics = Arc::new(ServerMetrics::new());
    conn.set_metrics(metrics.clone());

    conn.run().await.expect("connection should succeed");

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.total_queries, 1);
    assert_eq!(snapshot.successful_startups, 1);
}

#[tokio::test]
async fn simple_query_notice_is_written_to_wire() {
    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("NOTICE"));
    input.extend(build_terminate_bytes());

    let mut conn = make_connection(input);
    conn.run().await.unwrap();

    let tags = backend_message_tags(&conn.writer);
    assert_eq!(&tags[STARTUP_RESPONSE_TAG_COUNT..], b"NZ");
}

#[tokio::test]
async fn startup_auth_failure_respects_configured_backoff() {
    tokio::time::pause();

    let input = build_startup_bytes();
    let engine = Arc::new(MockEngine::with_startup_error(
        DbError::invalid_authorization("bad login"),
    ));
    let mut conn = make_connection_with_engine(input, engine);
    conn.set_auth_failure_backoff(Duration::from_secs(1));

    let task = tokio::spawn(async move {
        let result = conn.run().await;
        (result, conn.writer)
    });

    tokio::task::yield_now().await;
    assert!(!task.is_finished(), "startup should be waiting on backoff");

    tokio::time::advance(Duration::from_millis(999)).await;
    tokio::task::yield_now().await;
    assert!(!task.is_finished(), "startup should still be backing off");

    tokio::time::advance(Duration::from_millis(1)).await;
    let (result, writer) = task.await.expect("task should complete");
    let error = result.expect_err("startup should fail");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
    assert_eq!(error_response_codes(&writer), vec!["28000"]);
}

#[tokio::test]
async fn startup_lockout_failure_respects_configured_backoff() {
    tokio::time::pause();

    let input = build_startup_bytes();
    let engine = Arc::new(MockEngine::with_startup_error(
        DbError::authorization_error(
            aiondb_core::SqlState::TooManyAuthenticationFailures,
            "locked out",
        ),
    ));
    let mut conn = make_connection_with_engine(input, engine);
    conn.set_auth_failure_backoff(Duration::from_secs(1));

    let task = tokio::spawn(async move {
        let result = conn.run().await;
        (result, conn.writer)
    });

    tokio::task::yield_now().await;
    assert!(!task.is_finished(), "startup should be waiting on backoff");

    tokio::time::advance(Duration::from_millis(999)).await;
    tokio::task::yield_now().await;
    assert!(!task.is_finished(), "startup should still be backing off");

    tokio::time::advance(Duration::from_millis(1)).await;
    let (result, writer) = task.await.expect("task should complete");
    let error = result.expect_err("startup should fail");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::TooManyAuthenticationFailures
    );
    assert_eq!(error_response_codes(&writer), vec!["28P02"]);
}

#[tokio::test]
async fn startup_scram_auth_succeeds_and_finishes_startup() {
    let verifier =
        ScramVerifier::from_password_with_salt("pencil", b"salt_for_testing", 4096).unwrap();
    let engine = Arc::new(ScramMockEngine::new(
        verifier,
        b"scram-proof-token".to_vec(),
    ));
    let (mut client, server) = duplex(4096);
    let (server_reader, server_writer) = split(server);
    let mut conn = Connection::new(
        engine,
        server_reader,
        server_writer,
        1,
        42,
        CancelRegistry::new(),
    );

    let task = tokio::spawn(async move { conn.run().await });

    client.write_all(&build_startup_bytes()).await.unwrap();

    let sasl = read_backend_message(&mut client).await.unwrap();
    assert_eq!(sasl.tag, b'R');
    assert_eq!(parse_auth_request(sasl.payload.clone()).unwrap(), 10);
    assert!(
        String::from_utf8_lossy(&sasl.payload[4..]).contains("SCRAM-SHA-256"),
        "expected SCRAM mechanism advertisement"
    );

    let client_first = "n,,n=test,r=clientnonce";
    client
        .write_all(&build_sasl_initial_response_bytes(
            "SCRAM-SHA-256",
            client_first,
        ))
        .await
        .unwrap();

    let server_continue = read_backend_message(&mut client).await.unwrap();
    assert_eq!(server_continue.tag, b'R');
    let server_first = parse_auth_sasl_continue(server_continue.payload).unwrap();

    let client_final = build_scram_client_final("pencil", client_first, &server_first);
    client
        .write_all(&build_sasl_response_bytes(&client_final))
        .await
        .unwrap();

    let server_final = read_backend_message(&mut client).await.unwrap();
    assert_eq!(server_final.tag, b'R');
    let server_final_payload = parse_auth_sasl_final(server_final.payload).unwrap();
    assert!(server_final_payload.starts_with("v="));

    let auth_ok = read_backend_message(&mut client).await.unwrap();
    assert_eq!(auth_ok.tag, b'R');
    assert_eq!(parse_auth_request(auth_ok.payload).unwrap(), 0);

    let mut saw_backend_key = false;
    let mut saw_ready = false;
    while !saw_ready {
        let message = read_backend_message(&mut client).await.unwrap();
        match message.tag {
            b'S' => {}
            b'K' => saw_backend_key = true,
            b'Z' => saw_ready = true,
            other => panic!("unexpected startup message tag {}", other as char),
        }
    }
    assert!(
        saw_backend_key,
        "expected BackendKeyData before ReadyForQuery"
    );

    client.write_all(&build_terminate_bytes()).await.unwrap();
    client.flush().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn startup_scram_rejects_sasl_initial_response_with_trailing_bytes() {
    let verifier =
        ScramVerifier::from_password_with_salt("pencil", b"salt_for_testing", 4096).unwrap();
    let engine = Arc::new(ScramMockEngine::new(
        verifier,
        b"scram-proof-token".to_vec(),
    ));
    let (mut client, server) = duplex(4096);
    let (server_reader, server_writer) = split(server);
    let mut conn = Connection::new(
        engine,
        server_reader,
        server_writer,
        1,
        42,
        CancelRegistry::new(),
    );

    let task = tokio::spawn(async move { conn.run().await });

    client.write_all(&build_startup_bytes()).await.unwrap();

    let sasl = read_backend_message(&mut client).await.unwrap();
    assert_eq!(sasl.tag, b'R');
    assert_eq!(parse_auth_request(sasl.payload).unwrap(), 10);

    let mut payload = Vec::new();
    payload.extend_from_slice(b"SCRAM-SHA-256");
    payload.push(0);
    payload.extend_from_slice(&(24i32).to_be_bytes());
    payload.extend_from_slice(b"n,,n=test,r=clientnonce");
    payload.extend_from_slice(b"junk");
    client
        .write_all(&build_raw_message(b'p', &payload))
        .await
        .unwrap();

    let error_response = read_backend_message(&mut client).await.unwrap();
    assert_eq!(error_response.tag, b'E');

    let error = task
        .await
        .expect("task should complete")
        .expect_err("startup must reject malformed SCRAM input");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::InternalError);
    assert!(
        error.report().message.contains("unexpected trailing bytes"),
        "unexpected error: {error}"
    );

    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_transaction_rejects_non_recovery_queries() {
    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes("ERROR"));
    input.extend(build_query_bytes("SELECT 1"));
    input.extend(build_query_bytes("ROLLBACK"));
    input.extend(build_terminate_bytes());

    let mut conn = make_connection(input);
    conn.run().await.unwrap();

    assert_eq!(
        ready_for_query_statuses(&conn.writer),
        vec![b'I', b'T', b'E', b'E', b'I']
    );
    assert!(error_response_codes(&conn.writer)
        .iter()
        .any(|code| code == SqlState::InFailedSqlTransaction.code()));
}

#[tokio::test]
async fn extended_parse_error_skips_messages_until_sync() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s1", "ERROR bad sql", &[]));
    input.extend(build_bind_bytes("p1", "s1", &[], &[], &[]));
    input.extend(build_execute_bytes("p1", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s2", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p2", "s2", &[], &[], &[]));
    input.extend(build_execute_bytes("p2", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let mut conn = make_connection(input);
    conn.run().await.unwrap();

    let tags = backend_message_tags(&conn.writer);
    assert_eq!(&tags[STARTUP_RESPONSE_TAG_COUNT..], b"EZ12CZ");
}

#[tokio::test]
async fn extended_parse_bind_execute_accepts_vector_text_parameter() {
    let engine = Arc::new(VectorParamExtendedEngine::new());
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_vec", "SELECT $1::vector", &[62_000]));
    input.extend(build_bind_bytes(
        "p_vec",
        "s_vec",
        &[0],
        &[Some(b"[1.0,2.0,3.0]")],
        &[],
    ));
    input.extend(build_execute_bytes("p_vec", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let mut conn = make_connection_with_query_engine(input, engine.clone());
    conn.run().await.unwrap();

    let tags = backend_message_tags(&conn.writer);
    assert_eq!(&tags[STARTUP_RESPONSE_TAG_COUNT..], b"12CZ");
    assert_eq!(
        engine.bound_params(),
        vec![Value::Vector(VectorValue::new(3, vec![1.0, 2.0, 3.0]))]
    );
}

#[tokio::test]
async fn extended_bind_rejects_invalid_vector_text_parameter() {
    let engine = Arc::new(VectorParamExtendedEngine::new());
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_vec", "SELECT $1::vector", &[62_000]));
    input.extend(build_bind_bytes(
        "p_vec",
        "s_vec",
        &[0],
        &[Some(b"[1.0,nope,3.0]")],
        &[],
    ));
    input.extend(build_execute_bytes("p_vec", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let mut conn = make_connection_with_query_engine(input, engine.clone());
    conn.run().await.unwrap();

    let tags = backend_message_tags(&conn.writer);
    assert_eq!(&tags[STARTUP_RESPONSE_TAG_COUNT..], b"1EZ");
    assert!(engine.bound_params().is_empty());
}

#[tokio::test]
async fn extended_parse_rejects_mismatched_parameter_oid() {
    let engine = Arc::new(VectorParamExtendedEngine::new());
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_vec", "SELECT $1::vector", &[23]));
    input.extend(build_bind_bytes(
        "p_vec",
        "s_vec",
        &[0],
        &[Some(b"[1.0,2.0,3.0]")],
        &[],
    ));
    input.extend(build_execute_bytes("p_vec", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let mut conn = make_connection_with_query_engine(input, engine.clone());
    conn.run().await.unwrap();

    let tags = backend_message_tags(&conn.writer);
    assert_eq!(&tags[STARTUP_RESPONSE_TAG_COUNT..], b"EZ");
    assert!(engine.bound_params().is_empty());
}

#[tokio::test]
async fn extended_parse_rejects_parameter_oid_count_mismatch() {
    let engine = Arc::new(VectorParamExtendedEngine::new());
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_vec",
        "SELECT $1::vector",
        &[62_000, 23],
    ));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let mut conn = make_connection_with_query_engine(input, engine);
    conn.run().await.unwrap();

    let tags = backend_message_tags(&conn.writer);
    assert_eq!(&tags[STARTUP_RESPONSE_TAG_COUNT..], b"EZ");
}

#[tokio::test]
async fn malformed_extended_message_skips_until_sync() {
    let mut input = build_startup_bytes();
    input.extend(build_raw_message(b'P', b"malformed-parse"));
    input.extend(build_parse_bytes("ignored_stmt", "SELECT 1", &[]));
    input.extend(build_bind_bytes(
        "ignored_portal",
        "ignored_stmt",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("ignored_portal", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s2", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p2", "s2", &[], &[], &[]));
    input.extend(build_execute_bytes("p2", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let mut conn = make_connection(input);
    conn.run().await.unwrap();

    let tags = backend_message_tags(&conn.writer);
    assert_eq!(&tags[STARTUP_RESPONSE_TAG_COUNT..], b"EZ12CZ");
}

#[tokio::test]
async fn extended_parse_error_in_transaction_waits_for_sync() {
    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_parse_bytes("s1", "ERROR bad sql", &[]));
    input.extend(build_bind_bytes("p1", "s1", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("ROLLBACK"));
    input.extend(build_terminate_bytes());

    let mut conn = make_connection(input);
    conn.run().await.unwrap();

    let tags = backend_message_tags(&conn.writer);
    assert_eq!(&tags[STARTUP_RESPONSE_TAG_COUNT..], b"CZEZCZ");
    assert_eq!(
        ready_for_query_statuses(&conn.writer),
        vec![b'I', b'T', b'E', b'I']
    );
}

// -----------------------------------------------------------------------
// Connection lifecycle tests (with mock engine)
// -----------------------------------------------------------------------

#[tokio::test]
async fn connection_startup_and_terminate() {
    let mut input = build_startup_bytes();
    input.extend(build_terminate_bytes());

    let mut conn = make_connection(input);
    conn.run().await.unwrap();
}

#[tokio::test]
async fn connection_simple_query_select() {
    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("SELECT 1"));
    input.extend(build_terminate_bytes());

    let mut conn = make_connection(input);
    conn.run().await.unwrap();
}

#[tokio::test]
async fn connection_empty_query() {
    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("   "));
    input.extend(build_terminate_bytes());

    let mut conn = make_connection(input);
    conn.run().await.unwrap();
}

#[tokio::test]
async fn connection_query_error_and_sync_stay_idle_outside_transaction() {
    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("ERROR"));
    input.push(b'S');
    input.extend_from_slice(&4u32.to_be_bytes());
    input.extend(build_terminate_bytes());

    let mut conn = make_connection(input);
    conn.run().await.unwrap();

    assert_eq!(
        ready_for_query_statuses(&conn.writer),
        vec![b'I', b'I', b'I']
    );
}

#[tokio::test]
async fn connection_cancel_request_startup() {
    // Build a CancelRequest startup message.
    let mut data = Vec::new();
    let len = 16u32;
    data.extend_from_slice(&len.to_be_bytes());
    data.extend_from_slice(&codec::CANCEL_REQUEST.to_be_bytes());
    data.extend_from_slice(&42u32.to_be_bytes()); // target pid
    data.extend_from_slice(&99u32.to_be_bytes()); // target key

    let mut conn = make_connection(data);
    // CancelRequest during startup should complete without error.
    conn.run().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_request_interrupts_running_query() {
    let engine = Arc::new(MockEngine::with_blocking_query());
    let cancel_registry = CancelRegistry::new();

    let (mut client, server) = duplex(1024);
    let (reader, writer) = tokio::io::split(server);
    let mut conn = Connection::new(
        engine.clone(),
        reader,
        writer,
        1,
        42,
        cancel_registry.clone(),
    );
    let query_task = tokio::spawn(async move { conn.run().await });

    client.write_all(&build_startup_bytes()).await.unwrap();
    loop {
        let message = read_backend_message(&mut client).await.unwrap();
        if message.tag == b'Z' {
            break;
        }
    }

    client.write_all(&build_query_bytes("BLOCK")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    let (mut cancel_client, cancel_server) = duplex(128);
    let (cancel_reader, cancel_writer) = tokio::io::split(cancel_server);
    let mut cancel_conn =
        Connection::new(engine, cancel_reader, cancel_writer, 9, 99, cancel_registry);
    let cancel_task = tokio::spawn(async move { cancel_conn.run().await });

    let mut cancel_request = Vec::new();
    cancel_request.extend_from_slice(&16u32.to_be_bytes());
    cancel_request.extend_from_slice(&codec::CANCEL_REQUEST.to_be_bytes());
    cancel_request.extend_from_slice(&1u32.to_be_bytes());
    cancel_request.extend_from_slice(&42u32.to_be_bytes());
    cancel_client.write_all(&cancel_request).await.unwrap();
    cancel_client.shutdown().await.unwrap();

    cancel_task.await.unwrap().unwrap();

    let error = read_backend_message(&mut client).await.unwrap();
    assert_eq!(error.tag, b'E');
    let ready = read_backend_message(&mut client).await.unwrap();
    assert_eq!(ready.tag, b'Z');

    client.write_all(&build_terminate_bytes()).await.unwrap();
    client.shutdown().await.unwrap();
    query_task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_request_interrupts_copy_in_before_copy_done() {
    let engine = Arc::new(CancelableCopyInEngine::new());
    let cancel_registry = CancelRegistry::new();

    let (mut client, server) = duplex(1024);
    let (reader, writer) = tokio::io::split(server);
    let mut conn = Connection::new(
        engine.clone(),
        reader,
        writer,
        1,
        42,
        cancel_registry.clone(),
    );
    let query_task = tokio::spawn(async move { conn.run().await });

    client.write_all(&build_startup_bytes()).await.unwrap();
    loop {
        let message = read_backend_message(&mut client).await.unwrap();
        if message.tag == b'Z' {
            break;
        }
    }

    client
        .write_all(&build_query_bytes("COPY t FROM STDIN"))
        .await
        .unwrap();
    let copy_response = read_backend_message(&mut client).await.unwrap();
    assert_eq!(copy_response.tag, b'G');

    client
        .write_all(&build_raw_message(b'd', b"1\n"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    let (mut cancel_client, cancel_server) = duplex(128);
    let (cancel_reader, cancel_writer) = tokio::io::split(cancel_server);
    let mut cancel_conn =
        Connection::new(engine, cancel_reader, cancel_writer, 9, 99, cancel_registry);
    let cancel_task = tokio::spawn(async move { cancel_conn.run().await });

    let mut cancel_request = Vec::new();
    cancel_request.extend_from_slice(&16u32.to_be_bytes());
    cancel_request.extend_from_slice(&codec::CANCEL_REQUEST.to_be_bytes());
    cancel_request.extend_from_slice(&1u32.to_be_bytes());
    cancel_request.extend_from_slice(&42u32.to_be_bytes());
    cancel_client.write_all(&cancel_request).await.unwrap();
    cancel_client.shutdown().await.unwrap();
    cancel_task.await.unwrap().unwrap();

    let error = tokio::time::timeout(Duration::from_secs(1), read_backend_message(&mut client))
        .await
        .expect("cancel should interrupt COPY IN promptly")
        .unwrap();
    assert_eq!(error.tag, b'E');
    let ready = read_backend_message(&mut client).await.unwrap();
    assert_eq!(ready.tag, b'Z');

    client.write_all(&build_terminate_bytes()).await.unwrap();
    client.shutdown().await.unwrap();
    query_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn startup_propagates_peer_addr_to_engine() {
    let mut input = build_startup_bytes();
    input.extend(build_terminate_bytes());
    let engine = Arc::new(MockEngine::new());
    let mut conn = make_connection_with_engine(input, Arc::clone(&engine));
    conn.set_peer_addr(Some("127.0.0.1:55432".to_owned()));

    conn.run().await.unwrap();

    let transport = engine.last_transport().expect("startup transport");
    assert_eq!(
        transport,
        TransportInfo {
            kind: TransportKind::Network {
                tls: false,
                peer_addr: Some("127.0.0.1:55432".to_owned()),
            },
        }
    );
}

#[tokio::test]
async fn startup_timeout_aborts_idle_connection() {
    tokio::time::pause();

    let engine = Arc::new(MockEngine::new());
    let (_client, server) = tokio::io::duplex(64);
    let (reader, writer) = tokio::io::split(server);
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_startup_timeout(Duration::from_secs(1));

    let task = tokio::spawn(async move { conn.run().await });
    tokio::task::yield_now().await;
    assert!(
        !task.is_finished(),
        "startup should still be waiting for input"
    );

    tokio::time::advance(Duration::from_secs(1)).await;
    let error = task
        .await
        .expect("task should complete")
        .expect_err("startup must time out");
    assert!(
        format!("{error}").contains("startup timeout exceeded"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn startup_timeout_aborts_idle_cleartext_password_handshake() {
    tokio::time::pause();

    let engine = Arc::new(CleartextMockEngine::new("pencil"));
    let (mut client, server) = duplex(256);
    let (reader, writer) = tokio::io::split(server);
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    let metrics = Arc::new(ServerMetrics::new());
    conn.set_metrics(metrics.clone());
    conn.set_tls(true);
    conn.set_startup_timeout(Duration::from_secs(1));

    let task = tokio::spawn(async move { conn.run().await });

    client.write_all(&build_startup_bytes()).await.unwrap();
    let auth_request = read_backend_message(&mut client).await.unwrap();
    assert_eq!(auth_request.tag, b'R');
    assert_eq!(parse_auth_request(auth_request.payload).unwrap(), 3);

    tokio::task::yield_now().await;
    assert!(
        !task.is_finished(),
        "startup should still be waiting for the password response"
    );

    tokio::time::advance(Duration::from_secs(1)).await;
    let error = task
        .await
        .expect("task should complete")
        .expect_err("startup must time out");
    assert!(
        format!("{error}").contains("startup timeout exceeded"),
        "unexpected error: {error}"
    );

    let error_response = read_backend_message(&mut client).await.unwrap();
    assert_eq!(error_response.tag, b'E');

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.successful_startups, 0);
    assert_eq!(snapshot.failed_startups, 1);
    assert_eq!(snapshot.authentication_failures, 0);
}

#[tokio::test]
async fn startup_timeout_aborts_idle_scram_initial_response_handshake() {
    tokio::time::pause();

    let verifier =
        ScramVerifier::from_password_with_salt("pencil", b"salt_for_testing", 4096).unwrap();
    let engine = Arc::new(ScramMockEngine::new(
        verifier,
        b"scram-proof-token".to_vec(),
    ));
    let (mut client, server) = duplex(256);
    let (reader, writer) = tokio::io::split(server);
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    let metrics = Arc::new(ServerMetrics::new());
    conn.set_metrics(metrics.clone());
    conn.set_startup_timeout(Duration::from_secs(1));

    let task = tokio::spawn(async move { conn.run().await });

    client.write_all(&build_startup_bytes()).await.unwrap();
    let sasl = read_backend_message(&mut client).await.unwrap();
    assert_eq!(sasl.tag, b'R');
    assert_eq!(parse_auth_request(sasl.payload).unwrap(), 10);

    tokio::task::yield_now().await;
    assert!(
        !task.is_finished(),
        "startup should still be waiting for the SCRAM initial response"
    );

    tokio::time::advance(Duration::from_secs(1)).await;
    let error = task
        .await
        .expect("task should complete")
        .expect_err("startup must time out");
    assert!(
        format!("{error}").contains("startup timeout exceeded"),
        "unexpected error: {error}"
    );

    let error_response = read_backend_message(&mut client).await.unwrap();
    assert_eq!(error_response.tag, b'E');

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.successful_startups, 0);
    assert_eq!(snapshot.failed_startups, 1);
    assert_eq!(snapshot.authentication_failures, 0);
}

#[tokio::test]
async fn startup_timeout_aborts_idle_scram_final_response_handshake() {
    tokio::time::pause();

    let verifier =
        ScramVerifier::from_password_with_salt("pencil", b"salt_for_testing", 4096).unwrap();
    let engine = Arc::new(ScramMockEngine::new(
        verifier,
        b"scram-proof-token".to_vec(),
    ));
    let (mut client, server) = duplex(512);
    let (reader, writer) = tokio::io::split(server);
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    let metrics = Arc::new(ServerMetrics::new());
    conn.set_metrics(metrics.clone());
    conn.set_startup_timeout(Duration::from_secs(1));

    let task = tokio::spawn(async move { conn.run().await });

    client.write_all(&build_startup_bytes()).await.unwrap();
    let sasl = read_backend_message(&mut client).await.unwrap();
    assert_eq!(sasl.tag, b'R');
    assert_eq!(parse_auth_request(sasl.payload).unwrap(), 10);

    client
        .write_all(&build_sasl_initial_response_bytes(
            "SCRAM-SHA-256",
            "n,,n=test,r=clientnonce",
        ))
        .await
        .unwrap();
    let server_continue = read_backend_message(&mut client).await.unwrap();
    assert_eq!(server_continue.tag, b'R');
    assert_eq!(
        parse_auth_request(server_continue.payload.clone()).unwrap(),
        11
    );

    tokio::task::yield_now().await;
    assert!(
        !task.is_finished(),
        "startup should still be waiting for the SCRAM final response"
    );

    tokio::time::advance(Duration::from_secs(1)).await;
    let error = task
        .await
        .expect("task should complete")
        .expect_err("startup must time out");
    assert!(
        format!("{error}").contains("startup timeout exceeded"),
        "unexpected error: {error}"
    );

    let error_response = read_backend_message(&mut client).await.unwrap();
    assert_eq!(error_response.tag, b'E');

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.successful_startups, 0);
    assert_eq!(snapshot.failed_startups, 1);
    assert_eq!(snapshot.authentication_failures, 0);
}
