use std::fs;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use aiondb_engine::{
    Action, Credential, DatabaseId, MetadataWriter, NodeId, NodeMembership, PlacementEpoch,
    QueryEngine, RelationId, ReplicaRole, SecretString, ShardDescriptor, ShardId, ShardPlacement,
    StartupParams, TransportInfo, TransportKind,
};
use aiondb_pgwire::server::ServerHealthState;
use axum::body::Body;
use axum::http::Request;
use serde_json::Value as JsonValue;
use tower::ServiceExt;

fn startup_with_network_password(user: &str, password: &str, tls: bool) -> StartupParams {
    StartupParams {
        database: "default".to_owned(),
        application_name: Some("test".to_owned()),
        options: Default::default(),
        credential: Credential::CleartextPassword {
            user: user.to_owned(),
            password: SecretString::new(password.to_owned()),
        },
        transport: TransportInfo {
            kind: TransportKind::Network {
                tls,
                peer_addr: Some("127.0.0.1:5432".to_owned()),
            },
        },
    }
}

fn test_runtime_config(storage_backend: StorageBackend) -> RuntimeConfig {
    let mut config = RuntimeConfig::default();
    apply_server_security_baseline(&mut config, storage_backend);
    config
}

fn unique_test_data_dir() -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "aiondb-server-tests-{}-{timestamp}",
        std::process::id()
    ))
}

fn test_server() -> Arc<PgWireServer<Engine>> {
    let config = test_runtime_config(StorageBackend::InMemory);
    Arc::new(PgWireServer::new_plain(
        build_server_engine(None, &config, StorageBackend::InMemory, false)
            .expect("build server test engine"),
        PgWireConfig {
            require_tls: false,
            ..PgWireConfig::default()
        },
    ))
}

#[test]
fn resolve_server_tls_allows_loopback_plaintext_when_unconfigured() {
    let config = RuntimeConfig::default();
    let (tls, require_tls) = resolve_server_tls(&config).expect("prefer without paths");
    assert!(tls.is_none());
    assert!(!require_tls);
}

#[test]
fn parse_cli_args_accepts_project_friendly_startup_flags() {
    let args = vec![
        "aiondb".to_owned(),
        "--listen-addr".to_owned(),
        "127.0.0.1:5544".to_owned(),
        "--bootstrap-user".to_owned(),
        "admin".to_owned(),
        "--bootstrap-password".to_owned(),
        "StrongPassw0rd!".to_owned(),
        "--allow-unencrypted-storage".to_owned(),
        "--observability-port".to_owned(),
        "9199".to_owned(),
        "--no-observability".to_owned(),
    ];

    let cli = parse_cli_args_from(&args);
    assert_eq!(cli.listen_addr.as_deref(), Some("127.0.0.1:5544"));
    assert_eq!(cli.bootstrap_user.as_deref(), Some("admin"));
    assert_eq!(cli.bootstrap_password.as_deref(), Some("StrongPassw0rd!"));
    assert!(cli.allow_unencrypted_storage);
    assert_eq!(cli.observability_port, Some(9199));
    assert!(cli.disable_observability);
}

#[test]
fn cli_runtime_overrides_replace_pgwire_listen_addr() {
    let mut config = RuntimeConfig::default();
    let cli = CliArgs {
        listen_addr: Some("127.0.0.1:6543".to_owned()),
        ..CliArgs {
            command: CliCommand::Serve,
            data_dir: None,
            storage_backend: None,
            listen_addr: None,
            ephemeral: false,
            bootstrap_user: None,
            bootstrap_password: None,
            allow_unencrypted_storage: false,
            observability_bind: None,
            observability_port: None,
            disable_observability: false,
            dump_output: None,
            restore_input: None,
        }
    };

    apply_cli_runtime_overrides(&cli, &mut config);
    assert_eq!(config.pgwire.listen_addr, "127.0.0.1:6543");
}

#[test]
fn storage_encryption_policy_accepts_cli_override_for_local_persistent_mode() {
    let dir = unique_test_data_dir();
    fs::create_dir_all(&dir).expect("create temp data dir");
    let result = enforce_storage_encryption_policy(StorageBackend::Durable, &dir, true);
    fs::remove_dir_all(&dir).expect("cleanup temp data dir");
    assert!(
        result.is_ok(),
        "cli override should allow local persistent startup"
    );
}

#[test]
fn resolve_server_tls_rejects_non_loopback_prefer_without_identity_files() {
    let mut config = RuntimeConfig::default();
    config.pgwire.listen_addr = "0.0.0.0:5432".to_owned();
    let err = resolve_server_tls(&config).expect_err("remote prefer without cert/key must fail");
    assert!(err.contains("tls_mode=require"));
}

#[test]
fn resolve_server_tls_rejects_non_loopback_prefer_even_with_identity_files() {
    let mut config = RuntimeConfig::default();
    config.pgwire.listen_addr = "0.0.0.0:5432".to_owned();
    config.pgwire.tls_cert_path = Some(PathBuf::from("/does/not/matter/server.crt"));
    config.pgwire.tls_key_path = Some(PathBuf::from("/does/not/matter/server.key"));
    let err = resolve_server_tls(&config)
        .expect_err("remote prefer must fail even when cert/key paths are configured");
    assert!(err.contains("tls_mode=require"));
}

#[test]
fn resolve_server_tls_rejects_require_without_identity_files() {
    let mut config = RuntimeConfig::default();
    config.pgwire.tls_mode = TlsMode::Require;
    config.pgwire.tls_cert_path = None;
    config.pgwire.tls_key_path = None;
    let err = resolve_server_tls(&config).expect_err("require without cert/key must fail");
    assert!(err.contains("requires TLS cert/key"));
}

#[test]
fn resolve_server_tls_ignores_paths_when_disabled() {
    let mut config = RuntimeConfig::default();
    config.pgwire.tls_mode = TlsMode::Disable;
    config.pgwire.tls_cert_path = Some(PathBuf::from("/does/not/matter/server.crt"));
    config.pgwire.tls_key_path = Some(PathBuf::from("/does/not/matter/server.key"));
    let (tls, require_tls) = resolve_server_tls(&config).expect("disable ignores TLS files");
    assert!(tls.is_none());
    assert!(!require_tls);
}

#[test]
fn resolve_server_tls_rejects_non_loopback_when_disabled() {
    let mut config = RuntimeConfig::default();
    config.pgwire.listen_addr = "0.0.0.0:5432".to_owned();
    config.pgwire.tls_mode = TlsMode::Disable;
    let err = resolve_server_tls(&config).expect_err("remote disable must fail");
    assert!(err.contains("tls_mode=require"));
}

#[test]
fn server_security_baseline_hardens_runtime_defaults() {
    let mut config = RuntimeConfig::default();
    config.pgwire.max_connections = 12;

    apply_server_security_baseline(&mut config, StorageBackend::Durable);

    assert!(config.security.require_tls_for_password);
    assert!(config.security.reject_role_name_as_password);
    assert!(config.security.password_require_lowercase);
    assert!(config.security.password_require_uppercase);
    assert!(config.security.password_require_digit);
    assert!(config.security.password_require_symbol);
    assert!(config.security.ddl_audit_enabled);
    assert_eq!(
        config.security.password_min_length,
        SERVER_MIN_PASSWORD_LENGTH
    );
    assert_eq!(
        config.security.max_session_idle_timeout,
        Some(SERVER_DEFAULT_MAX_SESSION_IDLE_TIMEOUT)
    );
    assert_eq!(
        config.security.max_session_lifetime,
        Some(SERVER_DEFAULT_MAX_SESSION_LIFETIME)
    );
    assert_eq!(
        config.security.max_transaction_idle_timeout,
        Some(SERVER_DEFAULT_MAX_TRANSACTION_IDLE_TIMEOUT)
    );
    assert_eq!(config.security.max_concurrent_sessions_per_role, Some(12));
    assert!(config.security.durable_auth_lockout);
    assert!(config.security.durable_auth_audit);
}

#[test]
fn server_security_baseline_keeps_ephemeral_auth_state_non_durable() {
    let mut config = RuntimeConfig::default();

    apply_server_security_baseline(&mut config, StorageBackend::InMemory);

    assert!(!config.security.durable_auth_lockout);
    assert!(!config.security.durable_auth_audit);
}

#[test]
fn persistent_server_security_baseline_satisfies_production_requirements() {
    let mut config = RuntimeConfig::default();
    config.pgwire.max_connections = 12;

    apply_server_security_baseline(&mut config, StorageBackend::Durable);

    config
        .security
        .validate_production_requirements()
        .expect("durable server baseline should satisfy production requirements");
}

#[test]
fn remote_exposure_security_allows_loopback_defaults() {
    let config = RuntimeConfig::default();
    validate_remote_exposure_security(&config).expect("loopback defaults should be allowed");
}

#[test]
fn remote_exposure_security_rejects_weak_security_controls() {
    let mut config = RuntimeConfig::default();
    config.pgwire.listen_addr = "0.0.0.0:5432".to_owned();

    let err = validate_remote_exposure_security(&config)
        .expect_err("remote exposure should require production-like security settings");
    assert!(err.contains("password_min_length"));
    assert!(err.contains("password_require_symbol"));
    assert!(err.contains("max_session_idle_timeout"));
    assert!(err.contains("ddl_audit_enabled"));
}

#[test]
fn remote_exposure_security_accepts_production_profile() {
    let mut config = RuntimeConfig::default();
    config.pgwire.listen_addr = "0.0.0.0:5432".to_owned();
    config.security =
        aiondb_config::SecurityConfig::from_profile(aiondb_config::SecurityProfile::Production);

    validate_remote_exposure_security(&config)
        .expect("production profile should satisfy remote exposure requirements");
}

#[test]
fn distributed_fragment_transport_disabled_by_default() {
    let config = RuntimeConfig::default();
    assert!(!distributed_fragment_transport_enabled(&config));
}

#[test]
fn distributed_fragment_transport_enabled_with_remote_nodes() {
    let mut config = RuntimeConfig::default();
    config
        .distributed
        .remote_nodes
        .push(aiondb_config::RemoteNodeConfig {
            node_id: "node-a".to_owned(),
            addr: "127.0.0.1:7543".to_owned(),
        });
    assert!(distributed_fragment_transport_enabled(&config));
}

#[test]
fn distributed_fragment_transport_enabled_with_auth_token() {
    let mut config = RuntimeConfig::default();
    config.distributed.inter_node_auth_token = Some("shared-secret".to_owned());
    assert!(distributed_fragment_transport_enabled(&config));
}

#[test]
fn distributed_fragment_transport_enabled_with_non_default_port() {
    let mut config = RuntimeConfig::default();
    config.distributed.fragment_transport_port = 6000;
    assert!(distributed_fragment_transport_enabled(&config));
}

#[test]
fn experimental_gate_rejects_distributed_runtime_without_opt_in() {
    let mut config = RuntimeConfig::default();
    config
        .distributed
        .remote_nodes
        .push(aiondb_config::RemoteNodeConfig {
            node_id: "node-a".to_owned(),
            addr: "127.0.0.1:7543".to_owned(),
        });

    with_env_var(EXPERIMENTAL_DISTRIBUTED_ENV, "false", || {
        let err = validate_experimental_release_gates(&config)
            .expect_err("distributed runtime must require explicit v0.1 opt-in");
        assert!(err.contains(EXPERIMENTAL_DISTRIBUTED_ENV));
    });
}

#[test]
fn experimental_gate_accepts_distributed_runtime_with_opt_in() {
    let mut config = RuntimeConfig::default();
    config.distributed.sharding.enabled = true;

    with_env_var(EXPERIMENTAL_DISTRIBUTED_ENV, "true", || {
        validate_experimental_release_gates(&config)
            .expect("explicit opt-in should allow experimental distributed runtime");
    });
}

#[test]
fn experimental_gate_rejects_ha_runtime_without_opt_in() {
    let mut config = RuntimeConfig::default();
    config.ha.enabled = true;

    with_env_var(EXPERIMENTAL_DISTRIBUTED_ENV, "false", || {
        let err = validate_experimental_release_gates(&config)
            .expect_err("HA runtime must require explicit v0.1 opt-in");
        assert!(err.contains("distributed/sharding/HA runtime is experimental"));
    });
}

#[test]
fn fragment_transport_listen_addr_uses_pgwire_bind_host() {
    let mut config = RuntimeConfig::default();
    config.pgwire.listen_addr = "0.0.0.0:5432".to_owned();
    config.distributed.fragment_transport_port = 6543;
    assert_eq!(fragment_transport_listen_addr(&config), "0.0.0.0:6543");
}

#[test]
fn fragment_transport_listen_addr_formats_ipv6_hosts() {
    let mut config = RuntimeConfig::default();
    config.pgwire.listen_addr = "[::1]:5432".to_owned();
    config.distributed.fragment_transport_port = 6543;
    assert_eq!(fragment_transport_listen_addr(&config), "[::1]:6543");
}

#[test]
fn fragment_transport_fail_fast_reads_environment_override() {
    with_env_var(FRAGMENT_TRANSPORT_FAIL_FAST_ENV, "true", || {
        assert!(fragment_transport_fail_fast());
    });
    with_env_var(FRAGMENT_TRANSPORT_FAIL_FAST_ENV, "0", || {
        assert!(!fragment_transport_fail_fast());
    });
}

#[tokio::test]
async fn init_fragment_transport_server_degrades_when_bind_fails_and_fail_fast_is_disabled() {
    let occupied_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind occupied test port");
    let occupied_port = occupied_listener
        .local_addr()
        .expect("occupied listener local addr")
        .port();

    let mut config = RuntimeConfig::default();
    config.pgwire.listen_addr = "127.0.0.1:5432".to_owned();
    config
        .distributed
        .remote_nodes
        .push(aiondb_config::RemoteNodeConfig {
            node_id: "node-a".to_owned(),
            addr: "127.0.0.1:7543".to_owned(),
        });
    config.distributed.inter_node_auth_token = Some("shared-secret".to_owned());
    config.distributed.require_tls = false;
    config.distributed.fragment_transport_port = occupied_port;

    let engine = build_server_engine(None, &config, StorageBackend::InMemory, false)
        .expect("build server test engine");
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    let task = init_fragment_transport_server(engine, &config, shutdown_rx, false).await;
    assert!(task.is_none());
}

#[test]
fn server_session_authorizer_allows_authenticated_sql_workloads() {
    let authorizer = ServerSessionAuthorizer;
    let config = test_runtime_config(StorageBackend::InMemory);
    let engine = build_server_engine(None, &config, StorageBackend::InMemory, false)
        .expect("build server engine");
    engine
        .bootstrap_role("admin", "StrongPass123!", true)
        .expect("bootstrap role");
    let (_session, info) = engine
        .startup(startup_with_network_password(
            "admin",
            "StrongPass123!",
            true,
        ))
        .expect("server-built engine should allow startup on the production path");
    let identity = info.identity;

    authorizer
        .authorize(
            &identity,
            &AccessRequest {
                action: Action::Connect,
                target: None,
            },
        )
        .expect("CONNECT should be allowed for authenticated sessions");

    authorizer
        .authorize(
            &identity,
            &AccessRequest {
                action: Action::Select,
                target: None,
            },
        )
        .expect("statement authorization should defer to catalog ACL checks");
}

#[test]
fn bench_mode_authorizer_allows_bootstrap_workloads() {
    let authorizer = server_authorizer(true);
    let config = test_runtime_config(StorageBackend::InMemory);
    let engine = build_server_engine(None, &config, StorageBackend::InMemory, false)
        .expect("build server engine");
    engine
        .bootstrap_role("admin", "StrongPass123!", true)
        .expect("bootstrap role");
    let (_session, info) = engine
        .startup(startup_with_network_password(
            "admin",
            "StrongPass123!",
            true,
        ))
        .expect("startup should succeed");
    let identity = info.identity;

    authorizer
        .authorize(
            &identity,
            &AccessRequest {
                action: Action::Connect,
                target: None,
            },
        )
        .expect("CONNECT should be allowed");

    authorizer
        .authorize(
            &identity,
            &AccessRequest {
                action: Action::Select,
                target: None,
            },
        )
        .expect("bench mode should allow SQL workloads");
}

#[test]
fn build_server_engine_allows_authenticated_connect_on_in_memory_path() {
    let config = test_runtime_config(StorageBackend::InMemory);
    let engine = build_server_engine(None, &config, StorageBackend::InMemory, false)
        .expect("build in-memory server engine");

    engine
        .bootstrap_role("admin", "StrongPass123!", true)
        .expect("bootstrap role");

    let (_session, info) = engine
        .startup(startup_with_network_password(
            "admin",
            "StrongPass123!",
            true,
        ))
        .expect("startup should succeed once CONNECT is authorized");

    assert_eq!(info.identity.user, "admin");
}

#[test]
fn build_server_engine_allows_bootstrap_superuser_sql_without_bench_mode() {
    let config = test_runtime_config(StorageBackend::InMemory);
    let engine = build_server_engine(None, &config, StorageBackend::InMemory, false)
        .expect("build in-memory server engine");

    engine
        .bootstrap_role("admin", "StrongPass123!", true)
        .expect("bootstrap role");

    let (session, info) = engine
        .startup(startup_with_network_password(
            "admin",
            "StrongPass123!",
            true,
        ))
        .expect("startup should succeed");

    assert_eq!(info.identity.user, "admin");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE cli_smoke (id INT PRIMARY KEY, title TEXT)",
        )
        .expect("bootstrap superuser should be able to execute SQL");
    engine
        .execute_sql(&session, "INSERT INTO cli_smoke VALUES (1, 'ok')")
        .expect("bootstrap superuser insert should succeed");
    let rows = engine
        .execute_sql(&session, "SELECT title FROM cli_smoke WHERE id = 1")
        .expect("bootstrap superuser select should succeed");
    assert!(matches!(
        rows.as_slice(),
        [aiondb_engine::StatementResult::Query { rows, .. }] if rows.len() == 1
    ));
}

#[test]
fn build_server_engine_allows_authenticated_connect_on_persistent_path() {
    let config = test_runtime_config(StorageBackend::Durable);
    let data_dir = unique_test_data_dir();
    let _ = fs::remove_dir_all(&data_dir);

    let engine = build_server_engine(
        Some(data_dir.clone()),
        &config,
        StorageBackend::Durable,
        false,
    )
    .expect("build persistent server engine");

    engine
        .bootstrap_role("admin", "StrongPass123!", true)
        .expect("bootstrap role");

    let (_session, info) = engine
        .startup(startup_with_network_password(
            "admin",
            "StrongPass123!",
            true,
        ))
        .expect("startup should succeed once CONNECT is authorized");

    assert_eq!(info.identity.user, "admin");
    let _ = fs::remove_dir_all(&data_dir);
}

#[test]
fn build_server_engine_supports_django_constraint_reflection_query_on_persistent_path() {
    let config = test_runtime_config(StorageBackend::Durable);
    let data_dir = unique_test_data_dir();
    let _ = fs::remove_dir_all(&data_dir);

    let engine = build_server_engine(
        Some(data_dir.clone()),
        &config,
        StorageBackend::Durable,
        false,
    )
    .expect("build persistent server engine");

    engine
        .bootstrap_role("admin", "StrongPass123!", true)
        .expect("bootstrap role");

    let (session, info) = engine
        .startup(startup_with_network_password(
            "admin",
            "StrongPass123!",
            true,
        ))
        .expect("startup should succeed");

    assert_eq!(info.identity.user, "admin");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE django_server_parent (id INT PRIMARY KEY)",
        )
        .expect("create parent table");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE django_server_child ( \
                 id INT PRIMARY KEY, \
                 parent_id INT NOT NULL REFERENCES django_server_parent(id), \
                 slug TEXT UNIQUE \
             )",
        )
        .expect("create child table");

    let rows = engine
        .execute_sql(
            &session,
            "SELECT c.conname, \
                    array( \
                        SELECT attname \
                        FROM unnest(c.conkey) WITH ORDINALITY cols(colid, arridx) \
                        JOIN pg_attribute AS ca ON cols.colid = ca.attnum \
                        WHERE ca.attrelid = c.conrelid \
                        ORDER BY cols.arridx \
                    ), \
                    c.contype, \
                    (SELECT fkc.relname || '.' || fka.attname \
                       FROM pg_attribute AS fka \
                       JOIN pg_class AS fkc ON fka.attrelid = fkc.oid \
                      WHERE fka.attrelid = c.confrelid AND fka.attnum = c.confkey[1]), \
                    cl.reloptions \
               FROM pg_constraint AS c \
               JOIN pg_class AS cl ON c.conrelid = cl.oid \
              WHERE cl.relname = 'django_server_child' \
                AND pg_catalog.pg_table_is_visible(cl.oid)",
        )
        .expect("django constraint reflection query should succeed");

    assert!(matches!(
        rows.as_slice(),
        [aiondb_engine::StatementResult::Query { rows, .. }] if rows.len() == 3
    ));

    let _ = fs::remove_dir_all(&data_dir);
}

#[test]
fn resolve_data_dir_preserves_explicit_storage_config_value() {
    let mut config = RuntimeConfig::default();
    config.storage.data_dir = PathBuf::from("/srv/aiondb");
    assert_eq!(
        resolve_data_dir(None, &config),
        PathBuf::from("/srv/aiondb")
    );
}

#[test]
fn resolve_storage_backend_prefers_ephemeral() {
    let mut config = RuntimeConfig::default();
    config.storage.backend = StorageBackend::PageEngine;

    assert_eq!(
        resolve_storage_backend(Some(StorageBackend::Lsm), true, &config),
        StorageBackend::InMemory
    );
}

#[test]
fn resolve_storage_backend_prefers_cli_backend_over_config() {
    let mut config = RuntimeConfig::default();
    config.storage.backend = StorageBackend::Durable;

    assert_eq!(
        resolve_storage_backend(Some(StorageBackend::Disk), false, &config),
        StorageBackend::Disk
    );
}

fn with_env_var(key: &'static str, value: &'static str, test_fn: impl FnOnce()) {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("environment lock must not be poisoned");

    let previous = std::env::var_os(key);
    std::env::set_var(key, value);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(test_fn));
    match previous {
        Some(previous_value) => std::env::set_var(key, previous_value),
        None => std::env::remove_var(key),
    }
    if let Err(panic_payload) = result {
        std::panic::resume_unwind(panic_payload);
    }
}

#[test]
fn enforce_storage_encryption_policy_allows_in_memory_unconditionally() {
    let tmp = Path::new("/tmp");
    assert!(enforce_storage_encryption_policy(StorageBackend::InMemory, tmp, false).is_ok());
}

#[test]
fn enforce_storage_encryption_policy_rejects_persistent_on_plain_fs() {
    // Only meaningful on systems where /tmp is NOT on LUKS.
    let tmp = Path::new("/tmp/aiondb-test-plain");
    if !path_is_on_encrypted_device(tmp) {
        with_env_var("AIONDB_ALLOW_UNENCRYPTED_STORAGE", "false", || {
            let result = enforce_storage_encryption_policy(StorageBackend::Durable, tmp, false);
            assert!(result.is_err());
        });
    }
}

#[test]
fn enforce_storage_encryption_policy_allows_in_memory() {
    let tmp = Path::new("/tmp");
    assert!(enforce_storage_encryption_policy(StorageBackend::InMemory, tmp, false).is_ok());
}

#[test]
fn enforce_storage_encryption_policy_rejects_persistent_on_unencrypted_without_override() {
    // /tmp is (almost certainly) not on a LUKS volume in CI.
    let tmp = Path::new("/tmp/aiondb-test-unencrypted");
    with_env_var("AIONDB_ALLOW_UNENCRYPTED_STORAGE", "false", || {
        let result = enforce_storage_encryption_policy(StorageBackend::Durable, tmp, false);
        // If LUKS is detected the test passes anyway (the policy is satisfied).
        // On a typical CI runner without LUKS this is Err.
        if !path_is_on_encrypted_device(tmp) {
            assert!(result.is_err());
        }
    });
}

#[test]
fn enforce_storage_encryption_policy_allows_persistent_with_unencrypted_override() {
    let tmp = Path::new("/tmp/aiondb-test-unencrypted");
    with_env_var("AIONDB_ALLOW_UNENCRYPTED_STORAGE", "true", || {
        assert!(enforce_storage_encryption_policy(StorageBackend::Durable, tmp, false).is_ok());
    });
}

#[test]
fn observability_bind_policy_rejects_public_without_override() {
    let result = enforce_observability_bind_policy_with_override("0.0.0.0", false);
    assert!(result.is_err());
}

#[test]
fn observability_bind_policy_allows_public_with_override() {
    let result = enforce_observability_bind_policy_with_override("0.0.0.0", true);
    assert!(result.is_ok());
}

#[test]
fn observability_bind_policy_allows_loopback_without_override() {
    let result = enforce_observability_bind_policy_with_override("127.0.0.1", false);
    assert!(result.is_ok());
}

#[test]
fn health_status_code_matches_readiness_state() {
    let ready = ServerHealthSnapshot {
        state: ServerHealthState::Ready,
        accepting_connections: true,
        active_connections: 0,
    };
    let idle = ServerHealthSnapshot {
        state: ServerHealthState::Idle,
        accepting_connections: false,
        active_connections: 0,
    };
    let draining = ServerHealthSnapshot {
        state: ServerHealthState::Draining,
        accepting_connections: false,
        active_connections: 1,
    };

    assert_eq!(health_status_code(ready), StatusCode::OK);
    assert_eq!(health_status_code(idle), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        health_status_code(draining),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn health_handler_returns_503_when_server_is_not_ready() {
    let state = Arc::new(ObservabilityState {
        server: test_server(),
        replica_metrics: None,
    });
    let response = health_handler(State(state)).await.into_response();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn readyz_route_uses_health_readiness_state() {
    let app = observability_router(test_server(), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("readyz response");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn livez_route_reports_observability_liveness() {
    let app = observability_router(test_server(), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/livez")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("livez response");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn init_observability_server_degrades_when_policy_fails_and_fail_fast_is_disabled() {
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = init_observability_server(
        test_server(),
        None,
        ObservabilityConfig {
            bind_address: "0.0.0.0".to_owned(),
            port: DEFAULT_OBSERVABILITY_PORT,
        },
        shutdown_rx,
        false,
    )
    .await;

    assert!(task.is_none());
}

#[test]
fn memory_guard_clamps_unsafe_limits_on_8gb_host() {
    let mut config = RuntimeConfig::default();
    config.limits.max_memory_bytes = GIB;
    config.limits.max_temp_bytes = 4 * GIB;
    config.limits.max_result_rows = 1_000_000;
    config.limits.max_result_bytes = 128 * MIB;
    config.limits.max_portals = 1_000;
    config.limits.max_prepared_statements = 2_000;
    config.limits.max_recursive_rows = 5_000_000;
    config.limits.max_recursive_iterations = 100_000;
    config.pgwire.max_connections = 512;
    config.pgwire.max_connections_per_ip = 512;
    config.pgwire.engine_pool.worker_threads = 64;
    config.pgwire.engine_pool.queue_depth = 10_000;

    apply_memory_safety_guard_with_host_memory(&mut config, 8 * GIB);

    assert!(config.limits.max_memory_bytes <= MEMORY_GUARD_TIER8_MAX_MEMORY_BYTES);
    assert!(config.limits.max_temp_bytes <= MEMORY_GUARD_TIER8_MAX_TEMP_BYTES);
    assert!(config.limits.max_result_rows <= MEMORY_GUARD_TIER8_MAX_RESULT_ROWS);
    assert!(config.limits.max_result_bytes <= MEMORY_GUARD_TIER8_MAX_RESULT_BYTES);
    assert!(config.limits.max_portals <= MEMORY_GUARD_TIER8_MAX_PORTALS);
    assert!(config.limits.max_prepared_statements <= MEMORY_GUARD_TIER8_MAX_PREPARED);
    assert!(config.limits.max_recursive_rows <= MEMORY_GUARD_TIER8_MAX_RECURSIVE_ROWS);
    assert!(config.limits.max_recursive_iterations <= MEMORY_GUARD_TIER8_MAX_RECURSIVE_ITERS);
    assert!(config.pgwire.max_connections <= MEMORY_GUARD_TIER8_MAX_CONNECTIONS);
    assert!(config.pgwire.max_connections_per_ip <= config.pgwire.max_connections);
    assert!(config.pgwire.engine_pool.worker_threads <= MEMORY_GUARD_TIER8_MAX_WORKERS);
    assert!(config.pgwire.engine_pool.queue_depth <= 512);
}

#[test]
fn memory_guard_keeps_stricter_user_limits() {
    let mut config = RuntimeConfig::default();
    config.limits.max_memory_bytes = 32 * MIB;
    config.limits.max_temp_bytes = 64 * MIB;
    config.pgwire.max_connections = 4;
    config.pgwire.max_connections_per_ip = 2;
    config.pgwire.engine_pool.worker_threads = 2;
    config.pgwire.engine_pool.queue_depth = 32;

    apply_memory_safety_guard_with_host_memory(&mut config, 16 * GIB);

    assert_eq!(config.limits.max_memory_bytes, 32 * MIB);
    assert_eq!(config.limits.max_temp_bytes, 64 * MIB);
    assert_eq!(config.pgwire.max_connections, 4);
    assert_eq!(config.pgwire.max_connections_per_ip, 2);
    assert_eq!(config.pgwire.engine_pool.worker_threads, 2);
    assert_eq!(config.pgwire.engine_pool.queue_depth, 32);
}

#[test]
fn memory_guard_clamps_unsafe_limits_on_16gb_host() {
    let mut config = RuntimeConfig::default();
    config.limits.max_memory_bytes = GIB;
    config.limits.max_temp_bytes = 2 * GIB;
    config.limits.max_result_rows = 500_000;
    config.limits.max_result_bytes = 64 * MIB;
    config.limits.max_portals = 10_000;
    config.limits.max_prepared_statements = 20_000;
    config.limits.max_recursive_rows = 5_000_000;
    config.limits.max_recursive_iterations = 100_000;
    config.pgwire.max_connections = 128;
    config.pgwire.engine_pool.worker_threads = 32;

    apply_memory_safety_guard_with_host_memory(&mut config, 16 * GIB);

    assert!(config.limits.max_memory_bytes <= MEMORY_GUARD_TIER16_MAX_MEMORY_BYTES);
    assert!(config.limits.max_temp_bytes <= MEMORY_GUARD_TIER16_MAX_TEMP_BYTES);
    assert!(config.limits.max_result_rows <= MEMORY_GUARD_TIER16_MAX_RESULT_ROWS);
    assert!(config.limits.max_result_bytes <= MEMORY_GUARD_TIER16_MAX_RESULT_BYTES);
    assert!(config.limits.max_portals <= MEMORY_GUARD_TIER16_MAX_PORTALS);
    assert!(config.limits.max_prepared_statements <= MEMORY_GUARD_TIER16_MAX_PREPARED);
    assert!(config.limits.max_recursive_rows <= MEMORY_GUARD_TIER16_MAX_RECURSIVE_ROWS);
    assert!(config.limits.max_recursive_iterations <= MEMORY_GUARD_TIER16_MAX_RECURSIVE_ITERS);
    assert!(config.pgwire.max_connections <= MEMORY_GUARD_TIER16_MAX_CONNECTIONS);
    assert!(config.pgwire.engine_pool.worker_threads <= MEMORY_GUARD_TIER16_MAX_WORKERS);
}

#[test]
fn observability_metrics_include_product_contract_gauges() {
    let server = test_server();
    let metrics = observability_metrics_prometheus_text(server.as_ref(), None);
    assert!(metrics.contains("aiondb_product_single_node_mode 1"));
    assert!(metrics.contains("aiondb_product_clustering_supported 0"));
    assert!(metrics.contains("aiondb_product_encryption_at_rest_supported 0"));
    assert!(metrics.contains("aiondb_product_backup_restore_supported 0"));
    assert!(metrics.contains("aiondb_distributed_remote_nodes_total 0"));
    assert!(metrics.contains("aiondb_distributed_control_plane_nodes_total 1"));
    assert!(metrics.contains("aiondb_distributed_control_plane_nodes_live 1"));
}

#[test]
fn observability_metrics_include_distributed_remote_node_gauges() {
    let mut config = test_runtime_config(StorageBackend::InMemory);
    config.distributed.remote_nodes = vec![aiondb_config::RemoteNodeConfig {
        node_id: "node-b".to_owned(),
        addr: "127.0.0.1:9100".to_owned(),
    }];
    config.distributed.inter_node_auth_token = Some("shared-secret".to_owned());
    config.distributed.require_tls = false;
    let engine = build_server_engine(None, &config, StorageBackend::InMemory, false)
        .expect("build distributed server test engine");
    let server = PgWireServer::new_plain(
        engine,
        PgWireConfig {
            require_tls: false,
            ..PgWireConfig::default()
        },
    );

    let metrics = observability_metrics_prometheus_text(&server, None);

    assert!(metrics.contains("aiondb_distributed_remote_nodes_total 1"));
    assert!(metrics.contains("aiondb_distributed_remote_nodes_available 1"));
    assert!(metrics.contains("aiondb_distributed_remote_circuits_open 0"));
    assert!(metrics.contains(
        "aiondb_distributed_remote_node_available{node_id=\"node-b\",addr=\"127.0.0.1:9100\"} 1"
    ));
    assert!(metrics.contains(
        "aiondb_distributed_remote_node_circuit_state{node_id=\"node-b\",addr=\"127.0.0.1:9100\"} 0"
    ));
    assert!(metrics.contains("aiondb_distributed_control_plane_nodes_total 2"));
    assert!(metrics.contains("aiondb_distributed_control_plane_nodes_live 2"));
    assert!(metrics.contains(
        "aiondb_distributed_control_plane_node_live{node_id=\"node-b\",endpoint=\"127.0.0.1:9100\"} 1"
    ));
}

#[test]
fn observability_metrics_include_distributed_replication_health_gauges() {
    let mut config = test_runtime_config(StorageBackend::InMemory);
    config.distributed.sharding.enabled = true;
    config.distributed.sharding.replication_factor = 2;
    config.distributed.sharding.auto_rebalance = false;
    config.distributed.remote_nodes = vec![
        aiondb_config::RemoteNodeConfig {
            node_id: "node-b".to_owned(),
            addr: "127.0.0.1:9100".to_owned(),
        },
        aiondb_config::RemoteNodeConfig {
            node_id: "node-c".to_owned(),
            addr: "127.0.0.1:9101".to_owned(),
        },
    ];
    config.distributed.inter_node_auth_token = Some("shared-secret".to_owned());
    config.distributed.require_tls = false;
    let engine = build_server_engine(None, &config, StorageBackend::InMemory, false)
        .expect("build sharded replication metrics test engine");
    engine
        .distributed_control_plane()
        .upsert_shard(ShardDescriptor {
            database_id: DatabaseId::DEFAULT,
            table_id: RelationId::new(42),
            shard_id: ShardId::new(7),
            placements: vec![
                ShardPlacement {
                    shard_id: ShardId::new(7),
                    node_id: NodeId::new("local"),
                    role: ReplicaRole::Leader,
                    lease_epoch: PlacementEpoch::default(),
                },
                ShardPlacement {
                    shard_id: ShardId::new(7),
                    node_id: NodeId::new("node-b"),
                    role: ReplicaRole::Follower,
                    lease_epoch: PlacementEpoch::default(),
                },
                ShardPlacement {
                    shard_id: ShardId::new(7),
                    node_id: NodeId::new("node-c"),
                    role: ReplicaRole::Follower,
                    lease_epoch: PlacementEpoch::default(),
                },
            ],
        })
        .expect("seed sharded placement");
    engine
        .distributed_control_plane()
        .mark_node_live(&NodeId::new("node-b"), false)
        .expect("mark one follower down");
    let server = PgWireServer::new_plain(
        engine,
        PgWireConfig {
            require_tls: false,
            ..PgWireConfig::default()
        },
    );

    let metrics = observability_metrics_prometheus_text(&server, None);

    assert!(metrics.contains("aiondb_distributed_replication_shards_total 1"));
    assert!(metrics.contains("aiondb_distributed_replication_shards_with_live_quorum 1"));
    assert!(metrics.contains("aiondb_distributed_replication_shards_without_live_quorum 0"));
    assert!(metrics.contains("aiondb_distributed_replication_under_replicated_shards 0"));
    assert!(metrics.contains("aiondb_distributed_replication_shards_with_down_voters 1"));
    assert!(metrics.contains("aiondb_distributed_replication_shards_with_learners 0"));
    assert!(metrics.contains("aiondb_distributed_replication_learner_replicas 0"));
    assert!(metrics.contains(
        "aiondb_distributed_replication_shard_live_quorum{database_id=\"1\",table_id=\"42\",shard_id=\"7\",leader=\"local\"} 1"
    ));
    assert!(metrics.contains(
        "aiondb_distributed_replication_shard_down_voters{database_id=\"1\",table_id=\"42\",shard_id=\"7\",leader=\"local\"} 1"
    ));
    assert!(metrics.contains(
        "aiondb_distributed_replication_shard_learners{database_id=\"1\",table_id=\"42\",shard_id=\"7\",leader=\"local\"} 0"
    ));
    assert!(metrics.contains(
        "aiondb_distributed_replication_node_leaders{node_id=\"local\",registered=\"true\",live=\"true\"} 1"
    ));
    assert!(metrics.contains(
        "aiondb_distributed_replication_node_voters{node_id=\"node-b\",registered=\"true\",live=\"false\"} 1"
    ));
    assert!(metrics.contains(
        "aiondb_distributed_replication_node_down_voters{node_id=\"node-b\",registered=\"true\",live=\"false\"} 1"
    ));
}

#[test]
fn observability_metrics_include_replica_runtime_counters_when_present() {
    let server = test_server();
    let replica_metrics = aiondb_replication::ReplicaMetrics::new();

    let metrics = observability_metrics_prometheus_text(server.as_ref(), Some(&replica_metrics));

    assert!(metrics.contains("aiondb_replica_runtime_sessions_started 0"));
    assert!(metrics.contains("aiondb_replica_runtime_sessions_succeeded 0"));
    assert!(metrics.contains("aiondb_replica_runtime_sessions_failed 0"));
    assert!(metrics.contains("aiondb_replica_runtime_reconnects 0"));
    assert!(metrics.contains("aiondb_replica_runtime_wal_bytes_received 0"));
    assert!(metrics.contains("aiondb_replica_runtime_standby_status_updates_sent 0"));
    assert!(metrics.contains("aiondb_replica_runtime_last_session_started_at_us 0"));
}

#[test]
fn observability_metrics_include_replica_wal_receiver_progress_for_replica_engine() {
    let data_dir = unique_test_data_dir();
    let mut config = test_runtime_config(StorageBackend::Durable);
    config.replication.role = aiondb_config::ReplicationRole::Replica;
    let engine = build_server_engine(
        Some(data_dir.clone()),
        &config,
        StorageBackend::Durable,
        false,
    )
    .expect("build replica server test engine");
    let server = PgWireServer::new_plain(
        engine,
        PgWireConfig {
            require_tls: false,
            ..PgWireConfig::default()
        },
    );

    let metrics = observability_metrics_prometheus_text(&server, None);

    assert!(metrics.contains("aiondb_replica_wal_receiver_write_lsn 0"));
    assert!(metrics.contains("aiondb_replica_wal_receiver_flush_lsn 0"));
    assert!(metrics.contains("aiondb_replica_wal_receiver_apply_lsn 0"));
    assert!(metrics.contains("aiondb_replica_wal_receiver_write_apply_lag_lsn 0"));
    assert!(metrics.contains("aiondb_replica_wal_receiver_flush_apply_lag_lsn 0"));
    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn prometheus_label_escape_handles_special_characters() {
    assert_eq!(
        prometheus_escape_label_value("node\"x\\y\nz"),
        "node\\\"x\\\\y\\nz"
    );
}

#[test]
fn observability_info_reports_current_contract() {
    let payload = observability_info_payload();
    let JsonValue::Object(map) = payload else {
        panic!("observability info must be a JSON object");
    };
    assert_eq!(
        map.get("release_line"),
        Some(&JsonValue::String("0.1".into()))
    );
    assert_eq!(
        map.get("deployment")
            .and_then(|v| v.get("clustering"))
            .and_then(JsonValue::as_str),
        Some("unsupported")
    );
    assert_eq!(
        map.get("storage")
            .and_then(|v| v.get("encryption_at_rest"))
            .and_then(JsonValue::as_str),
        Some("unsupported")
    );
}
