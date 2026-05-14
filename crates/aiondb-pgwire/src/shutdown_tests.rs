//! Graceful shutdown tests (MISS-W2).
//!
//! Tests that the `PgWireServer` shutdown mechanism works correctly at the
//! unit level, focusing on the watch channel signaling and server behavior
//! rather than full TCP integration.

use std::sync::Arc;
use std::time::Duration;

use aiondb_core::{DatabaseId, Value};
use aiondb_engine::{
    AuthenticatedIdentity, DbResult, IsolationLevel, PortalBatch, PortalDescription,
    PreparedStatementDesc, QueryEngine, SessionHandle, SessionInfo, SessionLimits, StartupParams,
    StatementResult,
};
use tokio::sync::watch;

use crate::server::{PgWireConfig, PgWireServer};

// -------------------------------------------------------------------------
// Mock engine
// -------------------------------------------------------------------------

struct ShutdownMockEngine;

impl ShutdownMockEngine {
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

impl QueryEngine for ShutdownMockEngine {
    fn requires_password(&self) -> bool {
        false
    }
    fn startup(&self, _params: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
        Ok((SessionHandle::test_handle(), Self::dummy_session_info()))
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

// =========================================================================
// MISS-W2: Graceful shutdown tests
// =========================================================================

/// Test that the shutdown watch channel propagates the signal correctly.
///
/// The watch channel is the core mechanism used by `PgWireServer::start()`.
/// Sending `true` should be observable by the receiver.
#[tokio::test]
async fn shutdown_watch_channel_signal_propagates() {
    let (tx, mut rx) = watch::channel(false);

    // Initially false.
    assert!(!*rx.borrow());

    // Send the shutdown signal.
    tx.send(true).expect("send should succeed");

    // The receiver should see the new value.
    rx.changed().await.expect("changed should succeed");
    assert!(*rx.borrow());
}

/// Test that the shutdown signal stops the server accept loop.
///
/// We start the server on an ephemeral port and immediately send the
/// shutdown signal. The server should exit promptly without error.
#[tokio::test]
async fn shutdown_signal_stops_server() {
    let engine = Arc::new(ShutdownMockEngine);
    let Some(port) = portpicker_ephemeral() else {
        return;
    };
    let config = PgWireConfig {
        bind_address: "127.0.0.1".to_string(),
        port,
        max_connections: 128,
        max_connections_per_ip: 32,
        startup_timeout: Duration::from_secs(5),
        shutdown_timeout: Duration::from_secs(1),
        auth_failure_backoff: Duration::ZERO,
        engine_pool: aiondb_config::EnginePoolConfig::default(),
        tls: None,
        idle_timeout: Duration::ZERO,
        require_tls: false,
        fail_on_weak_rng: false,
        max_portals: aiondb_config::runtime::DEFAULT_LIMITS_MAX_PORTALS,
    };

    let server = Arc::new(PgWireServer::new_plain(engine, config));
    let (tx, rx) = watch::channel(false);

    let server_handle = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.start(rx).await })
    };

    // Give the server a moment to start listening.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send the shutdown signal.
    tx.send(true).expect("send should succeed");

    // The server should exit within a reasonable time.
    let result = tokio::time::timeout(Duration::from_secs(5), server_handle)
        .await
        .expect("server should stop within timeout");

    let server_result = result.expect("server task should not panic");
    if let Err(error) = server_result {
        if bind_is_forbidden(&error) {
            return;
        }
        panic!("server should shut down without error: {error}");
    }
}

/// Test that when no connections are active, shutdown completes immediately.
///
/// Start the server, send shutdown right away -- there are no active connections
#[tokio::test]
async fn shutdown_no_active_connections_completes_immediately() {
    let engine = Arc::new(ShutdownMockEngine);
    let Some(port) = portpicker_ephemeral() else {
        return;
    };
    let config = PgWireConfig {
        bind_address: "127.0.0.1".to_string(),
        port,
        max_connections: 128,
        max_connections_per_ip: 32,
        startup_timeout: Duration::from_secs(5),
        shutdown_timeout: Duration::from_secs(1),
        auth_failure_backoff: Duration::ZERO,
        engine_pool: aiondb_config::EnginePoolConfig::default(),
        tls: None,
        idle_timeout: Duration::ZERO,
        require_tls: false,
        fail_on_weak_rng: false,
        max_portals: aiondb_config::runtime::DEFAULT_LIMITS_MAX_PORTALS,
    };

    let server = Arc::new(PgWireServer::new_plain(engine, config));
    let (tx, rx) = watch::channel(false);

    let server_handle = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.start(rx).await })
    };

    // Give the server a moment to bind.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send shutdown immediately -- no connections are active.
    tx.send(true).expect("send should succeed");

    let start = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(5), server_handle)
        .await
        .expect("server should stop within timeout");

    let elapsed = start.elapsed();
    let server_result = result.expect("server task should not panic");
    if let Err(error) = server_result {
        if bind_is_forbidden(&error) {
            return;
        }
        panic!("server should shut down without error: {error}");
    }
    // Shutdown with no active connections should be fast (well under 1 second).
    assert!(
        elapsed < Duration::from_secs(2),
        "shutdown with no connections should complete quickly, took {elapsed:?}"
    );
}

/// Test that the shutdown signal prevents new connections from being accepted.
///
/// After sending the shutdown signal, attempting to connect should fail
/// because the server is no longer accepting.
#[tokio::test]
async fn shutdown_prevents_new_connections() {
    let engine = Arc::new(ShutdownMockEngine);
    let Some(port) = portpicker_ephemeral() else {
        return;
    };
    let config = PgWireConfig {
        bind_address: "127.0.0.1".to_string(),
        port,
        max_connections: 128,
        max_connections_per_ip: 32,
        startup_timeout: Duration::from_secs(5),
        shutdown_timeout: Duration::from_secs(1),
        auth_failure_backoff: Duration::ZERO,
        engine_pool: aiondb_config::EnginePoolConfig::default(),
        tls: None,
        idle_timeout: Duration::ZERO,
        require_tls: false,
        fail_on_weak_rng: false,
        max_portals: aiondb_config::runtime::DEFAULT_LIMITS_MAX_PORTALS,
    };

    let server = Arc::new(PgWireServer::new_plain(engine, config));
    let (tx, rx) = watch::channel(false);

    let server_handle = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.start(rx).await })
    };

    // Give the server time to start.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send shutdown.
    tx.send(true).expect("send should succeed");

    // Wait for the server to finish.
    let server_result = tokio::time::timeout(Duration::from_secs(5), server_handle)
        .await
        .expect("server should stop")
        .expect("server task should not panic");
    if let Err(error) = server_result {
        if bind_is_forbidden(&error) {
            return;
        }
        panic!("server should shut down without error: {error}");
    }

    // Now try to connect -- should fail since the server is stopped.
    let connect_result = tokio::time::timeout(
        Duration::from_millis(500),
        tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")),
    )
    .await;

    match connect_result {
        Ok(Ok(_)) => panic!("connection should have been refused after shutdown"),
        Ok(Err(_)) => { /* Expected: connection refused */ }
        Err(_) => { /* Timeout is also acceptable -- no one is listening */ }
    }
}

/// Test that an idle pre-auth client is released once the startup timeout expires.
#[tokio::test]
async fn startup_timeout_releases_idle_pre_auth_connection_slot() {
    let engine = Arc::new(ShutdownMockEngine);
    let Some(port) = portpicker_ephemeral() else {
        return;
    };
    let config = PgWireConfig {
        bind_address: "127.0.0.1".to_string(),
        port,
        max_connections: 1,
        max_connections_per_ip: 1,
        startup_timeout: Duration::from_millis(50),
        shutdown_timeout: Duration::from_secs(1),
        auth_failure_backoff: Duration::ZERO,
        engine_pool: aiondb_config::EnginePoolConfig::default(),
        tls: None,
        idle_timeout: Duration::ZERO,
        require_tls: false,
        fail_on_weak_rng: false,
        max_portals: aiondb_config::runtime::DEFAULT_LIMITS_MAX_PORTALS,
    };

    let server = Arc::new(PgWireServer::new_plain(engine, config));
    let (tx, rx) = watch::channel(false);

    let server_handle = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.start(rx).await })
    };

    tokio::time::sleep(Duration::from_millis(50)).await;

    let idle_client = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .expect("idle client should connect");

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if server.health_snapshot().active_connections == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("startup timeout should release the idle connection slot");

    let second_client = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .expect("connection slot should be reusable after timeout");

    drop(second_client);
    drop(idle_client);
    tx.send(true).expect("send should succeed");

    let server_result = tokio::time::timeout(Duration::from_secs(5), server_handle)
        .await
        .expect("server should stop")
        .expect("server task should not panic");
    if let Err(error) = server_result {
        if bind_is_forbidden(&error) {
            return;
        }
        panic!("server should shut down without error: {error}");
    }
}

/// Test that multiple receivers of the watch channel all see the shutdown.
///
/// This validates the broadcast nature of the watch channel used internally.
#[tokio::test]
async fn shutdown_watch_channel_multiple_receivers() {
    let (tx, rx1) = watch::channel(false);
    let mut rx2 = rx1.clone();
    let mut rx3 = rx1.clone();
    // Drop rx1 since we only test rx2 and rx3.
    drop(rx1);

    tx.send(true).expect("send should succeed");

    rx2.changed().await.expect("rx2 changed");
    assert!(*rx2.borrow());

    rx3.changed().await.expect("rx3 changed");
    assert!(*rx3.borrow());
}

/// Test that the cancel registry is accessible on the server.
#[test]
fn server_exposes_cancel_registry() {
    let engine = Arc::new(ShutdownMockEngine);
    let config = PgWireConfig::default();
    let server = PgWireServer::new_plain(engine, config);
    let registry = server.cancel_registry();
    // Registry should be empty initially.
    assert!(registry.lookup(1, 1).is_none());
}

// -------------------------------------------------------------------------
// Helper: pick an ephemeral port
// -------------------------------------------------------------------------

fn bind_is_forbidden(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::AddrNotAvailable
    )
}

/// Find a likely-available ephemeral port by binding to port 0.
fn portpicker_ephemeral() -> Option<u16> {
    match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => Some(listener.local_addr().expect("local addr").port()),
        Err(error) if bind_is_forbidden(&error) => None,
        Err(error) => panic!("bind to ephemeral port: {error}"),
    }
}
