use super::*;

// -------------------------------------------------------------------
// load_from_file: non-existent file -> error
// -------------------------------------------------------------------

#[test]
fn load_from_file_nonexistent_file_errors() {
    let result = load_from_file("/tmp/aiondb_does_not_exist_at_all.cfg");
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// load_from_file: empty file -> default config
// -------------------------------------------------------------------

#[test]
fn load_from_file_empty_file_gives_defaults() {
    let path = temp_config_file("empty.cfg", "");
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg, RuntimeConfig::default());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_from_file_rejects_oversized_file() {
    let content = " ".repeat(MAX_CONFIG_FILE_BYTES as usize + 1);
    let path = temp_config_file("oversized.cfg", &content);

    let result = load_from_file(&path);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("exceeds maximum"));

    let _ = std::fs::remove_file(&path);
}

// -------------------------------------------------------------------
// load_from_file: comments and blank lines are skipped
// -------------------------------------------------------------------

#[test]
fn load_from_file_comments_and_blanks_give_defaults() {
    let content = "# This is a comment\n\n  # Another comment\n  \n";
    let path = temp_config_file("comments.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg, RuntimeConfig::default());
    let _ = std::fs::remove_file(&path);
}

// -------------------------------------------------------------------
// load_from_file: valid key=value pairs
// -------------------------------------------------------------------

#[test]
fn load_from_file_valid_key_value() {
    let content = "AIONDB_STORAGE_DATA_DIR = /my/data\nAIONDB_STORAGE_PAGE_SIZE = 8192\n";
    let path = temp_config_file("valid_kv.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg.storage.data_dir, PathBuf::from("/my/data"));
    assert_eq!(
        cfg.storage.page_size,
        crate::storage::DEFAULT_STORAGE_PAGE_SIZE
    );
    let _ = std::fs::remove_file(&path);
}

// -------------------------------------------------------------------
// load_from_file: invalid line (no = sign) -> error
// -------------------------------------------------------------------

#[test]
fn load_from_file_invalid_line_no_equals() {
    let content = "THIS_HAS_NO_EQUALS_SIGN\n";
    let path = temp_config_file("bad_line.cfg", content);
    let result = load_from_file(&path);
    assert!(result.is_err());
    let _ = std::fs::remove_file(&path);
}

// -------------------------------------------------------------------
// -------------------------------------------------------------------

#[test]
fn load_from_file_unknown_keys_ignored() {
    let content = "SOME_UNKNOWN_KEY = some_value\n";
    let path = temp_config_file("unknown.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg, RuntimeConfig::default());
    let _ = std::fs::remove_file(&path);
}

// -------------------------------------------------------------------
// load_from_map: AIONDB_STORAGE_DATA_DIR sets data_dir
// -------------------------------------------------------------------

#[test]
fn map_storage_data_dir() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_STORAGE_DATA_DIR".to_owned(),
        "/custom/dir".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.storage.data_dir, PathBuf::from("/custom/dir"));
}

#[test]
fn map_storage_backend_page_engine() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_STORAGE_BACKEND".to_owned(),
        "page_engine".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.storage.backend,
        crate::storage::StorageBackend::PageEngine
    );
}

#[test]
fn map_storage_backend_invalid_errors() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_STORAGE_BACKEND".to_owned(), "banana".to_owned());
    let result = load_from_map(entries);
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// load_from_map: AIONDB_STORAGE_PAGE_SIZE
// -------------------------------------------------------------------

#[test]
fn map_storage_page_size() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_STORAGE_PAGE_SIZE".to_owned(), "8192".to_owned());
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.storage.page_size,
        crate::storage::DEFAULT_STORAGE_PAGE_SIZE
    );
}

// -------------------------------------------------------------------
// load_from_map: storage buffer pool sizing
// -------------------------------------------------------------------

#[test]
fn map_storage_table_pool_frames() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_STORAGE_TABLE_POOL_FRAMES".to_owned(),
        "512".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.storage.table_pool_frames, 512);
}

#[test]
fn map_storage_snapshot_pool_frames() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_STORAGE_SNAPSHOT_POOL_FRAMES".to_owned(),
        "96".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.storage.snapshot_pool_frames, 96);
}

#[test]
fn map_storage_wal_group_commit_delay_micros() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_STORAGE_WAL_GROUP_COMMIT_DELAY_MICROS".to_owned(),
        "2500".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.storage.wal_group_commit_delay_micros, 2500);
}

// -------------------------------------------------------------------
// load_from_map: AIONDB_PGWIRE_LISTEN_ADDR
// -------------------------------------------------------------------

#[test]
fn map_pgwire_listen_addr() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_PGWIRE_LISTEN_ADDR".to_owned(),
        "0.0.0.0:5433".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.pgwire.listen_addr, "0.0.0.0:5433");
}

// -------------------------------------------------------------------
// load_from_map: AIONDB_PGWIRE_MAX_CONNECTIONS
// -------------------------------------------------------------------

#[test]
fn map_pgwire_max_connections() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_PGWIRE_MAX_CONNECTIONS".to_owned(), "256".to_owned());
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.pgwire.max_connections, 256);
}

#[test]
fn map_replication_settings() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_REPLICATION_ROLE".to_owned(), "primary".to_owned());
    entries.insert(
        "AIONDB_REPLICATION_PRIMARY_CONNINFO".to_owned(),
        "host=primary port=5432 dbname=aion user=replica".to_owned(),
    );
    entries.insert(
        "AIONDB_REPLICATION_MAX_WAL_SENDERS".to_owned(),
        "24".to_owned(),
    );
    entries.insert(
        "AIONDB_REPLICATION_WAL_KEEP_SEGMENTS".to_owned(),
        "96".to_owned(),
    );
    entries.insert(
        "AIONDB_REPLICATION_STATUS_INTERVAL_MS".to_owned(),
        "2500".to_owned(),
    );
    entries.insert(
        "AIONDB_REPLICATION_SYNCHRONOUS_COMMIT".to_owned(),
        "true".to_owned(),
    );
    entries.insert(
        "AIONDB_REPLICATION_WAL_COMPRESSION".to_owned(),
        "lz4".to_owned(),
    );
    entries.insert(
        "AIONDB_REPLICATION_WAL_LSN_MODE".to_owned(),
        "logical".to_owned(),
    );

    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.replication.role,
        crate::replication::ReplicationRole::Primary
    );
    assert_eq!(
        cfg.replication.primary_conninfo.as_deref(),
        Some("host=primary port=5432 dbname=aion user=replica")
    );
    assert_eq!(cfg.replication.max_wal_senders, 24);
    assert_eq!(cfg.replication.wal_keep_segments, 96);
    assert_eq!(cfg.replication.status_interval, Duration::from_millis(2500));
    assert!(cfg.replication.synchronous_commit);
    assert_eq!(
        cfg.replication.write_concern,
        crate::replication::WriteConcern::Majority
    );
    assert_eq!(
        cfg.replication.wal_compression,
        crate::replication::WalCompression::Lz4
    );
    assert_eq!(
        cfg.replication.wal_lsn_mode,
        crate::replication::WalLsnMode::Logical
    );
}

#[test]
fn map_replication_wal_compression_invalid_errors() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_REPLICATION_WAL_COMPRESSION".to_owned(),
        "brotli".to_owned(),
    );

    let err = load_from_map(entries).expect_err("invalid wal compression must fail");
    assert!(err.to_string().contains("invalid WAL compression"));
}

#[test]
fn map_replication_wal_lsn_mode_invalid_errors() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_REPLICATION_WAL_LSN_MODE".to_owned(),
        "timeline".to_owned(),
    );

    let err = load_from_map(entries).expect_err("invalid wal lsn mode must fail");
    assert!(err.to_string().contains("invalid WAL LSN mode"));
}

// -------------------------------------------------------------------
// load_from_map: AIONDB_PGWIRE_TLS_MODE with all valid values
// -------------------------------------------------------------------

#[test]
fn map_tls_mode_disable_lowercase() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_PGWIRE_TLS_MODE".to_owned(), "disable".to_owned());
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.pgwire.tls_mode, TlsMode::Disable);
}

#[test]
fn map_tls_mode_prefer_lowercase() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_PGWIRE_TLS_MODE".to_owned(), "prefer".to_owned());
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.pgwire.tls_mode, TlsMode::Prefer);
}

#[test]
fn map_tls_mode_require_lowercase() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_PGWIRE_TLS_MODE".to_owned(), "require".to_owned());
    entries.insert(
        "AIONDB_PGWIRE_TLS_CERT_PATH".to_owned(),
        "/tmp/server.crt".to_owned(),
    );
    entries.insert(
        "AIONDB_PGWIRE_TLS_KEY_PATH".to_owned(),
        "/tmp/server.key".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.pgwire.tls_mode, TlsMode::Require);
}

// -------------------------------------------------------------------
// load_from_map: AIONDB_PGWIRE_TLS_MODE invalid value -> error
// -------------------------------------------------------------------

#[test]
fn map_tls_mode_invalid_errors() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_PGWIRE_TLS_MODE".to_owned(), "banana".to_owned());
    let result = load_from_map(entries);
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// load_from_map: TLS_MODE case insensitive (uppercase, mixed)
// -------------------------------------------------------------------

#[test]
fn map_tls_mode_uppercase_disable() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_PGWIRE_TLS_MODE".to_owned(), "DISABLE".to_owned());
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.pgwire.tls_mode, TlsMode::Disable);
}

#[test]
fn map_tls_mode_mixed_case_prefer() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_PGWIRE_TLS_MODE".to_owned(), "Prefer".to_owned());
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.pgwire.tls_mode, TlsMode::Prefer);
}

#[test]
fn map_tls_mode_uppercase_require() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_PGWIRE_TLS_MODE".to_owned(), "REQUIRE".to_owned());
    entries.insert(
        "AIONDB_PGWIRE_TLS_CERT_PATH".to_owned(),
        "/tmp/server.crt".to_owned(),
    );
    entries.insert(
        "AIONDB_PGWIRE_TLS_KEY_PATH".to_owned(),
        "/tmp/server.key".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.pgwire.tls_mode, TlsMode::Require);
}

#[test]
fn map_pgwire_tls_identity_paths() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_PGWIRE_TLS_CERT_PATH".to_owned(),
        "/tmp/aiondb/server.crt".to_owned(),
    );
    entries.insert(
        "AIONDB_PGWIRE_TLS_KEY_PATH".to_owned(),
        "/tmp/aiondb/server.key".to_owned(),
    );
    entries.insert(
        "AIONDB_PGWIRE_TLS_CLIENT_CA_PATH".to_owned(),
        "/tmp/aiondb/ca.pem".to_owned(),
    );

    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.pgwire.tls_cert_path,
        Some(PathBuf::from("/tmp/aiondb/server.crt"))
    );
    assert_eq!(
        cfg.pgwire.tls_key_path,
        Some(PathBuf::from("/tmp/aiondb/server.key"))
    );
    assert_eq!(
        cfg.pgwire.tls_client_ca_path,
        Some(PathBuf::from("/tmp/aiondb/ca.pem"))
    );
}

// -------------------------------------------------------------------
// load_from_map: AIONDB_LIMITS_STATEMENT_TIMEOUT_MS
// -------------------------------------------------------------------

#[test]
fn map_limits_statement_timeout_ms() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_LIMITS_STATEMENT_TIMEOUT_MS".to_owned(),
        "5000".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.limits.statement_timeout, Duration::from_secs(5));
}

// -------------------------------------------------------------------
// load_from_map: AIONDB_SECURITY_ALLOW_ANONYMOUS_LOCAL with true/false
// -------------------------------------------------------------------

#[test]
fn map_security_allow_anonymous_true() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_ALLOW_ANONYMOUS_LOCAL".to_owned(),
        "true".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert!(cfg.security.allow_anonymous_local);
}

#[test]
fn map_security_allow_anonymous_false() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_ALLOW_ANONYMOUS_LOCAL".to_owned(),
        "false".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert!(!cfg.security.allow_anonymous_local);
}

// -------------------------------------------------------------------
// load_from_map: AIONDB_SECURITY_ALLOW_ANONYMOUS_LOCAL invalid bool
// -------------------------------------------------------------------

#[test]
fn map_security_allow_anonymous_invalid_bool_errors() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_ALLOW_ANONYMOUS_LOCAL".to_owned(),
        "yes".to_owned(),
    );
    let result = load_from_map(entries);
    assert!(result.is_err());
}

#[test]
fn map_security_durable_auth_lockout_and_state_path() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_DURABLE_AUTH_LOCKOUT".to_owned(),
        "true".to_owned(),
    );
    entries.insert(
        "AIONDB_SECURITY_AUTH_LOCKOUT_STATE_PATH".to_owned(),
        "/var/lib/aiondb/security/lockouts.state".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert!(cfg.security.durable_auth_lockout);
    assert_eq!(
        cfg.security.auth_lockout_state_path,
        Some(PathBuf::from("/var/lib/aiondb/security/lockouts.state"))
    );
}

#[test]
fn map_security_durable_auth_audit_and_log_path() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_DURABLE_AUTH_AUDIT".to_owned(),
        "true".to_owned(),
    );
    entries.insert(
        "AIONDB_SECURITY_AUTH_AUDIT_LOG_PATH".to_owned(),
        "/var/lib/aiondb/security/auth_audit.log".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert!(cfg.security.durable_auth_audit);
    assert_eq!(
        cfg.security.auth_audit_log_path,
        Some(PathBuf::from("/var/lib/aiondb/security/auth_audit.log"))
    );
}

#[test]
fn map_security_auth_audit_rotation_fields() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_AUTH_AUDIT_MAX_FILE_SIZE_BYTES".to_owned(),
        "16384".to_owned(),
    );
    entries.insert(
        "AIONDB_SECURITY_AUTH_AUDIT_MAX_ROTATED_FILES".to_owned(),
        "3".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.security.auth_audit_max_file_size_bytes, 16384);
    assert_eq!(cfg.security.auth_audit_max_rotated_files, 3);
}

#[test]
fn map_security_password_policy_fields() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_PASSWORD_MIN_LENGTH".to_owned(),
        "16".to_owned(),
    );
    entries.insert(
        "AIONDB_SECURITY_REJECT_ROLE_NAME_AS_PASSWORD".to_owned(),
        "true".to_owned(),
    );
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
    assert_eq!(cfg.security.password_min_length, 16);
    assert!(cfg.security.reject_role_name_as_password);
    assert!(cfg.security.password_require_lowercase);
    assert!(cfg.security.password_require_uppercase);
    assert!(cfg.security.password_require_digit);
    assert!(cfg.security.password_require_symbol);
}

// -------------------------------------------------------------------
// Invalid u32 value -> error
// -------------------------------------------------------------------

#[test]
fn map_invalid_u32_value_errors() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_PGWIRE_MAX_CONNECTIONS".to_owned(),
        "not_a_number".to_owned(),
    );
    let result = load_from_map(entries);
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// Invalid u64 value -> error
// -------------------------------------------------------------------

#[test]
fn map_invalid_u64_value_errors() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_LIMITS_STATEMENT_TIMEOUT_MS".to_owned(),
        "abc".to_owned(),
    );
    let result = load_from_map(entries);
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// Invalid usize value -> error
// -------------------------------------------------------------------

#[test]
fn map_invalid_usize_value_errors() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_STORAGE_PAGE_SIZE".to_owned(), "xyz".to_owned());
    let result = load_from_map(entries);
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// Negative number for u32 -> error
// -------------------------------------------------------------------

#[test]
fn map_negative_u32_errors() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_PGWIRE_MAX_CONNECTIONS".to_owned(), "-1".to_owned());
    let result = load_from_map(entries);
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// Negative number for u64 -> error
// -------------------------------------------------------------------

#[test]
fn map_negative_u64_errors() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_LIMITS_MAX_RESULT_ROWS".to_owned(),
        "-100".to_owned(),
    );
    let result = load_from_map(entries);
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// Negative number for usize -> error
// -------------------------------------------------------------------

#[test]
fn map_negative_usize_errors() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_STORAGE_MAX_OPEN_FILES".to_owned(), "-5".to_owned());
    let result = load_from_map(entries);
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// Overflow u32 -> error
// -------------------------------------------------------------------

#[test]
fn map_overflow_u32_errors() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_PGWIRE_MAX_CONNECTIONS".to_owned(),
        "99999999999".to_owned(),
    );
    let result = load_from_map(entries);
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// Empty value for numeric field -> error
// -------------------------------------------------------------------

#[test]
fn map_empty_numeric_value_errors() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_STORAGE_PAGE_SIZE".to_owned(), String::new());
    let result = load_from_map(entries);
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// Floating point for u64 -> error
// -------------------------------------------------------------------

#[test]
fn map_float_for_u64_errors() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_LIMITS_MAX_RESULT_BYTES".to_owned(),
        "3.14".to_owned(),
    );
    let result = load_from_map(entries);
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// Multiple valid keys at once
// -------------------------------------------------------------------

#[test]
fn map_multiple_keys_all_applied() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_STORAGE_DATA_DIR".to_owned(), "/opt/db".to_owned());
    entries.insert("AIONDB_PGWIRE_MAX_CONNECTIONS".to_owned(), "64".to_owned());
    entries.insert(
        "AIONDB_SECURITY_ALLOW_ANONYMOUS_LOCAL".to_owned(),
        "true".to_owned(),
    );
    entries.insert("AIONDB_LIMITS_MAX_PORTALS".to_owned(), "32".to_owned());
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.storage.data_dir, PathBuf::from("/opt/db"));
    assert_eq!(cfg.pgwire.max_connections, 64);
    assert!(cfg.security.allow_anonymous_local);
    assert_eq!(cfg.limits.max_portals, 32);
}

// -------------------------------------------------------------------
// load_from_file: value with = in it (only first = splits)
// -------------------------------------------------------------------

#[test]
fn load_from_file_value_containing_equals() {
    let content = "AIONDB_STORAGE_DATA_DIR = /path/with=equals\n";
    let path = temp_config_file("equals_val.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg.storage.data_dir, PathBuf::from("/path/with=equals"));
    let _ = std::fs::remove_file(&path);
}

// -------------------------------------------------------------------
// load_from_file: whitespace around key and value is trimmed
// -------------------------------------------------------------------

#[test]
fn load_from_file_whitespace_trimmed() {
    let content = "  AIONDB_STORAGE_PAGE_SIZE  =  8192  \n";
    let path = temp_config_file("ws.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(
        cfg.storage.page_size,
        crate::storage::DEFAULT_STORAGE_PAGE_SIZE
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_from_file_security_durable_lockout_config() {
    let content = "AIONDB_SECURITY_DURABLE_AUTH_LOCKOUT = true\nAIONDB_SECURITY_AUTH_LOCKOUT_STATE_PATH = /tmp/aiondb lockout.state\n";
    let path = temp_config_file("durable_lockout.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert!(cfg.security.durable_auth_lockout);
    assert_eq!(
        cfg.security.auth_lockout_state_path,
        Some(PathBuf::from("/tmp/aiondb lockout.state"))
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_from_file_security_durable_auth_audit_config() {
    let content = "AIONDB_SECURITY_DURABLE_AUTH_AUDIT = true\nAIONDB_SECURITY_AUTH_AUDIT_LOG_PATH = /tmp/aiondb auth audit.log\n";
    let path = temp_config_file("durable_auth_audit.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert!(cfg.security.durable_auth_audit);
    assert_eq!(
        cfg.security.auth_audit_log_path,
        Some(PathBuf::from("/tmp/aiondb auth audit.log"))
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_from_file_security_auth_audit_rotation_config() {
    let content = "AIONDB_SECURITY_AUTH_AUDIT_MAX_FILE_SIZE_BYTES = 2048\nAIONDB_SECURITY_AUTH_AUDIT_MAX_ROTATED_FILES = 2\n";
    let path = temp_config_file("auth_audit_rotation.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg.security.auth_audit_max_file_size_bytes, 2048);
    assert_eq!(cfg.security.auth_audit_max_rotated_files, 2);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_from_file_security_password_policy_config() {
    let content = "AIONDB_SECURITY_PASSWORD_MIN_LENGTH = 18\nAIONDB_SECURITY_REJECT_ROLE_NAME_AS_PASSWORD = true\nAIONDB_SECURITY_PASSWORD_REQUIRE_LOWERCASE = true\nAIONDB_SECURITY_PASSWORD_REQUIRE_UPPERCASE = true\nAIONDB_SECURITY_PASSWORD_REQUIRE_DIGIT = true\nAIONDB_SECURITY_PASSWORD_REQUIRE_SYMBOL = true\n";
    let path = temp_config_file("password_policy.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg.security.password_min_length, 18);
    assert!(cfg.security.reject_role_name_as_password);
    assert!(cfg.security.password_require_lowercase);
    assert!(cfg.security.password_require_uppercase);
    assert!(cfg.security.password_require_digit);
    assert!(cfg.security.password_require_symbol);
    let _ = std::fs::remove_file(&path);
}

// -------------------------------------------------------------------
// load_from_file: mixed comments, blanks, and valid lines
// -------------------------------------------------------------------

#[test]
fn load_from_file_mixed_content() {
    let content = "\
# comment at top
AIONDB_STORAGE_DATA_DIR = /mixed

# another comment

AIONDB_PGWIRE_MAX_CONNECTIONS = 10
";
    let path = temp_config_file("mixed.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg.storage.data_dir, PathBuf::from("/mixed"));
    assert_eq!(cfg.pgwire.max_connections, 10);
    let _ = std::fs::remove_file(&path);
}

// -------------------------------------------------------------------
