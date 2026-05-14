use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read as _,
    path::Path,
    time::Duration,
};

use aiondb_core::{DbError, DbResult};
use aiondb_shard::{
    MAX_STORAGE_HASH_RING_VIRTUAL_NODES, MAX_STORAGE_SHARD_COUNT,
    MAX_STORAGE_VIRTUAL_NODES_PER_SHARD,
};
use tracing::warn;

use crate::{
    pgwire::TlsMode,
    replication::{ReplicationRole, WalCompression, WalLsnMode},
    runtime::{RemoteNodeConfig, RemoteSnapshotMode, RuntimeConfig},
    security::{SecurityConfig, SecurityProfile},
    storage::{DurableWalCommitPolicy, StorageBackend},
};

const MIN_INTER_NODE_AUTH_TOKEN_BYTES: usize = 32;
const MAX_CONFIG_FILE_BYTES: u64 = 1024 * 1024;

pub fn load_from_env() -> DbResult<RuntimeConfig> {
    let entries = std::env::vars().collect::<HashMap<_, _>>();
    load_from_map(entries)
}

pub fn load_from_file(path: impl AsRef<Path>) -> DbResult<RuntimeConfig> {
    let path = path.as_ref();
    let content = read_config_file_to_string(path)?;

    let mut entries = HashMap::new();
    for (line_no, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(DbError::internal(format!(
                "invalid config line {}: expected KEY=VALUE",
                line_no + 1
            )));
        };

        entries.insert(key.trim().to_owned(), value.trim().to_owned());
    }

    load_from_map(entries)
}

fn read_config_file_to_string(path: &Path) -> DbResult<String> {
    let file = fs::File::open(path).map_err(|error| {
        DbError::internal(format!(
            "failed to read config file {}: {error}",
            path.display()
        ))
    })?;
    let file_len = file
        .metadata()
        .map_err(|error| {
            DbError::internal(format!(
                "failed to read config file metadata {}: {error}",
                path.display()
            ))
        })?
        .len();
    if file_len > MAX_CONFIG_FILE_BYTES {
        return Err(DbError::internal(format!(
            "config file {} exceeds maximum size of {MAX_CONFIG_FILE_BYTES} bytes",
            path.display()
        )));
    }

    let capacity = usize::try_from(file_len).map_err(|_| {
        DbError::internal(format!(
            "config file {} size {file_len} does not fit in usize",
            path.display()
        ))
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut limited = file.take(MAX_CONFIG_FILE_BYTES.saturating_add(1));
    limited.read_to_end(&mut bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to read config file {}: {error}",
            path.display()
        ))
    })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CONFIG_FILE_BYTES {
        return Err(DbError::internal(format!(
            "config file {} grew while reading and exceeds maximum size of {MAX_CONFIG_FILE_BYTES} bytes",
            path.display()
        )));
    }

    String::from_utf8(bytes).map_err(|error| {
        DbError::internal(format!(
            "config file {} is not valid UTF-8: {error}",
            path.display()
        ))
    })
}

fn load_from_map(entries: HashMap<String, String>) -> DbResult<RuntimeConfig> {
    let mut config = RuntimeConfig::default();
    let mut write_concern_was_explicit = false;

    // `AIONDB_CONFIG_STRICT=true` makes unknown keys fail fast.
    // Default: permissive handling for backward compatibility.
    let strict_mode = entries
        .get("AIONDB_CONFIG_STRICT")
        .is_some_and(|value| value.eq_ignore_ascii_case("true") || value == "1");

    // PRE-PASS: apply `AIONDB_SECURITY_PROFILE` first so the wholesale
    // `from_profile(...)` swap can't clobber sibling `AIONDB_SECURITY_*`
    // overrides that happen to be processed earlier in HashMap iteration
    // order (which is non-deterministic and would otherwise produce
    // inconsistent configs across runs).
    if let Some(value) = entries.get("AIONDB_SECURITY_PROFILE") {
        let profile = match value.to_lowercase().as_str() {
            "development" | "dev" => SecurityProfile::Development,
            "staging" => SecurityProfile::Staging,
            "production" | "prod" => SecurityProfile::Production,
            _ => {
                return Err(DbError::internal(format!(
                    "AIONDB_SECURITY_PROFILE: expected one of development, staging, production (got {value})"
                )));
            }
        };
        config.security = SecurityConfig::from_profile(profile);
    }

    for (key, value) in &entries {
        match key.as_str() {
            // Meta-key: consumed above, not a config field.
            "AIONDB_CONFIG_STRICT" => {}
            // Server/runtime-only keys consumed outside `aiondb-config`.
            // They must be recognized here so strict mode remains compatible.
            "AIONDB_IN_MEMORY"
            | "AIONDB_ALLOW_UNENCRYPTED_STORAGE"
            | "AIONDB_OBSERVABILITY_BIND"
            | "AIONDB_OBSERVABILITY_PORT"
            | "AIONDB_OBSERVABILITY_FAIL_FAST"
            | "AIONDB_DISTRIBUTED_FRAGMENT_TRANSPORT_FAIL_FAST"
            | "AIONDB_ALLOW_PUBLIC_OBSERVABILITY"
            | "AIONDB_DISABLE_MEMORY_GUARD"
            | "AIONDB_ENGINE_DISABLE_PARSED_SQL_FINGERPRINT_CACHE"
            | "AIONDB_REPLICATION_PROMOTE_ON_START"
            | "AIONDB_PGWIRE_COPY_IN_MAX_BUFFER"
            | "AIONDB_PGWIRE_COPY_IN_TOTAL_TIMEOUT_MS"
            | "AIONDB_STORAGE_MAX_SNAPSHOT_BYTES" => {}

            "AIONDB_STORAGE_BACKEND" => {
                config.storage.backend = parse_storage_backend(key, value)?;
            }
            "AIONDB_STORAGE_DATA_DIR" => {
                config.storage.data_dir = value.as_str().into();
            }
            "AIONDB_STORAGE_PAGE_SIZE" => {
                config.storage.page_size = parse_usize(key, value)?;
            }
            "AIONDB_STORAGE_MAX_OPEN_FILES" => {
                config.storage.max_open_files = parse_usize(key, value)?;
            }
            "AIONDB_STORAGE_TABLE_POOL_FRAMES" => {
                config.storage.table_pool_frames = parse_usize(key, value)?;
            }
            "AIONDB_STORAGE_SNAPSHOT_POOL_FRAMES" => {
                config.storage.snapshot_pool_frames = parse_usize(key, value)?;
            }
            "AIONDB_STORAGE_DURABLE_WAL_COMMIT_POLICY" => {
                config.storage.durable_wal_commit_policy =
                    parse_durable_wal_commit_policy(key, value)?;
            }
            "AIONDB_STORAGE_WAL_GROUP_COMMIT_DELAY_MICROS" => {
                config.storage.wal_group_commit_delay_micros = parse_u64(key, value)?;
            }
            "AIONDB_PGWIRE_LISTEN_ADDR" => {
                config.pgwire.listen_addr.clone_from(value);
            }
            "AIONDB_PGWIRE_MAX_CONNECTIONS" => {
                config.pgwire.max_connections = parse_u32(key, value)?;
            }
            "AIONDB_PGWIRE_MAX_CONNECTIONS_PER_IP" => {
                config.pgwire.max_connections_per_ip = parse_u32(key, value)?;
            }
            "AIONDB_PGWIRE_STARTUP_TIMEOUT_MS" => {
                // Floor at 1ms so a typo of `0` does not disable the
                // slow-loris guard entirely (audit config M-04).
                let raw = parse_u64(key, value)?;
                config.pgwire.startup_timeout = Duration::from_millis(raw.max(1));
            }
            "AIONDB_PGWIRE_AUTH_FAILURE_BACKOFF_MS" => {
                // Floor at 50ms so a typo of `0` does not turn off the
                // brute-force throttle (audit config M-04).
                let raw = parse_u64(key, value)?;
                config.pgwire.auth_failure_backoff = Duration::from_millis(raw.max(50));
            }
            "AIONDB_PGWIRE_IDLE_TIMEOUT_MS" => {
                config.pgwire.idle_timeout = Duration::from_millis(parse_u64(key, value)?);
            }
            "AIONDB_PGWIRE_TLS_MODE" => {
                config.pgwire.tls_mode = parse_tls_mode(value)?;
            }
            "AIONDB_PGWIRE_TLS_CERT_PATH" => {
                config.pgwire.tls_cert_path = Some(value.as_str().into());
            }
            "AIONDB_PGWIRE_TLS_KEY_PATH" => {
                config.pgwire.tls_key_path = Some(value.as_str().into());
            }
            "AIONDB_PGWIRE_TLS_CLIENT_CA_PATH" => {
                config.pgwire.tls_client_ca_path = Some(value.as_str().into());
            }
            "AIONDB_ENGINE_POOL_WORKER_THREADS" => {
                config.pgwire.engine_pool.worker_threads = parse_usize(key, value)?;
            }
            "AIONDB_ENGINE_POOL_QUEUE_DEPTH" => {
                config.pgwire.engine_pool.queue_depth = parse_usize(key, value)?;
            }
            "AIONDB_REPLICATION_ROLE" => {
                config.replication.role = parse_replication_role(key, value)?;
            }
            "AIONDB_REPLICATION_PRIMARY_CONNINFO" => {
                config.replication.primary_conninfo = Some(value.clone());
            }
            "AIONDB_REPLICATION_MAX_WAL_SENDERS" => {
                config.replication.max_wal_senders = parse_u32(key, value)?;
            }
            "AIONDB_REPLICATION_WAL_KEEP_SEGMENTS" => {
                config.replication.wal_keep_segments = parse_u32(key, value)?;
            }
            "AIONDB_REPLICATION_STATUS_INTERVAL_MS" => {
                config.replication.status_interval = Duration::from_millis(parse_u64(key, value)?);
            }
            "AIONDB_REPLICATION_SYNCHRONOUS_COMMIT" => {
                config.replication.synchronous_commit = parse_bool(key, value)?;
            }
            "AIONDB_REPLICATION_WAL_COMPRESSION" => {
                config.replication.wal_compression = parse_wal_compression(key, value)?;
            }
            "AIONDB_REPLICATION_WAL_LSN_MODE" => {
                config.replication.wal_lsn_mode = parse_wal_lsn_mode(key, value)?;
            }
            "AIONDB_REPLICATION_WRITE_CONCERN" => {
                write_concern_was_explicit = true;
                config.replication.write_concern = parse_write_concern(key, value)?;
            }
            "AIONDB_REPLICATION_FACTOR" => {
                config.replication.replication_factor = parse_u32(key, value)?;
            }
            "AIONDB_REPLICATION_SYNC_COMMIT_TIMEOUT_MS" => {
                config.replication.sync_commit_timeout =
                    Duration::from_millis(parse_u64(key, value)?);
            }
            "AIONDB_LIMITS_STATEMENT_TIMEOUT_MS" => {
                config.limits.statement_timeout = Duration::from_millis(parse_u64(key, value)?);
            }
            "AIONDB_LIMITS_LOCK_TIMEOUT_MS" => {
                config.limits.lock_timeout = Duration::from_millis(parse_u64(key, value)?);
            }
            "AIONDB_LIMITS_MAX_RESULT_ROWS" => {
                config.limits.max_result_rows = parse_u64(key, value)?;
            }
            "AIONDB_LIMITS_MAX_RESULT_BYTES" => {
                config.limits.max_result_bytes = parse_u64(key, value)?;
            }
            "AIONDB_LIMITS_MAX_MEMORY_BYTES" => {
                config.limits.max_memory_bytes = parse_u64(key, value)?;
            }
            "AIONDB_LIMITS_MAX_TEMP_BYTES" => {
                config.limits.max_temp_bytes = parse_u64(key, value)?;
            }
            "AIONDB_LIMITS_MAX_PARALLEL_WORKERS_PER_QUERY" => {
                config.limits.max_parallel_workers_per_query = parse_usize(key, value)?;
            }
            "AIONDB_LIMITS_MAX_PORTALS" => {
                config.limits.max_portals = parse_usize(key, value)?;
            }
            "AIONDB_LIMITS_MAX_PREPARED_STATEMENTS" => {
                config.limits.max_prepared_statements = parse_usize(key, value)?;
            }
            "AIONDB_LIMITS_MAX_RECURSIVE_ITERATIONS" => {
                config.limits.max_recursive_iterations = parse_usize(key, value)?;
            }
            "AIONDB_LIMITS_MAX_RECURSIVE_ROWS" => {
                config.limits.max_recursive_rows = parse_usize(key, value)?;
            }
            "AIONDB_DISTRIBUTED_LOOPBACK_NODES" => {
                config.distributed.loopback_remote_nodes = parse_csv_node_list(key, value)?;
            }
            "AIONDB_DISTRIBUTED_ALLOW_UNREGISTERED_LOOPBACK_NODES" => {
                config.distributed.allow_unregistered_loopback_nodes = parse_bool(key, value)?;
            }
            "AIONDB_DISTRIBUTED_REMOTE_NODES" => {
                config.distributed.remote_nodes = parse_remote_nodes(key, value)?;
            }
            "AIONDB_DISTRIBUTED_INTER_NODE_AUTH_TOKEN" => {
                let token = value.trim();
                config.distributed.inter_node_auth_token = if token.is_empty() {
                    None
                } else {
                    Some(token.to_owned())
                };
            }
            "AIONDB_DISTRIBUTED_TLS_CERT_PATH" => {
                config.distributed.tls_cert_path = Some(value.clone());
            }
            "AIONDB_DISTRIBUTED_TLS_KEY_PATH" => {
                config.distributed.tls_key_path = Some(value.clone());
            }
            "AIONDB_DISTRIBUTED_TLS_CA_CERT_PATH" => {
                config.distributed.tls_ca_cert_path = Some(value.clone());
            }
            "AIONDB_DISTRIBUTED_REQUIRE_TLS" => {
                config.distributed.require_tls = parse_bool(key, value)?;
            }
            "AIONDB_DISTRIBUTED_FRAGMENT_TRANSPORT_PORT" => {
                config.distributed.fragment_transport_port = parse_u16(key, value)?;
            }
            "AIONDB_DISTRIBUTED_REMOTE_SNAPSHOT_MODE" => {
                config.distributed.remote_snapshot_mode = parse_remote_snapshot_mode(key, value)?;
            }
            "AIONDB_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_FAILURE_THRESHOLD" => {
                config.distributed.remote_circuit_breaker_failure_threshold =
                    parse_u32(key, value)?;
            }
            "AIONDB_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_RESET_TIMEOUT_MS" => {
                config.distributed.remote_circuit_breaker_reset_timeout =
                    Duration::from_millis(parse_u64(key, value)?);
            }
            "AIONDB_DISTRIBUTED_REMOTE_CONNECT_TIMEOUT_MS" => {
                config.distributed.remote_connect_timeout =
                    Duration::from_millis(parse_u64(key, value)?);
            }
            "AIONDB_DISTRIBUTED_REMOTE_MAX_RETRIES" => {
                config.distributed.remote_max_retries = parse_u32(key, value)?;
            }
            "AIONDB_DISTRIBUTED_REMOTE_RETRY_BACKOFF_MS" => {
                config.distributed.remote_retry_backoff =
                    Duration::from_millis(parse_u64(key, value)?);
            }
            // ---------------------------------------------------------------
            // Sharding
            // ---------------------------------------------------------------
            "AIONDB_SHARDING_ENABLED" => {
                config.distributed.sharding.enabled = parse_bool(key, value)?;
            }
            "AIONDB_SHARDING_DEFAULT_SHARD_COUNT" => {
                config.distributed.sharding.default_shard_count = parse_u32(key, value)?;
            }
            "AIONDB_SHARDING_VIRTUAL_NODES_PER_SHARD" => {
                config.distributed.sharding.virtual_nodes_per_shard = parse_u32(key, value)?;
            }
            "AIONDB_SHARDING_REPLICATION_FACTOR" => {
                config.distributed.sharding.replication_factor = parse_u32(key, value)?;
            }
            "AIONDB_SHARDING_AUTO_REBALANCE" => {
                config.distributed.sharding.auto_rebalance = parse_bool(key, value)?;
            }
            "AIONDB_SHARDING_MAX_LEARNERS_PER_SHARD" => {
                config.distributed.sharding.max_learners_per_shard = parse_usize(key, value)?;
            }
            "AIONDB_SHARDING_MAX_LEARNERS_PER_NODE" => {
                config.distributed.sharding.max_learners_per_node = parse_usize(key, value)?;
            }
            "AIONDB_SHARDING_LEADERSHIP_MAX_TRANSFERS_PER_MAINTENANCE" => {
                config
                    .distributed
                    .sharding
                    .leadership_max_transfers_per_maintenance = parse_usize(key, value)?;
            }
            "AIONDB_SHARDING_LEADERSHIP_MIN_LOAD_DELTA" => {
                config.distributed.sharding.leadership_min_load_delta = parse_usize(key, value)?;
            }
            "AIONDB_SHARDING_NODE_ATTRIBUTES" => {
                config.distributed.sharding.node_attributes = parse_node_attributes(key, value)?;
            }
            "AIONDB_SHARDING_PLACEMENT_REQUIRED_ATTRIBUTES" => {
                config.distributed.sharding.placement_required_attributes =
                    parse_attribute_constraints(key, value)?;
            }
            "AIONDB_SHARDING_LEASE_PREFERENCE_ATTRIBUTES" => {
                config.distributed.sharding.lease_preference_attributes =
                    parse_attribute_constraints(key, value)?;
            }
            "AIONDB_SHARDING_PLACEMENT_SPREAD_ATTRIBUTES" => {
                config.distributed.sharding.placement_spread_attributes =
                    parse_csv_string_list(key, value)?;
            }
            // ---------------------------------------------------------------
            // High Availability
            // ---------------------------------------------------------------
            "AIONDB_HA_ENABLED" => {
                config.ha.enabled = parse_bool(key, value)?;
            }
            "AIONDB_HA_NODE_ID" => {
                config.ha.node_id = parse_u64(key, value)?;
            }
            "AIONDB_HA_HEALTH_CHECK_INTERVAL_MS" => {
                config.ha.health_check_interval = Duration::from_millis(parse_u64(key, value)?);
            }
            "AIONDB_HA_HEALTH_CHECK_TIMEOUT_MS" => {
                config.ha.health_check_timeout = Duration::from_millis(parse_u64(key, value)?);
            }
            "AIONDB_HA_ELECTION_TIMEOUT_MS" => {
                config.ha.election_timeout = Duration::from_millis(parse_u64(key, value)?);
            }
            "AIONDB_HA_MAX_FAILOVER_LAG" => {
                config.ha.max_failover_lag = parse_u64(key, value)?;
            }
            "AIONDB_HA_CLUSTER_NODES" => {
                config.ha.cluster_nodes = value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect();
            }
            "AIONDB_HA_PORT" => {
                config.ha.ha_port = parse_u16(key, value)?;
            }
            "AIONDB_HA_FENCING_TOKEN_PATH" => {
                config.ha.fencing_token_path = Some(value.clone());
            }
            "AIONDB_HA_AUTH_TOKEN" => {
                let token = value.trim();
                config.ha.inter_node_auth_token = if token.is_empty() {
                    None
                } else {
                    Some(token.to_owned())
                };
            }
            // Already consumed in the pre-pass above; skip here so the
            // wholesale profile assignment runs first, then individual
            // SECURITY_* overrides layer on top deterministically.
            "AIONDB_SECURITY_PROFILE" => {}
            "AIONDB_SECURITY_ALLOW_ANONYMOUS_LOCAL" => {
                config.security.allow_anonymous_local = parse_bool(key, value)?;
            }
            "AIONDB_SECURITY_MAX_AUTH_FAILURES" => {
                let n = parse_u32(key, value)?;
                if n == 0 || n > 1000 {
                    return Err(DbError::internal(format!(
                        "{key}: must be between 1 and 1000"
                    )));
                }
                config.security.max_auth_failures = n;
            }
            "AIONDB_SECURITY_AUTH_LOCKOUT_WINDOW_MS" => {
                let ms = parse_u64(key, value)?;
                if ms == 0 {
                    return Err(DbError::internal(format!(
                        "{key}: lockout window must be > 0"
                    )));
                }
                config.security.auth_lockout_window = Duration::from_millis(ms);
            }
            "AIONDB_SECURITY_DURABLE_AUTH_LOCKOUT" => {
                config.security.durable_auth_lockout = parse_bool(key, value)?;
            }
            "AIONDB_SECURITY_AUTH_LOCKOUT_STATE_PATH" => {
                config.security.auth_lockout_state_path = Some(value.as_str().into());
            }
            "AIONDB_SECURITY_DURABLE_AUTH_AUDIT" => {
                config.security.durable_auth_audit = parse_bool(key, value)?;
            }
            "AIONDB_SECURITY_AUTH_AUDIT_LOG_PATH" => {
                config.security.auth_audit_log_path = Some(value.as_str().into());
            }
            "AIONDB_SECURITY_AUTH_AUDIT_MAX_FILE_SIZE_BYTES" => {
                config.security.auth_audit_max_file_size_bytes = parse_u64(key, value)?;
            }
            "AIONDB_SECURITY_AUTH_AUDIT_MAX_ROTATED_FILES" => {
                config.security.auth_audit_max_rotated_files = parse_usize(key, value)?;
            }
            "AIONDB_SECURITY_PASSWORD_MIN_LENGTH" => {
                config.security.password_min_length = parse_usize(key, value)?;
            }
            "AIONDB_SECURITY_REJECT_ROLE_NAME_AS_PASSWORD" => {
                config.security.reject_role_name_as_password = parse_bool(key, value)?;
            }
            "AIONDB_SECURITY_PASSWORD_REQUIRE_LOWERCASE" => {
                config.security.password_require_lowercase = parse_bool(key, value)?;
            }
            "AIONDB_SECURITY_PASSWORD_REQUIRE_UPPERCASE" => {
                config.security.password_require_uppercase = parse_bool(key, value)?;
            }
            "AIONDB_SECURITY_PASSWORD_REQUIRE_DIGIT" => {
                config.security.password_require_digit = parse_bool(key, value)?;
            }
            "AIONDB_SECURITY_PASSWORD_REQUIRE_SYMBOL" => {
                config.security.password_require_symbol = parse_bool(key, value)?;
            }
            "AIONDB_SECURITY_REQUIRE_TLS_FOR_PASSWORD" => {
                config.security.require_tls_for_password = parse_bool(key, value)?;
            }
            "AIONDB_SECURITY_ALLOW_EPHEMERAL_USERS" => {
                config.security.allow_ephemeral_users = parse_bool(key, value)?;
            }
            "AIONDB_SECURITY_MAX_SESSION_IDLE_TIMEOUT_MS" => {
                config.security.max_session_idle_timeout =
                    Some(Duration::from_millis(parse_u64(key, value)?));
            }
            "AIONDB_SECURITY_MAX_SESSION_LIFETIME_MS" => {
                config.security.max_session_lifetime =
                    Some(Duration::from_millis(parse_u64(key, value)?));
            }
            "AIONDB_SECURITY_MAX_CONCURRENT_SESSIONS_PER_ROLE" => {
                config.security.max_concurrent_sessions_per_role = Some(parse_u32(key, value)?);
            }
            "AIONDB_SECURITY_MAX_TRANSACTION_IDLE_TIMEOUT_MS" => {
                config.security.max_transaction_idle_timeout =
                    Some(Duration::from_millis(parse_u64(key, value)?));
            }
            "AIONDB_SECURITY_DDL_AUDIT_ENABLED" => {
                config.security.ddl_audit_enabled = parse_bool(key, value)?;
            }
            _ if key.starts_with("AIONDB_") && strict_mode => {
                return Err(DbError::internal(format!(
                    "unknown configuration key: {key} (strict mode is enabled via AIONDB_CONFIG_STRICT=true)"
                )));
            }
            _ if key.starts_with("AIONDB_") => {
                warn!(key = %key, "unknown configuration key ignored in permissive mode");
            }
            _ => {}
        }
    }

    // Backward compatibility: synchronous_commit=true without explicit
    // write_concern upgrades to Majority.
    if config.replication.synchronous_commit
        && !write_concern_was_explicit
        && config.replication.write_concern == crate::replication::WriteConcern::Local
    {
        config.replication.write_concern = crate::replication::WriteConcern::Majority;
    }

    if config.pgwire.max_connections_per_ip > config.pgwire.max_connections {
        config.pgwire.max_connections_per_ip = config.pgwire.max_connections;
    }
    validate_config(&config)?;

    Ok(config)
}

/// Reject paths containing `..` components to prevent path traversal.
fn reject_path_traversal(label: &str, path: &Path) -> DbResult<()> {
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(DbError::internal(format!(
            "{label}: path must not contain '..' components"
        )));
    }
    Ok(())
}

fn validate_config(config: &RuntimeConfig) -> DbResult<()> {
    // Path traversal checks on all file-path configuration values.
    reject_path_traversal("AIONDB_STORAGE_DATA_DIR", &config.storage.data_dir)?;
    if let Some(ref p) = config.pgwire.tls_cert_path {
        reject_path_traversal("AIONDB_PGWIRE_TLS_CERT_PATH", Path::new(p))?;
    }
    if let Some(ref p) = config.pgwire.tls_key_path {
        reject_path_traversal("AIONDB_PGWIRE_TLS_KEY_PATH", Path::new(p))?;
    }
    if let Some(ref p) = config.pgwire.tls_client_ca_path {
        reject_path_traversal("AIONDB_PGWIRE_TLS_CLIENT_CA_PATH", Path::new(p))?;
    }
    if let Some(ref p) = config.security.auth_lockout_state_path {
        reject_path_traversal("AIONDB_SECURITY_AUTH_LOCKOUT_STATE_PATH", Path::new(p))?;
    }
    if let Some(ref p) = config.security.auth_audit_log_path {
        reject_path_traversal("AIONDB_SECURITY_AUTH_AUDIT_LOG_PATH", Path::new(p))?;
    }
    if let Some(ref p) = config.distributed.tls_cert_path {
        reject_path_traversal("AIONDB_DISTRIBUTED_TLS_CERT_PATH", Path::new(p))?;
    }
    if let Some(ref p) = config.distributed.tls_key_path {
        reject_path_traversal("AIONDB_DISTRIBUTED_TLS_KEY_PATH", Path::new(p))?;
    }
    if let Some(ref p) = config.distributed.tls_ca_cert_path {
        reject_path_traversal("AIONDB_DISTRIBUTED_TLS_CA_CERT_PATH", Path::new(p))?;
    }
    if let Some(ref p) = config.ha.fencing_token_path {
        reject_path_traversal("AIONDB_HA_FENCING_TOKEN_PATH", Path::new(p))?;
    }

    // Storage
    if config.storage.page_size != crate::storage::DEFAULT_STORAGE_PAGE_SIZE {
        return Err(DbError::internal(format!(
            "AIONDB_STORAGE_PAGE_SIZE is fixed at {}, got {}",
            crate::storage::DEFAULT_STORAGE_PAGE_SIZE,
            config.storage.page_size
        )));
    }
    if config.storage.max_open_files == 0 {
        return Err(DbError::internal(
            "AIONDB_STORAGE_MAX_OPEN_FILES must be >= 1",
        ));
    }
    if config.storage.table_pool_frames == 0 {
        return Err(DbError::internal(
            "AIONDB_STORAGE_TABLE_POOL_FRAMES must be >= 1",
        ));
    }
    if config.storage.snapshot_pool_frames == 0 {
        return Err(DbError::internal(
            "AIONDB_STORAGE_SNAPSHOT_POOL_FRAMES must be >= 1",
        ));
    }

    // PgWire - validate listen_addr
    if !config.pgwire.listen_addr.is_empty() {
        use std::net::SocketAddr;
        let addr = &config.pgwire.listen_addr;
        // Accept either an explicit `IP:port` (parsed as SocketAddr) or a
        // `hostname:port` form whose hostname is resolved at bind time.
        // Rejecting bare hostnames here forced operators using `localhost:5432`
        // to switch to an IP literal even though Tokio's `bind` accepts the
        // hostname form.
        if addr.parse::<SocketAddr>().is_err() {
            let (host, port) = addr.rsplit_once(':').ok_or_else(|| {
                DbError::internal(format!(
                    "AIONDB_PGWIRE_LISTEN_ADDR: invalid socket address \"{addr}\": expected host:port"
                ))
            })?;
            if host.is_empty() {
                return Err(DbError::internal(format!(
                    "AIONDB_PGWIRE_LISTEN_ADDR: empty host in \"{addr}\""
                )));
            }
            port.parse::<u16>().map_err(|e| {
                DbError::internal(format!(
                    "AIONDB_PGWIRE_LISTEN_ADDR: invalid port in \"{addr}\": {e}"
                ))
            })?;
        }
    }
    // Cap connection counts so an env-typo of `4294967295` cannot pre-allocate
    // gigabytes of per-connection state (audit config M-01).
    const MAX_CONNECTION_LIMIT: u32 = 1_000_000;
    if config.pgwire.max_connections == 0 {
        return Err(DbError::internal(
            "AIONDB_PGWIRE_MAX_CONNECTIONS must be >= 1",
        ));
    }
    if config.pgwire.max_connections > MAX_CONNECTION_LIMIT {
        return Err(DbError::internal(format!(
            "AIONDB_PGWIRE_MAX_CONNECTIONS must be <= {MAX_CONNECTION_LIMIT}"
        )));
    }
    // Floor security-critical timeouts here too, not just at the env loader,
    // so TOML / public-field-set callers don't end up with a `Duration::ZERO`
    // brute-force throttle (audit second-opinion against config M-04 fix).
    if config.pgwire.auth_failure_backoff < Duration::from_millis(50) {
        return Err(DbError::internal(
            "pgwire.auth_failure_backoff must be >= 50ms",
        ));
    }
    if config.pgwire.startup_timeout < Duration::from_millis(1) {
        return Err(DbError::internal("pgwire.startup_timeout must be >= 1ms"));
    }
    // guardrail (audit config M-02). Refuse the obvious sentinel.
    if config.limits.statement_timeout >= Duration::from_secs(86_400 * 365) {
        return Err(DbError::internal(
            "AIONDB_LIMITS_STATEMENT_TIMEOUT_MS must be < 1 year (365 days)",
        ));
    }
    if config.limits.lock_timeout >= Duration::from_secs(86_400 * 365) {
        return Err(DbError::internal(
            "AIONDB_LIMITS_LOCK_TIMEOUT_MS must be < 1 year (365 days)",
        ));
    }
    if config.pgwire.max_connections_per_ip == 0 {
        return Err(DbError::internal(
            "AIONDB_PGWIRE_MAX_CONNECTIONS_PER_IP must be >= 1",
        ));
    }
    let has_tls_cert = config.pgwire.tls_cert_path.is_some();
    let has_tls_key = config.pgwire.tls_key_path.is_some();
    if has_tls_cert != has_tls_key {
        return Err(DbError::internal(
            "AIONDB_PGWIRE_TLS_CERT_PATH and AIONDB_PGWIRE_TLS_KEY_PATH must be set together",
        ));
    }
    if matches!(config.pgwire.tls_mode, TlsMode::Require) && !has_tls_cert {
        return Err(DbError::internal(
            "AIONDB_PGWIRE_TLS_MODE=require requires AIONDB_PGWIRE_TLS_CERT_PATH and AIONDB_PGWIRE_TLS_KEY_PATH",
        ));
    }
    if config.pgwire.tls_client_ca_path.is_some() && !has_tls_cert {
        return Err(DbError::internal(
            "AIONDB_PGWIRE_TLS_CLIENT_CA_PATH requires AIONDB_PGWIRE_TLS_CERT_PATH and AIONDB_PGWIRE_TLS_KEY_PATH",
        ));
    }
    // Refuse the foot-gun combination of binding to a non-loopback address
    // with TLS disabled, unless the operator explicitly opts in via env
    // (audit config L-02). Catches the worst-of-both: plaintext credentials
    // travelling over the wire on a public interface.
    let listen_host = config
        .pgwire
        .listen_addr
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(config.pgwire.listen_addr.as_str())
        .trim_matches(|c| c == '[' || c == ']');
    let is_loopback = matches!(listen_host, "127.0.0.1" | "::1" | "localhost" | "")
        || listen_host.starts_with("127.");
    if !is_loopback
        && matches!(config.pgwire.tls_mode, TlsMode::Disable)
        && std::env::var_os("AIONDB_ALLOW_PLAINTEXT_PUBLIC").is_none()
    {
        return Err(DbError::internal(
            "AIONDB_PGWIRE_LISTEN_ADDR is non-loopback with AIONDB_PGWIRE_TLS_MODE=disable; \
             set AIONDB_ALLOW_PLAINTEXT_PUBLIC=1 to opt out (not recommended)",
        ));
    }

    // Engine pool
    if config.pgwire.engine_pool.worker_threads == 0 {
        return Err(DbError::internal(
            "AIONDB_ENGINE_POOL_WORKER_THREADS must be >= 1",
        ));
    }
    if config.pgwire.engine_pool.queue_depth == 0 {
        return Err(DbError::internal(
            "AIONDB_ENGINE_POOL_QUEUE_DEPTH must be >= 1",
        ));
    }

    // Replication
    if config.replication.role == ReplicationRole::Replica {
        match config.replication.primary_conninfo.as_deref() {
            Some(conninfo) if !conninfo.trim().is_empty() => {}
            _ => {
                return Err(DbError::internal(
                    "AIONDB_REPLICATION_PRIMARY_CONNINFO must be non-empty when AIONDB_REPLICATION_ROLE=replica",
                ));
            }
        }
    }
    if config.replication.role == ReplicationRole::Primary
        && config.replication.max_wal_senders == 0
    {
        return Err(DbError::internal(
            "AIONDB_REPLICATION_MAX_WAL_SENDERS must be >= 1 when AIONDB_REPLICATION_ROLE=primary",
        ));
    }
    if config.replication.replication_factor == 0 {
        return Err(DbError::internal("AIONDB_REPLICATION_FACTOR must be >= 1"));
    }
    if config.replication.status_interval.is_zero() {
        return Err(DbError::internal(
            "AIONDB_REPLICATION_STATUS_INTERVAL_MS must be >= 1",
        ));
    }
    if config.replication.sync_commit_timeout.is_zero() {
        return Err(DbError::internal(
            "AIONDB_REPLICATION_SYNC_COMMIT_TIMEOUT_MS must be >= 1",
        ));
    }
    if let crate::replication::WriteConcern::Factor(n) = config.replication.write_concern {
        if n == 0 {
            return Err(DbError::internal(
                "AIONDB_REPLICATION_WRITE_CONCERN factor:0 is invalid; use local for no replica wait",
            ));
        }
        let max_replica_acks = config.replication.replication_factor.saturating_sub(1);
        if n > max_replica_acks {
            return Err(DbError::internal(format!(
                "AIONDB_REPLICATION_WRITE_CONCERN factor:{n} requires more replica acks than replication_factor {} allows (max {max_replica_acks})",
                config.replication.replication_factor
            )));
        }
    }
    // Limits
    if config.limits.max_result_rows == 0 {
        return Err(DbError::internal(
            "AIONDB_LIMITS_MAX_RESULT_ROWS must be >= 1",
        ));
    }
    if config.limits.max_result_bytes == 0 {
        return Err(DbError::internal(
            "AIONDB_LIMITS_MAX_RESULT_BYTES must be >= 1",
        ));
    }
    if config.limits.max_parallel_workers_per_query == 0 {
        return Err(DbError::internal(
            "AIONDB_LIMITS_MAX_PARALLEL_WORKERS_PER_QUERY must be >= 1",
        ));
    }
    if config.limits.max_portals == 0 {
        return Err(DbError::internal("AIONDB_LIMITS_MAX_PORTALS must be >= 1"));
    }
    if config.limits.max_prepared_statements == 0 {
        return Err(DbError::internal(
            "AIONDB_LIMITS_MAX_PREPARED_STATEMENTS must be >= 1",
        ));
    }
    if config.limits.max_recursive_iterations == 0 {
        return Err(DbError::internal(
            "AIONDB_LIMITS_MAX_RECURSIVE_ITERATIONS must be >= 1",
        ));
    }
    if config.limits.max_recursive_rows == 0 {
        return Err(DbError::internal(
            "AIONDB_LIMITS_MAX_RECURSIVE_ROWS must be >= 1",
        ));
    }

    // Distributed
    if config.distributed.fragment_transport_port == 0 {
        return Err(DbError::internal(
            "AIONDB_DISTRIBUTED_FRAGMENT_TRANSPORT_PORT must be >= 1",
        ));
    }
    if config.distributed.remote_circuit_breaker_failure_threshold == 0 {
        return Err(DbError::internal(
            "AIONDB_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_FAILURE_THRESHOLD must be >= 1",
        ));
    }
    if config
        .distributed
        .remote_circuit_breaker_reset_timeout
        .is_zero()
    {
        return Err(DbError::internal(
            "AIONDB_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_RESET_TIMEOUT_MS must be >= 1",
        ));
    }
    if config.distributed.remote_connect_timeout.is_zero() {
        return Err(DbError::internal(
            "AIONDB_DISTRIBUTED_REMOTE_CONNECT_TIMEOUT_MS must be >= 1",
        ));
    }
    if config.distributed.remote_retry_backoff.is_zero() {
        return Err(DbError::internal(
            "AIONDB_DISTRIBUTED_REMOTE_RETRY_BACKOFF_MS must be >= 1",
        ));
    }
    if let Some(token) = config.distributed.inter_node_auth_token.as_ref() {
        if token.trim().is_empty() {
            return Err(DbError::internal(
                "AIONDB_DISTRIBUTED_INTER_NODE_AUTH_TOKEN must not be empty when set",
            ));
        }
        if token.len() < MIN_INTER_NODE_AUTH_TOKEN_BYTES {
            return Err(DbError::internal(format!(
                "AIONDB_DISTRIBUTED_INTER_NODE_AUTH_TOKEN must be at least {MIN_INTER_NODE_AUTH_TOKEN_BYTES} bytes when set"
            )));
        }
    }
    if distributed_fragment_transport_configured(config)
        && config.distributed.inter_node_auth_token.is_none()
    {
        return Err(DbError::internal(
            "AIONDB_DISTRIBUTED_INTER_NODE_AUTH_TOKEN must be set when fragment transport is configured",
        ));
    }
    let mut seen_remote_nodes = HashSet::new();
    for node in &config.distributed.remote_nodes {
        if node.node_id.trim().is_empty() {
            return Err(DbError::internal(
                "AIONDB_DISTRIBUTED_REMOTE_NODES: node_id must not be empty",
            ));
        }
        if !seen_remote_nodes.insert(node.node_id.to_ascii_lowercase()) {
            return Err(DbError::internal(format!(
                "AIONDB_DISTRIBUTED_REMOTE_NODES: duplicate node_id '{}'",
                node.node_id
            )));
        }
        validate_remote_node_addr("AIONDB_DISTRIBUTED_REMOTE_NODES", &node.addr)?;
    }

    // Sharding
    if config.distributed.sharding.enabled {
        if config.distributed.sharding.default_shard_count == 0 {
            return Err(DbError::internal(
                "AIONDB_SHARDING_DEFAULT_SHARD_COUNT must be >= 1 when sharding is enabled",
            ));
        }
        if config.distributed.sharding.default_shard_count > MAX_STORAGE_SHARD_COUNT {
            return Err(DbError::internal(format!(
                "AIONDB_SHARDING_DEFAULT_SHARD_COUNT must be <= {MAX_STORAGE_SHARD_COUNT} when sharding is enabled"
            )));
        }
        if config.distributed.sharding.virtual_nodes_per_shard == 0 {
            return Err(DbError::internal(
                "AIONDB_SHARDING_VIRTUAL_NODES_PER_SHARD must be >= 1 when sharding is enabled",
            ));
        }
        if config.distributed.sharding.virtual_nodes_per_shard > MAX_STORAGE_VIRTUAL_NODES_PER_SHARD
        {
            return Err(DbError::internal(format!(
                "AIONDB_SHARDING_VIRTUAL_NODES_PER_SHARD must be <= {MAX_STORAGE_VIRTUAL_NODES_PER_SHARD} when sharding is enabled"
            )));
        }
        let total_virtual_nodes = u64::from(config.distributed.sharding.default_shard_count)
            * u64::from(config.distributed.sharding.virtual_nodes_per_shard);
        if total_virtual_nodes > MAX_STORAGE_HASH_RING_VIRTUAL_NODES {
            return Err(DbError::internal(format!(
                "AIONDB_SHARDING_DEFAULT_SHARD_COUNT * AIONDB_SHARDING_VIRTUAL_NODES_PER_SHARD would create {total_virtual_nodes} hash-ring points, exceeding {MAX_STORAGE_HASH_RING_VIRTUAL_NODES}"
            )));
        }
        if config.distributed.sharding.leadership_min_load_delta == 0 {
            return Err(DbError::internal(
                "AIONDB_SHARDING_LEADERSHIP_MIN_LOAD_DELTA must be >= 1 when sharding is enabled",
            ));
        }
        for (node_id, attributes) in &config.distributed.sharding.node_attributes {
            if node_id.trim().is_empty() {
                return Err(DbError::internal(
                    "AIONDB_SHARDING_NODE_ATTRIBUTES contains an empty node id",
                ));
            }
            for (key, value) in attributes {
                validate_attribute_pair("AIONDB_SHARDING_NODE_ATTRIBUTES", key, value)?;
            }
        }
        for constraint in &config.distributed.sharding.placement_required_attributes {
            validate_attribute_pair(
                "AIONDB_SHARDING_PLACEMENT_REQUIRED_ATTRIBUTES",
                &constraint.key,
                &constraint.value,
            )?;
        }
        for constraint in &config.distributed.sharding.lease_preference_attributes {
            validate_attribute_pair(
                "AIONDB_SHARDING_LEASE_PREFERENCE_ATTRIBUTES",
                &constraint.key,
                &constraint.value,
            )?;
        }
        for attribute in &config.distributed.sharding.placement_spread_attributes {
            if attribute.trim().is_empty() {
                return Err(DbError::internal(
                    "AIONDB_SHARDING_PLACEMENT_SPREAD_ATTRIBUTES contains an empty attribute",
                ));
            }
        }
    }

    // HA validation
    if config.ha.enabled {
        if config.ha.cluster_nodes.is_empty() {
            return Err(DbError::internal(
                "AIONDB_HA_CLUSTER_NODES must be set when HA is enabled",
            ));
        }
        if config.ha.node_id == 0 {
            return Err(DbError::internal(
                "AIONDB_HA_NODE_ID must be a non-zero value when HA is enabled",
            ));
        }
        if config.ha.health_check_timeout <= config.ha.health_check_interval {
            return Err(DbError::internal(
                "AIONDB_HA_HEALTH_CHECK_TIMEOUT_MS must be greater than AIONDB_HA_HEALTH_CHECK_INTERVAL_MS",
            ));
        }
        if config.ha.election_timeout <= config.ha.health_check_timeout {
            return Err(DbError::internal(
                "AIONDB_HA_ELECTION_TIMEOUT_MS must be greater than AIONDB_HA_HEALTH_CHECK_TIMEOUT_MS",
            ));
        }
        if config.ha.inter_node_auth_token.is_none() {
            return Err(DbError::internal(
                "AIONDB_HA_AUTH_TOKEN must be set when HA is enabled",
            ));
        }
    }
    if matches!(config.ha.inter_node_auth_token, Some(ref token) if token.trim().is_empty()) {
        return Err(DbError::internal(
            "AIONDB_HA_AUTH_TOKEN must not be empty when set",
        ));
    }
    if matches!(
        config.ha.inter_node_auth_token,
        Some(ref token) if token.len() < MIN_INTER_NODE_AUTH_TOKEN_BYTES
    ) {
        return Err(DbError::internal(format!(
            "AIONDB_HA_AUTH_TOKEN must be at least {MIN_INTER_NODE_AUTH_TOKEN_BYTES} bytes when set"
        )));
    }

    // Security
    if config.security.max_auth_failures == 0 {
        return Err(DbError::internal(
            "AIONDB_SECURITY_MAX_AUTH_FAILURES must be >= 1",
        ));
    }
    if config.security.auth_audit_max_file_size_bytes == 0 {
        return Err(DbError::internal(
            "AIONDB_SECURITY_AUTH_AUDIT_MAX_FILE_SIZE_BYTES must be >= 1",
        ));
    }
    if matches!(config.security.max_concurrent_sessions_per_role, Some(0)) {
        return Err(DbError::internal(
            "AIONDB_SECURITY_MAX_CONCURRENT_SESSIONS_PER_ROLE must be >= 1",
        ));
    }

    Ok(())
}

fn distributed_fragment_transport_configured(config: &RuntimeConfig) -> bool {
    !config.distributed.remote_nodes.is_empty()
        || config.distributed.fragment_transport_port
            != crate::runtime::DistributedConfig::default().fragment_transport_port
}

fn parse_bool(key: &str, value: &str) -> DbResult<bool> {
    value
        .parse::<bool>()
        .map_err(|error| DbError::internal(format!("invalid boolean for {key}: {value} ({error})")))
}

fn parse_storage_backend(key: &str, value: &str) -> DbResult<StorageBackend> {
    StorageBackend::parse(value).ok_or_else(|| {
        DbError::internal(format!(
            "invalid storage backend for {key}: {value} (expected one of: in_memory, durable, disk, page_engine, lsm)"
        ))
    })
}

fn parse_durable_wal_commit_policy(key: &str, value: &str) -> DbResult<DurableWalCommitPolicy> {
    DurableWalCommitPolicy::parse(value).ok_or_else(|| {
        DbError::internal(format!(
            "invalid durable WAL commit policy for {key}: {value} (expected one of: always, every:N, never)"
        ))
    })
}

fn parse_write_concern(key: &str, value: &str) -> DbResult<crate::replication::WriteConcern> {
    crate::replication::WriteConcern::parse(value).ok_or_else(|| {
        DbError::internal(format!(
            "invalid write concern for {key}: {value} (expected one of: local, majority, all, factor:N)"
        ))
    })
}

fn parse_replication_role(key: &str, value: &str) -> DbResult<ReplicationRole> {
    ReplicationRole::parse(value).ok_or_else(|| {
        DbError::internal(format!(
            "invalid replication role for {key}: {value} (expected one of: standalone, primary, replica)"
        ))
    })
}

fn parse_wal_compression(key: &str, value: &str) -> DbResult<WalCompression> {
    WalCompression::parse(value).ok_or_else(|| {
        DbError::internal(format!(
            "invalid WAL compression for {key}: {value} (expected one of: none, lz4, zstd)"
        ))
    })
}

fn parse_wal_lsn_mode(key: &str, value: &str) -> DbResult<WalLsnMode> {
    WalLsnMode::parse(value).ok_or_else(|| {
        DbError::internal(format!(
            "invalid WAL LSN mode for {key}: {value} (expected one of: logical, byte_offset)"
        ))
    })
}

fn parse_remote_snapshot_mode(key: &str, value: &str) -> DbResult<RemoteSnapshotMode> {
    RemoteSnapshotMode::parse(value).ok_or_else(|| {
        DbError::internal(format!(
            "invalid remote snapshot mode for {key}: {value} (expected one of: latest_visible, coordinator)"
        ))
    })
}

fn parse_csv_node_list(key: &str, value: &str) -> DbResult<Vec<String>> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut nodes = Vec::new();
    let mut seen = HashSet::new();
    for raw_part in value.split(',') {
        let node = raw_part.trim();
        if node.is_empty() {
            return Err(DbError::internal(format!(
                "invalid node list for {key}: empty node id in \"{value}\"",
            )));
        }
        if seen.insert(node.to_ascii_lowercase()) {
            nodes.push(node.to_owned());
        }
    }
    Ok(nodes)
}

fn parse_csv_string_list(key: &str, value: &str) -> DbResult<Vec<String>> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut values = Vec::new();
    let mut seen = HashSet::new();
    for raw_part in value.split(',') {
        let item = raw_part.trim();
        if item.is_empty() {
            return Err(DbError::internal(format!(
                "invalid comma-separated list for {key}: empty entry in \"{value}\"",
            )));
        }
        if seen.insert(item.to_ascii_lowercase()) {
            values.push(item.to_owned());
        }
    }
    Ok(values)
}

fn parse_attribute_constraints(
    key: &str,
    value: &str,
) -> DbResult<Vec<aiondb_shard::PlacementAttributeConstraint>> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut constraints = Vec::new();
    for raw_part in value.split(',') {
        let part = raw_part.trim();
        if part.is_empty() {
            return Err(DbError::internal(format!(
                "invalid attribute list for {key}: empty entry in \"{value}\"",
            )));
        }
        let (attr_key, attr_value) = parse_attribute_pair(key, part)?;
        constraints.push(aiondb_shard::PlacementAttributeConstraint {
            key: attr_key.to_owned(),
            value: attr_value.to_owned(),
        });
    }
    Ok(constraints)
}

fn parse_node_attributes(
    key: &str,
    value: &str,
) -> DbResult<std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>> {
    let mut nodes = std::collections::BTreeMap::new();
    if value.trim().is_empty() {
        return Ok(nodes);
    }

    for raw_node in value.split(',') {
        let node_spec = raw_node.trim();
        if node_spec.is_empty() {
            return Err(DbError::internal(format!(
                "invalid node attribute list for {key}: empty node entry in \"{value}\"",
            )));
        }
        let Some((raw_node_id, raw_attrs)) = node_spec.split_once(':') else {
            return Err(DbError::internal(format!(
                "invalid node attribute list for {key}: expected node_id:key=value;key=value in \"{node_spec}\"",
            )));
        };
        let node_id = raw_node_id.trim();
        if node_id.is_empty() {
            return Err(DbError::internal(format!(
                "invalid node attribute list for {key}: empty node_id in \"{node_spec}\"",
            )));
        }
        if nodes.contains_key(node_id) {
            return Err(DbError::internal(format!(
                "invalid node attribute list for {key}: duplicate node_id \"{node_id}\"",
            )));
        }

        let mut attrs = std::collections::BTreeMap::new();
        for raw_attr in raw_attrs.split(';') {
            let attr_spec = raw_attr.trim();
            if attr_spec.is_empty() {
                continue;
            }
            let (attr_key, attr_value) = parse_attribute_pair(key, attr_spec)?;
            if attrs
                .insert(attr_key.to_owned(), attr_value.to_owned())
                .is_some()
            {
                return Err(DbError::internal(format!(
                    "invalid node attribute list for {key}: duplicate attribute \"{attr_key}\" on node \"{node_id}\"",
                )));
            }
        }
        if attrs.is_empty() {
            return Err(DbError::internal(format!(
                "invalid node attribute list for {key}: node \"{node_id}\" has no attributes",
            )));
        }
        nodes.insert(node_id.to_owned(), attrs);
    }

    Ok(nodes)
}

fn parse_attribute_pair<'a>(key: &str, raw: &'a str) -> DbResult<(&'a str, &'a str)> {
    let Some((attr_key, attr_value)) = raw.split_once('=') else {
        return Err(DbError::internal(format!(
            "invalid attribute for {key}: expected key=value in \"{raw}\"",
        )));
    };
    let attr_key = attr_key.trim();
    let attr_value = attr_value.trim();
    validate_attribute_pair(key, attr_key, attr_value)?;
    Ok((attr_key, attr_value))
}

fn validate_attribute_pair(key: &str, attr_key: &str, attr_value: &str) -> DbResult<()> {
    if attr_key.is_empty() {
        return Err(DbError::internal(format!(
            "invalid attribute for {key}: key must not be empty",
        )));
    }
    if attr_value.is_empty() {
        return Err(DbError::internal(format!(
            "invalid attribute for {key}: value must not be empty",
        )));
    }
    Ok(())
}

fn parse_remote_nodes(key: &str, value: &str) -> DbResult<Vec<RemoteNodeConfig>> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut nodes = Vec::new();
    let mut seen = HashSet::new();
    for raw_part in value.split(',') {
        let part = raw_part.trim();
        if part.is_empty() {
            return Err(DbError::internal(format!(
                "invalid remote node list for {key}: empty entry in \"{value}\"",
            )));
        }

        let Some((raw_node_id, raw_addr)) = part.split_once('=') else {
            return Err(DbError::internal(format!(
                "invalid remote node list for {key}: expected node_id=host:port in \"{part}\"",
            )));
        };
        let node_id = raw_node_id.trim();
        if node_id.is_empty() {
            return Err(DbError::internal(format!(
                "invalid remote node list for {key}: empty node_id in \"{part}\"",
            )));
        }
        if !seen.insert(node_id.to_ascii_lowercase()) {
            return Err(DbError::internal(format!(
                "invalid remote node list for {key}: duplicate node_id \"{node_id}\"",
            )));
        }

        let addr = raw_addr.trim();
        validate_remote_node_addr(key, addr)?;
        nodes.push(RemoteNodeConfig {
            node_id: node_id.to_owned(),
            addr: addr.to_owned(),
        });
    }

    Ok(nodes)
}

fn validate_remote_node_addr(key: &str, addr: &str) -> DbResult<()> {
    if addr.is_empty() {
        return Err(DbError::internal(format!(
            "invalid remote node address for {key}: empty address",
        )));
    }
    let Some((host, raw_port)) = addr.rsplit_once(':') else {
        return Err(DbError::internal(format!(
            "invalid remote node address for {key}: expected host:port, got \"{addr}\"",
        )));
    };
    if host.trim().is_empty() {
        return Err(DbError::internal(format!(
            "invalid remote node address for {key}: host must not be empty (\"{addr}\")",
        )));
    }
    let port = parse_u16(key, raw_port.trim())?;
    if port == 0 {
        return Err(DbError::internal(format!(
            "invalid remote node address for {key}: port must be >= 1 (\"{addr}\")",
        )));
    }
    Ok(())
}

fn parse_unsigned<T: std::str::FromStr>(key: &str, value: &str) -> DbResult<T>
where
    T::Err: std::fmt::Display,
{
    let type_name = std::any::type_name::<T>();
    if value.starts_with('+') {
        return Err(DbError::internal(format!(
            "invalid {type_name} for {key}: {value}"
        )));
    }
    value.parse::<T>().map_err(|error| {
        DbError::internal(format!("invalid {type_name} for {key}: {value} ({error})"))
    })
}

fn parse_u32(key: &str, value: &str) -> DbResult<u32> {
    parse_unsigned(key, value)
}

fn parse_u64(key: &str, value: &str) -> DbResult<u64> {
    parse_unsigned(key, value)
}

fn parse_usize(key: &str, value: &str) -> DbResult<usize> {
    parse_unsigned(key, value)
}

fn parse_u16(key: &str, value: &str) -> DbResult<u16> {
    parse_unsigned(key, value)
}

fn parse_tls_mode(value: &str) -> DbResult<TlsMode> {
    match value.to_ascii_lowercase().as_str() {
        "disable" => Ok(TlsMode::Disable),
        "prefer" => Ok(TlsMode::Prefer),
        "require" => Ok(TlsMode::Require),
        other => Err(DbError::internal(format!("invalid TLS mode: {other}"))),
    }
}

#[cfg(test)]
mod tests;
