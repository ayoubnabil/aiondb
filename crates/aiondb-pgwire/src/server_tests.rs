use super::*;
use std::sync::Arc;

use aiondb_config::{
    pgwire::{DEFAULT_PGWIRE_BIND_ADDRESS, DEFAULT_PGWIRE_PORT},
    EnginePoolConfig,
};
use aiondb_engine::{
    AccessRequest, Action, AuthenticatedIdentity, Authorizer, DbResult, EngineBuilder,
};
use serde_json::json;

#[derive(Debug, Default)]
struct TestConnectAuthorizer;

impl Authorizer for TestConnectAuthorizer {
    fn authorize(
        &self,
        _identity: &AuthenticatedIdentity,
        request: &AccessRequest,
    ) -> DbResult<()> {
        match request.action {
            Action::Connect => Ok(()),
            _ => Err(DbError::insufficient_privilege(
                "pgwire test authorizer only permits CONNECT",
            )),
        }
    }
}

fn test_engine() -> Arc<aiondb_engine::Engine> {
    Arc::new(
        EngineBuilder::new_in_memory()
            .with_authorizer(Arc::new(TestConnectAuthorizer))
            .build()
            .expect("build pgwire test engine"),
    )
}

fn test_server() -> PgWireServer<aiondb_engine::Engine> {
    PgWireServer::new_plain(
        test_engine(),
        PgWireConfig {
            require_tls: false,
            ..PgWireConfig::default()
        },
    )
}

#[test]
fn config_default_values() {
    let cfg = PgWireConfig::default();
    assert_eq!(cfg.bind_address, DEFAULT_PGWIRE_BIND_ADDRESS);
    assert_eq!(cfg.port, DEFAULT_PGWIRE_PORT);
    assert_eq!(cfg.max_connections, 128);
    assert_eq!(cfg.max_connections_per_ip, 128);
    assert_eq!(cfg.startup_timeout, Duration::from_secs(5));
    assert_eq!(cfg.auth_failure_backoff, Duration::from_millis(250));
}

#[test]
fn config_clone() {
    let cfg = PgWireConfig {
        bind_address: "0.0.0.0".to_string(),
        port: 15432,
        max_connections: 10,
        max_connections_per_ip: 3,
        startup_timeout: Duration::from_secs(2),
        shutdown_timeout: Duration::from_secs(10),
        auth_failure_backoff: Duration::from_millis(500),
        engine_pool: EnginePoolConfig::default(),
        idle_timeout: Duration::from_secs(60 * 5),
        tls: None,
        require_tls: true,
        fail_on_weak_rng: true,
        max_portals: 64,
    };
    let cfg2 = cfg.clone();
    assert_eq!(cfg2.bind_address, "0.0.0.0");
    assert_eq!(cfg2.port, 15432);
    assert_eq!(cfg2.max_connections, 10);
    assert_eq!(cfg2.max_connections_per_ip, 3);
    assert_eq!(cfg2.startup_timeout, Duration::from_secs(2));
    assert_eq!(cfg2.shutdown_timeout, Duration::from_secs(10));
    assert_eq!(cfg2.auth_failure_backoff, Duration::from_millis(500));
    assert_eq!(cfg2.engine_pool, EnginePoolConfig::default());
    assert!(cfg2.tls.is_none());
    assert!(cfg2.require_tls);
    assert!(cfg2.fail_on_weak_rng);
}

#[test]
fn config_default_tls_is_none() {
    let cfg = PgWireConfig::default();
    assert!(cfg.tls.is_none());
}

#[test]
fn config_default_shutdown_timeout() {
    let cfg = PgWireConfig::default();
    assert_eq!(cfg.shutdown_timeout, DEFAULT_SHUTDOWN_TIMEOUT);
}

#[test]
fn config_default_require_tls_is_true() {
    let cfg = PgWireConfig::default();
    assert!(cfg.require_tls);
}

#[test]
fn config_default_fail_on_weak_rng_is_true() {
    let cfg = PgWireConfig::default();
    assert!(cfg.fail_on_weak_rng);
}

#[test]
fn checked_deadline_after_disables_overflowing_timeouts() {
    assert!(super::checked_deadline_after(Duration::ZERO, "test timeout").is_none());
    assert!(super::checked_deadline_after(Duration::from_millis(1), "test timeout").is_some());
    assert!(super::checked_deadline_after(Duration::MAX, "test timeout").is_none());
}

#[test]
fn server_enforces_global_connection_limit() {
    let config = PgWireConfig {
        max_connections: 1,
        max_connections_per_ip: 10,
        ..PgWireConfig::default()
    };
    let server = PgWireServer::new_plain(test_engine(), config);
    let first: SocketAddr = "127.0.0.1:10001".parse().unwrap();
    let second: SocketAddr = "127.0.0.2:10002".parse().unwrap();

    let first_ip = server.try_acquire_connection_slot(&first);
    assert_eq!(first_ip.as_deref(), Some("127.0.0.1"));
    assert!(server.try_acquire_connection_slot(&second).is_none());

    PgWireServer::<aiondb_engine::Engine>::release_connection_slot(
        server.metrics(),
        &server.connection_slots,
        first_ip.as_deref(),
    );
    assert!(server.try_acquire_connection_slot(&second).is_some());
}

#[test]
fn server_enforces_per_ip_connection_limit() {
    let config = PgWireConfig {
        max_connections: 10,
        max_connections_per_ip: 1,
        ..PgWireConfig::default()
    };
    let server = PgWireServer::new_plain(test_engine(), config);
    let first: SocketAddr = "127.0.0.1:10001".parse().unwrap();
    let second_same_ip: SocketAddr = "127.0.0.1:10002".parse().unwrap();
    let other_ip: SocketAddr = "127.0.0.2:10003".parse().unwrap();

    let first_ip = server.try_acquire_connection_slot(&first);
    assert_eq!(first_ip.as_deref(), Some("127.0.0.1"));
    assert!(server
        .try_acquire_connection_slot(&second_same_ip)
        .is_none());
    assert_eq!(
        server.try_acquire_connection_slot(&other_ip).as_deref(),
        Some("127.0.0.2")
    );

    PgWireServer::<aiondb_engine::Engine>::release_connection_slot(
        server.metrics(),
        &server.connection_slots,
        first_ip.as_deref(),
    );
    assert!(server
        .try_acquire_connection_slot(&second_same_ip)
        .is_some());
}

#[test]
fn server_normalizes_invalid_connection_limits() {
    let config = PgWireConfig {
        max_connections: 0,
        max_connections_per_ip: 5,
        ..PgWireConfig::default()
    };
    let server = PgWireServer::new_plain(test_engine(), config);
    let first: SocketAddr = "127.0.0.1:10001".parse().unwrap();
    let second_same_ip: SocketAddr = "127.0.0.1:10002".parse().unwrap();
    let other_ip: SocketAddr = "127.0.0.2:10003".parse().unwrap();

    assert_eq!(server.config.max_connections, 1);
    assert_eq!(server.config.max_connections_per_ip, 1);

    let first_ip = server.try_acquire_connection_slot(&first);
    assert_eq!(first_ip.as_deref(), Some("127.0.0.1"));
    assert!(server
        .try_acquire_connection_slot(&second_same_ip)
        .is_none());
    assert!(server.try_acquire_connection_slot(&other_ip).is_none());

    PgWireServer::<aiondb_engine::Engine>::release_connection_slot(
        server.metrics(),
        &server.connection_slots,
        first_ip.as_deref(),
    );
    assert!(server.try_acquire_connection_slot(&other_ip).is_some());
}

#[test]
fn cancel_registry_register_and_lookup() {
    let registry = CancelRegistry::new();
    let handle = aiondb_engine::SessionHandle::test_handle();
    registry.register(42, 123, handle.clone());
    assert!(registry.lookup(42, 123).is_some());
}

#[test]
fn cancel_registry_lookup_missing() {
    let registry = CancelRegistry::new();
    assert!(registry.lookup(99, 0).is_none());
}

#[test]
fn generate_cancel_secret_is_random() {
    let s1 = super::generate_cancel_secret(false).expect("cancel secret generation must succeed");
    let s2 = super::generate_cancel_secret(false).expect("cancel secret generation must succeed");
    // Extremely unlikely to be equal with 32-bit random values
    assert_ne!(s1, s2, "cancel secrets should be random, not deterministic");
}

#[test]
fn generate_cancel_secret_fail_on_weak_rng_returns_error() {
    super::inject_cancel_secret_rng_failure();
    let err = super::generate_cancel_secret(true)
        .expect_err("weak RNG must fail cleanly when fail_on_weak_rng is enabled");
    assert!(err
        .to_string()
        .contains("refusing to generate a predictable cancel secret"));
}

#[test]
fn generate_cancel_secret_falls_back_when_weak_rng_allowed() {
    super::inject_cancel_secret_rng_failure();
    let secret = super::generate_cancel_secret(false)
        .expect("fallback secret generation must succeed when weak RNG is allowed");
    assert_ne!(secret, 0);
}

#[test]
fn cancel_registry_unregister() {
    let registry = CancelRegistry::new();
    let handle = aiondb_engine::SessionHandle::test_handle();
    registry.register(1, 2, handle);
    registry.unregister(1, 2);
    assert!(registry.lookup(1, 2).is_none());
}

#[test]
fn server_metrics_snapshot_includes_startup_counters() {
    let metrics = ServerMetrics::new();
    metrics.total_connections.fetch_add(3, Ordering::Relaxed);
    metrics.active_connections.fetch_add(1, Ordering::Relaxed);
    metrics.total_queries.fetch_add(7, Ordering::Relaxed);
    metrics.record_startup_success();
    metrics.record_startup_failure(&DbError::invalid_authorization("bad login"));
    metrics.record_startup_failure(&DbError::protocol("malformed startup"));

    assert_eq!(
        metrics.snapshot(),
        ServerMetricsSnapshot {
            total_connections: 3,
            active_connections: 1,
            total_queries: 7,
            successful_startups: 1,
            failed_startups: 2,
            authentication_failures: 1,
        }
    );
}

#[test]
fn server_metrics_snapshot_prometheus_export_is_stable() {
    let snapshot = ServerMetricsSnapshot {
        total_connections: 3,
        active_connections: 1,
        total_queries: 7,
        successful_startups: 1,
        failed_startups: 2,
        authentication_failures: 1,
    };

    assert_eq!(
        snapshot.to_prometheus_text(),
        concat!(
            "# HELP aiondb_pgwire_connections_total Total number of accepted PostgreSQL wire connections since startup.\n",
            "# TYPE aiondb_pgwire_connections_total counter\n",
            "aiondb_pgwire_connections_total 3\n",
            "# HELP aiondb_pgwire_connections_active Number of currently active PostgreSQL wire connections.\n",
            "# TYPE aiondb_pgwire_connections_active gauge\n",
            "aiondb_pgwire_connections_active 1\n",
            "# HELP aiondb_pgwire_queries_total Total number of queries executed across all PostgreSQL wire connections.\n",
            "# TYPE aiondb_pgwire_queries_total counter\n",
            "aiondb_pgwire_queries_total 7\n",
            "# HELP aiondb_pgwire_successful_startups_total Total number of successful PostgreSQL wire startup handshakes.\n",
            "# TYPE aiondb_pgwire_successful_startups_total counter\n",
            "aiondb_pgwire_successful_startups_total 1\n",
            "# HELP aiondb_pgwire_failed_startups_total Total number of failed PostgreSQL wire startup attempts.\n",
            "# TYPE aiondb_pgwire_failed_startups_total counter\n",
            "aiondb_pgwire_failed_startups_total 2\n",
            "# HELP aiondb_pgwire_authentication_failures_total Total number of PostgreSQL wire startup failures caused by authentication or authorization.\n",
            "# TYPE aiondb_pgwire_authentication_failures_total counter\n",
            "aiondb_pgwire_authentication_failures_total 1\n",
        )
    );
}

#[test]
fn server_metrics_snapshot_json_export_is_stable() {
    let snapshot = ServerMetricsSnapshot {
        total_connections: 0,
        active_connections: 0,
        total_queries: 0,
        successful_startups: 0,
        failed_startups: 0,
        authentication_failures: 0,
    };

    let exported: serde_json::Value =
        serde_json::from_str(&snapshot.to_json_string()).expect("valid json");
    assert_eq!(
        exported,
        json!({
            "total_connections": 0,
            "active_connections": 0,
            "total_queries": 0,
            "successful_startups": 0,
            "failed_startups": 0,
            "authentication_failures": 0,
        })
    );
}

#[test]
fn server_health_snapshot_defaults_to_idle() {
    let server = test_server();
    assert_eq!(
        server.health_snapshot(),
        ServerHealthSnapshot {
            state: ServerHealthState::Idle,
            accepting_connections: false,
            active_connections: 0,
        }
    );
    assert!(!server.health_snapshot().is_ready());
}

#[test]
fn server_health_snapshot_json_export_is_stable() {
    let snapshot = ServerHealthSnapshot {
        state: ServerHealthState::Ready,
        accepting_connections: true,
        active_connections: 2,
    };
    let exported: serde_json::Value =
        serde_json::from_str(&snapshot.to_json_string()).expect("valid json");
    assert_eq!(
        exported,
        json!({
            "state": "ready",
            "accepting_connections": true,
            "active_connections": 2,
            "ready": true,
        })
    );
}

#[test]
fn server_health_snapshot_reflects_ready_and_draining_states() {
    let server = test_server();

    server.accepting_connections.store(true, Ordering::Relaxed);
    server
        .metrics
        .active_connections
        .fetch_add(2, Ordering::Relaxed);
    assert_eq!(
        server.health_snapshot(),
        ServerHealthSnapshot {
            state: ServerHealthState::Ready,
            accepting_connections: true,
            active_connections: 2,
        }
    );
    assert!(server.health_snapshot().is_ready());

    server.accepting_connections.store(false, Ordering::Relaxed);
    server.draining.store(true, Ordering::Relaxed);
    assert_eq!(
        server.health_snapshot(),
        ServerHealthSnapshot {
            state: ServerHealthState::Draining,
            accepting_connections: false,
            active_connections: 2,
        }
    );
    assert!(!server.health_snapshot().is_ready());
}

#[test]
fn server_export_wrappers_reflect_current_state() {
    let server = test_server();
    server.accepting_connections.store(true, Ordering::Relaxed);
    server
        .metrics
        .total_connections
        .fetch_add(4, Ordering::Relaxed);
    server
        .metrics
        .active_connections
        .fetch_add(2, Ordering::Relaxed);
    server.metrics.total_queries.fetch_add(9, Ordering::Relaxed);
    server.metrics.record_startup_success();
    server
        .metrics
        .record_startup_failure(&DbError::invalid_authorization("bad login"));

    assert_eq!(
        server.metrics_json(),
        server.metrics_snapshot().to_json_string()
    );
    assert_eq!(
        server.metrics_prometheus_text(),
        server.metrics_snapshot().to_prometheus_text()
    );
    assert_eq!(
        server.health_json(),
        server.health_snapshot().to_json_string()
    );
}
