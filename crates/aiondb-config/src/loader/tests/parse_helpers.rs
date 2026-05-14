use super::*;

// parse_tls_mode edge cases
// -------------------------------------------------------------------

#[test]
fn parse_tls_mode_empty_string_errors() {
    let result = parse_tls_mode("");
    assert!(result.is_err());
}

#[test]
fn parse_tls_mode_whitespace_only_errors() {
    let result = parse_tls_mode("  ");
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// parse_bool edge cases
// -------------------------------------------------------------------

#[test]
fn parse_bool_true_ok() {
    assert!(parse_bool("key", "true").unwrap());
}

#[test]
fn parse_bool_false_ok() {
    assert!(!parse_bool("key", "false").unwrap());
}

#[test]
fn parse_bool_one_errors() {
    // "1" is not a valid Rust bool
    assert!(parse_bool("key", "1").is_err());
}

#[test]
fn parse_bool_uppercase_true_errors() {
    // Rust's bool::parse is case-sensitive
    assert!(parse_bool("key", "True").is_err());
}

// -------------------------------------------------------------------
// parse_u32 / parse_u64 / parse_usize edge cases
// -------------------------------------------------------------------

#[test]
fn parse_u32_zero_ok() {
    assert_eq!(parse_u32("key", "0").unwrap(), 0);
}

#[test]
fn parse_u32_max_ok() {
    assert_eq!(parse_u32("key", "4294967295").unwrap(), u32::MAX);
}

#[test]
fn parse_u64_zero_ok() {
    assert_eq!(parse_u64("key", "0").unwrap(), 0);
}

#[test]
fn parse_usize_zero_ok() {
    assert_eq!(parse_usize("key", "0").unwrap(), 0);
}

#[test]
fn parse_u32_leading_plus_errors() {
    // "+1" is not valid for parse::<u32>()
    assert!(parse_u32("key", "+1").is_err());
}

// -------------------------------------------------------------------
// All remaining config keys via load_from_map
// -------------------------------------------------------------------

#[test]
fn map_pgwire_max_connections_per_ip() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_PGWIRE_MAX_CONNECTIONS_PER_IP".to_owned(),
        "16".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.pgwire.max_connections_per_ip, 16);
}

#[test]
fn map_pgwire_startup_timeout_ms() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_PGWIRE_STARTUP_TIMEOUT_MS".to_owned(),
        "10000".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.pgwire.startup_timeout, Duration::from_secs(10));
}

#[test]
fn map_pgwire_auth_failure_backoff_ms() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_PGWIRE_AUTH_FAILURE_BACKOFF_MS".to_owned(),
        "500".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.pgwire.auth_failure_backoff, Duration::from_millis(500));
}

#[test]
fn map_engine_pool_worker_threads() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_ENGINE_POOL_WORKER_THREADS".to_owned(),
        "8".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.pgwire.engine_pool.worker_threads, 8);
}

#[test]
fn map_engine_pool_queue_depth() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_ENGINE_POOL_QUEUE_DEPTH".to_owned(),
        "2048".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.pgwire.engine_pool.queue_depth, 2048);
}

#[test]
fn map_limits_max_result_rows() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_LIMITS_MAX_RESULT_ROWS".to_owned(),
        "50000".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.limits.max_result_rows, 50000);
}

#[test]
fn map_limits_max_result_bytes() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_LIMITS_MAX_RESULT_BYTES".to_owned(),
        "1048576".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.limits.max_result_bytes, 1_048_576);
}

#[test]
fn map_limits_max_memory_bytes() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_LIMITS_MAX_MEMORY_BYTES".to_owned(),
        "134217728".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.limits.max_memory_bytes, 134_217_728);
}

#[test]
fn map_limits_max_temp_bytes() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_LIMITS_MAX_TEMP_BYTES".to_owned(),
        "536870912".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.limits.max_temp_bytes, 536_870_912);
}

#[test]
fn map_limits_max_parallel_workers_per_query() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_LIMITS_MAX_PARALLEL_WORKERS_PER_QUERY".to_owned(),
        "8".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.limits.max_parallel_workers_per_query, 8);
}

#[test]
fn map_limits_max_portals() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_LIMITS_MAX_PORTALS".to_owned(), "16".to_owned());
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.limits.max_portals, 16);
}

#[test]
fn map_limits_max_prepared_statements() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_LIMITS_MAX_PREPARED_STATEMENTS".to_owned(),
        "256".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.limits.max_prepared_statements, 256);
}

#[test]
fn map_distributed_loopback_nodes() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_LOOPBACK_NODES".to_owned(),
        "node-a,node-b,node-a".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.distributed.loopback_remote_nodes,
        vec!["node-a".to_owned(), "node-b".to_owned()]
    );
}

#[test]
fn map_distributed_loopback_nodes_rejects_empty_node_entries() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_LOOPBACK_NODES".to_owned(),
        "node-a,,node-b".to_owned(),
    );
    let error = load_from_map(entries).expect_err("empty node IDs must fail");
    assert!(error
        .to_string()
        .contains("invalid node list for AIONDB_DISTRIBUTED_LOOPBACK_NODES"));
}

#[test]
fn map_distributed_allow_unregistered_loopback_nodes_true() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_ALLOW_UNREGISTERED_LOOPBACK_NODES".to_owned(),
        "true".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert!(cfg.distributed.allow_unregistered_loopback_nodes);
}

#[test]
fn map_distributed_allow_unregistered_loopback_nodes_invalid() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_ALLOW_UNREGISTERED_LOOPBACK_NODES".to_owned(),
        "notabool".to_owned(),
    );
    let error = load_from_map(entries).expect_err("invalid bool must fail");
    assert!(error
        .to_string()
        .contains("AIONDB_DISTRIBUTED_ALLOW_UNREGISTERED_LOOPBACK_NODES"));
}

#[test]
fn map_distributed_remote_nodes() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_INTER_NODE_AUTH_TOKEN".to_owned(),
        "0123456789abcdef0123456789abcdef".to_owned(),
    );
    entries.insert(
        "AIONDB_DISTRIBUTED_REMOTE_NODES".to_owned(),
        "node-a=127.0.0.1:7543,node-b=example.internal:8123".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.distributed.remote_nodes.len(), 2);
    assert_eq!(cfg.distributed.remote_nodes[0].node_id, "node-a");
    assert_eq!(cfg.distributed.remote_nodes[0].addr, "127.0.0.1:7543");
    assert_eq!(cfg.distributed.remote_nodes[1].node_id, "node-b");
    assert_eq!(
        cfg.distributed.remote_nodes[1].addr,
        "example.internal:8123"
    );
}

#[test]
fn map_distributed_remote_nodes_rejects_duplicate_node_id() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_REMOTE_NODES".to_owned(),
        "node-a=127.0.0.1:7543,node-a=127.0.0.1:7544".to_owned(),
    );
    let error = load_from_map(entries).expect_err("duplicate remote node IDs must fail");
    assert!(error.to_string().contains("duplicate node_id \"node-a\""));
}

#[test]
fn map_distributed_remote_nodes_rejects_invalid_address() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_REMOTE_NODES".to_owned(),
        "node-a=missing-port".to_owned(),
    );
    let error = load_from_map(entries).expect_err("remote address without port must fail");
    assert!(error
        .to_string()
        .contains("invalid remote node address for AIONDB_DISTRIBUTED_REMOTE_NODES"));
}

#[test]
fn map_distributed_inter_node_auth_token() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_INTER_NODE_AUTH_TOKEN".to_owned(),
        "0123456789abcdef0123456789abcdef".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.distributed.inter_node_auth_token,
        Some("0123456789abcdef0123456789abcdef".to_owned())
    );
}

#[test]
fn map_distributed_inter_node_auth_token_empty_clears_value() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_INTER_NODE_AUTH_TOKEN".to_owned(),
        "   ".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.distributed.inter_node_auth_token, None);
}

#[test]
fn map_distributed_fragment_transport_port() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_FRAGMENT_TRANSPORT_PORT".to_owned(),
        "6543".to_owned(),
    );
    entries.insert(
        "AIONDB_DISTRIBUTED_INTER_NODE_AUTH_TOKEN".to_owned(),
        "test-token-with-at-least-thirty-two-bytes-of-entropy".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.distributed.fragment_transport_port, 6543);
}

#[test]
fn map_distributed_fragment_transport_port_rejects_invalid() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_FRAGMENT_TRANSPORT_PORT".to_owned(),
        "70000".to_owned(),
    );
    let error = load_from_map(entries).expect_err("u16 overflow must fail");
    assert!(error
        .to_string()
        .contains("AIONDB_DISTRIBUTED_FRAGMENT_TRANSPORT_PORT"));
}

#[test]
fn map_distributed_remote_snapshot_mode() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_REMOTE_SNAPSHOT_MODE".to_owned(),
        "coordinator".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.distributed.remote_snapshot_mode,
        crate::runtime::RemoteSnapshotMode::Coordinator
    );
}

#[test]
fn map_distributed_remote_snapshot_mode_rejects_invalid() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_REMOTE_SNAPSHOT_MODE".to_owned(),
        "global-magic".to_owned(),
    );
    let error = load_from_map(entries).expect_err("invalid snapshot mode must fail");
    assert!(error
        .to_string()
        .contains("invalid remote snapshot mode for AIONDB_DISTRIBUTED_REMOTE_SNAPSHOT_MODE"));
}

#[test]
fn map_distributed_remote_circuit_breaker_config() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_FAILURE_THRESHOLD".to_owned(),
        "2".to_owned(),
    );
    entries.insert(
        "AIONDB_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_RESET_TIMEOUT_MS".to_owned(),
        "1500".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.distributed.remote_circuit_breaker_failure_threshold, 2);
    assert_eq!(
        cfg.distributed.remote_circuit_breaker_reset_timeout,
        Duration::from_millis(1500)
    );
}

#[test]
fn map_distributed_remote_circuit_breaker_rejects_zero_threshold() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_FAILURE_THRESHOLD".to_owned(),
        "0".to_owned(),
    );
    let error = load_from_map(entries).expect_err("zero threshold must fail");
    assert!(error
        .to_string()
        .contains("AIONDB_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_FAILURE_THRESHOLD"));
}

#[test]
fn map_distributed_remote_circuit_breaker_rejects_zero_reset_timeout() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_RESET_TIMEOUT_MS".to_owned(),
        "0".to_owned(),
    );
    let error = load_from_map(entries).expect_err("zero reset timeout must fail");
    assert!(error
        .to_string()
        .contains("AIONDB_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_RESET_TIMEOUT_MS"));
}

#[test]
fn map_distributed_remote_client_retry_config() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_REMOTE_CONNECT_TIMEOUT_MS".to_owned(),
        "250".to_owned(),
    );
    entries.insert(
        "AIONDB_DISTRIBUTED_REMOTE_MAX_RETRIES".to_owned(),
        "1".to_owned(),
    );
    entries.insert(
        "AIONDB_DISTRIBUTED_REMOTE_RETRY_BACKOFF_MS".to_owned(),
        "25".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.distributed.remote_connect_timeout,
        Duration::from_millis(250)
    );
    assert_eq!(cfg.distributed.remote_max_retries, 1);
    assert_eq!(
        cfg.distributed.remote_retry_backoff,
        Duration::from_millis(25)
    );
}

#[test]
fn map_distributed_remote_client_retry_allows_zero_retries() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_REMOTE_MAX_RETRIES".to_owned(),
        "0".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.distributed.remote_max_retries, 0);
}

#[test]
fn map_distributed_remote_client_retry_rejects_zero_connect_timeout() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_REMOTE_CONNECT_TIMEOUT_MS".to_owned(),
        "0".to_owned(),
    );
    let error = load_from_map(entries).expect_err("zero connect timeout must fail");
    assert!(error
        .to_string()
        .contains("AIONDB_DISTRIBUTED_REMOTE_CONNECT_TIMEOUT_MS"));
}

#[test]
fn map_distributed_remote_client_retry_rejects_zero_backoff() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_DISTRIBUTED_REMOTE_RETRY_BACKOFF_MS".to_owned(),
        "0".to_owned(),
    );
    let error = load_from_map(entries).expect_err("zero retry backoff must fail");
    assert!(error
        .to_string()
        .contains("AIONDB_DISTRIBUTED_REMOTE_RETRY_BACKOFF_MS"));
}

#[test]
fn map_security_max_auth_failures() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_MAX_AUTH_FAILURES".to_owned(),
        "10".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.security.max_auth_failures, 10);
}

#[test]
fn map_security_auth_lockout_window_ms() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_AUTH_LOCKOUT_WINDOW_MS".to_owned(),
        "30000".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.security.auth_lockout_window, Duration::from_secs(30));
}

#[test]
fn map_security_durable_auth_lockout_true() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_DURABLE_AUTH_LOCKOUT".to_owned(),
        "true".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert!(cfg.security.durable_auth_lockout);
}

#[test]
fn map_security_auth_lockout_state_path() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_AUTH_LOCKOUT_STATE_PATH".to_owned(),
        "/var/lib/aiondb/lockouts.state".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.security.auth_lockout_state_path,
        Some(PathBuf::from("/var/lib/aiondb/lockouts.state"))
    );
}

#[test]
fn map_security_durable_auth_audit_true() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_DURABLE_AUTH_AUDIT".to_owned(),
        "true".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert!(cfg.security.durable_auth_audit);
}

#[test]
fn map_security_auth_audit_log_path() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_AUTH_AUDIT_LOG_PATH".to_owned(),
        "/var/lib/aiondb/auth_audit.log".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.security.auth_audit_log_path,
        Some(PathBuf::from("/var/lib/aiondb/auth_audit.log"))
    );
}

#[test]
fn map_security_max_session_idle_timeout_ms() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_MAX_SESSION_IDLE_TIMEOUT_MS".to_owned(),
        "45000".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.security.max_session_idle_timeout,
        Some(Duration::from_secs(45))
    );
}

#[test]
fn map_security_max_session_lifetime_ms() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_MAX_SESSION_LIFETIME_MS".to_owned(),
        "7200000".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.security.max_session_lifetime,
        Some(Duration::from_secs(60 * 60 * 2))
    );
}

#[test]
fn map_security_max_concurrent_sessions_per_role() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_MAX_CONCURRENT_SESSIONS_PER_ROLE".to_owned(),
        "12".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.security.max_concurrent_sessions_per_role, Some(12));
}

#[test]
fn map_security_max_transaction_idle_timeout_ms() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_MAX_TRANSACTION_IDLE_TIMEOUT_MS".to_owned(),
        "900000".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.security.max_transaction_idle_timeout,
        Some(Duration::from_secs(60 * 15))
    );
}

#[test]
fn map_security_ddl_audit_enabled() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_DDL_AUDIT_ENABLED".to_owned(),
        "true".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert!(cfg.security.ddl_audit_enabled);
}

#[test]
fn map_security_auth_audit_max_file_size_bytes() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_AUTH_AUDIT_MAX_FILE_SIZE_BYTES".to_owned(),
        "8192".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.security.auth_audit_max_file_size_bytes, 8192);
}

#[test]
fn map_security_auth_audit_max_rotated_files() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_AUTH_AUDIT_MAX_ROTATED_FILES".to_owned(),
        "7".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.security.auth_audit_max_rotated_files, 7);
}

#[test]
fn map_security_password_min_length() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_PASSWORD_MIN_LENGTH".to_owned(),
        "14".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.security.password_min_length, 14);
}

#[test]
fn map_security_reject_role_name_as_password_true() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_REJECT_ROLE_NAME_AS_PASSWORD".to_owned(),
        "true".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert!(cfg.security.reject_role_name_as_password);
}

#[test]
fn map_security_password_complexity_flags_true() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_PASSWORD_REQUIRE_LOWERCASE".to_owned(),
        "true".to_owned(),
    );
    entries.insert(
        "AIONDB_SECURITY_PASSWORD_REQUIRE_UPPERCASE".to_owned(),
        "true".to_owned(),
    );
    entries.insert(
        "AIONDB_SECURITY_PASSWORD_REQUIRE_DIGIT".to_owned(),
        "true".to_owned(),
    );
    entries.insert(
        "AIONDB_SECURITY_PASSWORD_REQUIRE_SYMBOL".to_owned(),
        "true".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert!(cfg.security.password_require_lowercase);
    assert!(cfg.security.password_require_uppercase);
    assert!(cfg.security.password_require_digit);
    assert!(cfg.security.password_require_symbol);
}

#[test]
fn map_security_require_tls_for_password_true() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_REQUIRE_TLS_FOR_PASSWORD".to_owned(),
        "true".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert!(cfg.security.require_tls_for_password);
}

#[test]
fn map_storage_max_open_files() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_STORAGE_MAX_OPEN_FILES".to_owned(), "512".to_owned());
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.storage.max_open_files, 512);
}
