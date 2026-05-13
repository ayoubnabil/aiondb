//! `AionDB` server binary.
//!
//! Starts the pgwire-compatible TCP server.
//! Runtime config defaults to `durable` storage backend.  Persistent backends
//! require the data directory to reside on a LUKS/dm-crypt encrypted filesystem,
//! or the operator must set `AIONDB_ALLOW_UNENCRYPTED_STORAGE=true`.
//!
//! Public `v0.1` contract:
//! - single-node only, no clustering/failover orchestration
//! - no native encryption at rest - filesystem-level encryption (LUKS) required
//!   for persistent backends
//! - canonical SQL dump/restore is supported as the storage safety path
//! - binary online backup and point-in-time recovery are out of scope
//!
//! # CLI flags
//!
//! * `--data-dir <path>` -- directory for WAL and other persistent state
//!   (default: `./data/aiondb`, overridden by `AIONDB_STORAGE_DATA_DIR`).
//! * `--storage-backend <backend>` -- select `in_memory`, `durable`, `disk`,
//!   `page_engine`, or `lsm` (default: `durable`).
//! * `--ephemeral` -- run entirely in memory with no persistence.  **For
//!   test/development only.**  Equivalent to `--storage-backend in_memory` or
//!   setting `AIONDB_IN_MEMORY=true`.
//!
//! # Environment variables
//!
//! * `AIONDB_IN_MEMORY=true` -- same as `--ephemeral`.
//! * `AIONDB_STORAGE_BACKEND=in_memory|durable|disk|page_engine|lsm` -- same as
//!   `--storage-backend <backend>`.
//! * `AIONDB_STORAGE_DATA_DIR=<path>` -- same as `--data-dir <path>`.
//! * `AIONDB_PGWIRE_TLS_MODE=disable|prefer|require` -- pgwire TLS policy.
//! * `AIONDB_PGWIRE_TLS_CERT_PATH=<path>` -- PEM certificate chain for pgwire TLS.
//! * `AIONDB_PGWIRE_TLS_KEY_PATH=<path>` -- PEM private key for pgwire TLS.
//! * `AIONDB_PGWIRE_TLS_CLIENT_CA_PATH=<path>` -- optional PEM CA for mTLS.
//! * `AIONDB_OBSERVABILITY_BIND=<addr>` -- bind address for the HTTP observability server
//!   (default: `127.0.0.1`).
//! * `AIONDB_OBSERVABILITY_PORT=<port>` -- port for `/livez`, `/healthz`, `/readyz`,
//!   `/metrics`, and `/info`
//!   (default: `9187`).
//! * `AIONDB_OBSERVABILITY_FAIL_FAST=true|false` -- when `true`, startup fails if the
//!   observability HTTP server cannot initialize (default: `false`, continue in degraded mode).
//! * `AIONDB_DISTRIBUTED_FRAGMENT_TRANSPORT_FAIL_FAST=true|false` -- when `true`, startup fails
//!   if the fragment transport listener cannot initialize (default: `false`, continue in degraded mode).
//! * `AIONDB_ENABLE_EXPERIMENTAL_DISTRIBUTED=true` -- opt in to v0.1
//!   experimental distributed/sharding/fragment transport code. Without
//!   this flag the server refuses distributed runtime configuration.
//! * `AIONDB_ALLOW_PUBLIC_OBSERVABILITY` and `AIONDB_DISABLE_MEMORY_GUARD` are
//!   treated as unsafe overrides and are ignored by the server binary.
//! * `AIONDB_ALLOW_UNENCRYPTED_STORAGE=true` -- allow persistent storage without
//!   LUKS/dm-crypt encryption.  Data will be written unencrypted to disk.
//!   Use only for development or when filesystem encryption is handled externally.
//! * `AIONDB_BOOTSTRAP_USER=<name>` / `AIONDB_BOOTSTRAP_PASSWORD=<password>` --
//!   when both are set, provision a superuser role at startup (idempotent).
//!   **Dev, CI, and benchmark harness only - never use in production.** The
//!   password must satisfy the server security baseline (≥12 chars, mixed case,
//!   digit, symbol).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use aiondb_config::{
    env::{env_bool, env_string, env_u16},
    pgwire::split_listen_addr,
    storage::{DurableWalCommitPolicy, DEFAULT_SERVER_STORAGE_DATA_DIR},
    total_system_memory, DistributedConfig, ProductSupportLevel, ReplicationRole, RuntimeConfig,
    SecurityProfile, StorageBackend, TlsMode, V0_1_PRODUCT_CONSTRAINTS,
};
use aiondb_engine::{
    AccessRequest, AllowAllAuthorizer, AuthenticatedIdentity, Authorizer, DatabaseId, DbResult,
    Engine, EngineBuilder, QueryEngine, SqlState, StartupParams, TransportInfo,
};
use aiondb_fragment_transport::server::FragmentServerConfig;
use aiondb_fragment_transport::{AuthToken, FragmentServer};
use aiondb_pgwire::server::{PgWireConfig, PgWireServer, ServerHealthSnapshot};
use aiondb_pgwire::tls::{validate_tls_config, TlsConfig};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tokio::sync::watch;
use tracing::{error, info, warn};

mod ha_runtime;
mod replica_runtime;
mod runtime_bootstrap;

const DEFAULT_OBSERVABILITY_BIND_ADDRESS: &str = "127.0.0.1";
const DEFAULT_OBSERVABILITY_PORT: u16 = 9187;
const OBSERVABILITY_FAIL_FAST_ENV: &str = "AIONDB_OBSERVABILITY_FAIL_FAST";
const FRAGMENT_TRANSPORT_FAIL_FAST_ENV: &str = "AIONDB_DISTRIBUTED_FRAGMENT_TRANSPORT_FAIL_FAST";
const EXPERIMENTAL_DISTRIBUTED_ENV: &str = "AIONDB_ENABLE_EXPERIMENTAL_DISTRIBUTED";
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const MEMORY_GUARD_FALLBACK_HOST_BYTES: u64 = 8 * GIB;
const MEMORY_GUARD_MIN_DB_BUDGET_BYTES: u64 = 256 * MIB;
const MEMORY_GUARD_MIN_PER_QUERY_BYTES: u64 = 16 * MIB;
const MEMORY_GUARD_MAX_PER_QUERY_BYTES: u64 = 512 * MIB;
const MEMORY_GUARD_MAX_TEMP_BYTES: u64 = 2 * GIB;
const MEMORY_GUARD_TIER8_MAX_MEMORY_BYTES: u64 = 128 * MIB;
const MEMORY_GUARD_TIER8_MAX_TEMP_BYTES: u64 = 512 * MIB;
const MEMORY_GUARD_TIER8_MAX_RESULT_BYTES: u64 = 16 * MIB;
const MEMORY_GUARD_TIER8_MAX_RESULT_ROWS: u64 = 200_000;
const MEMORY_GUARD_TIER8_MAX_CONNECTIONS: u32 = 8;
const MEMORY_GUARD_TIER8_MAX_WORKERS: usize = 4;
const MEMORY_GUARD_TIER8_MAX_PORTALS: usize = 32;
const MEMORY_GUARD_TIER8_MAX_PREPARED: usize = 64;
const MEMORY_GUARD_TIER8_MAX_RECURSIVE_ROWS: usize = 200_000;
const MEMORY_GUARD_TIER8_MAX_RECURSIVE_ITERS: usize = 2_000;
const MEMORY_GUARD_TIER16_MAX_MEMORY_BYTES: u64 = 256 * MIB;
const MEMORY_GUARD_TIER16_MAX_TEMP_BYTES: u64 = GIB;
const MEMORY_GUARD_TIER16_MAX_RESULT_BYTES: u64 = 32 * MIB;
const MEMORY_GUARD_TIER16_MAX_RESULT_ROWS: u64 = 1_000_000;
const MEMORY_GUARD_TIER16_MAX_CONNECTIONS: u32 = 64;
const MEMORY_GUARD_TIER16_MAX_WORKERS: usize = 8;
const MEMORY_GUARD_TIER16_MAX_PORTALS: usize = 64;
const MEMORY_GUARD_TIER16_MAX_PREPARED: usize = 128;
const MEMORY_GUARD_TIER16_MAX_RECURSIVE_ROWS: usize = 500_000;
const MEMORY_GUARD_TIER16_MAX_RECURSIVE_ITERS: usize = 5_000;
const SERVER_MIN_PASSWORD_LENGTH: usize = 12;
const SERVER_DEFAULT_MAX_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(60 * 15);
const SERVER_DEFAULT_MAX_SESSION_LIFETIME: Duration = Duration::from_secs(60 * 60 * 4);
const SERVER_DEFAULT_MAX_TRANSACTION_IDLE_TIMEOUT: Duration = Duration::from_secs(60 * 10);
const SERVER_DEFAULT_MAX_SESSIONS_PER_ROLE: u32 = 50;

#[derive(Debug, Default)]
struct ServerSessionAuthorizer;

impl Authorizer for ServerSessionAuthorizer {
    fn authorize(
        &self,
        _identity: &AuthenticatedIdentity,
        _request: &AccessRequest,
    ) -> DbResult<()> {
        // The server binary authenticates sessions here, then relies on the
        // engine's catalog-backed ACL enforcement for statement privileges.
        Ok(())
    }
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
}

fn runtime_config() -> RuntimeConfig {
    aiondb_config::load_from_env().unwrap_or_else(|err| {
        eprintln!("failed to load config from environment: {err}");
        std::process::exit(1);
    })
}

fn apply_memory_safety_guard(config: &mut RuntimeConfig) {
    if env_bool("AIONDB_DISABLE_MEMORY_GUARD", false) {
        warn!(
            "AIONDB_DISABLE_MEMORY_GUARD is ignored for safety; memory safety guard remains enabled"
        );
    }

    let host_memory_bytes = total_system_memory().unwrap_or(MEMORY_GUARD_FALLBACK_HOST_BYTES);
    apply_memory_safety_guard_with_host_memory(config, host_memory_bytes);
}

fn apply_memory_safety_guard_with_host_memory(config: &mut RuntimeConfig, host_memory_bytes: u64) {
    if host_memory_bytes <= 8 * GIB {
        clamp_u64(
            &mut config.limits.max_memory_bytes,
            MEMORY_GUARD_TIER8_MAX_MEMORY_BYTES,
            "max_memory_bytes",
        );
        clamp_u64(
            &mut config.limits.max_temp_bytes,
            MEMORY_GUARD_TIER8_MAX_TEMP_BYTES,
            "max_temp_bytes",
        );
        clamp_u64(
            &mut config.limits.max_result_bytes,
            MEMORY_GUARD_TIER8_MAX_RESULT_BYTES,
            "max_result_bytes",
        );
        clamp_u64(
            &mut config.limits.max_result_rows,
            MEMORY_GUARD_TIER8_MAX_RESULT_ROWS,
            "max_result_rows",
        );
        clamp_u32(
            &mut config.pgwire.max_connections,
            MEMORY_GUARD_TIER8_MAX_CONNECTIONS,
            "max_connections",
        );
        clamp_usize(
            &mut config.limits.max_portals,
            MEMORY_GUARD_TIER8_MAX_PORTALS,
            "max_portals",
        );
        clamp_usize(
            &mut config.limits.max_prepared_statements,
            MEMORY_GUARD_TIER8_MAX_PREPARED,
            "max_prepared_statements",
        );
        clamp_usize(
            &mut config.limits.max_recursive_rows,
            MEMORY_GUARD_TIER8_MAX_RECURSIVE_ROWS,
            "max_recursive_rows",
        );
        clamp_usize(
            &mut config.limits.max_recursive_iterations,
            MEMORY_GUARD_TIER8_MAX_RECURSIVE_ITERS,
            "max_recursive_iterations",
        );
        clamp_usize(
            &mut config.pgwire.engine_pool.worker_threads,
            MEMORY_GUARD_TIER8_MAX_WORKERS,
            "engine_pool.worker_threads",
        );
    } else if host_memory_bytes <= 16 * GIB {
        clamp_u64(
            &mut config.limits.max_memory_bytes,
            MEMORY_GUARD_TIER16_MAX_MEMORY_BYTES,
            "max_memory_bytes",
        );
        clamp_u64(
            &mut config.limits.max_temp_bytes,
            MEMORY_GUARD_TIER16_MAX_TEMP_BYTES,
            "max_temp_bytes",
        );
        clamp_u64(
            &mut config.limits.max_result_bytes,
            MEMORY_GUARD_TIER16_MAX_RESULT_BYTES,
            "max_result_bytes",
        );
        clamp_u64(
            &mut config.limits.max_result_rows,
            MEMORY_GUARD_TIER16_MAX_RESULT_ROWS,
            "max_result_rows",
        );
        clamp_u32(
            &mut config.pgwire.max_connections,
            MEMORY_GUARD_TIER16_MAX_CONNECTIONS,
            "max_connections",
        );
        clamp_usize(
            &mut config.limits.max_portals,
            MEMORY_GUARD_TIER16_MAX_PORTALS,
            "max_portals",
        );
        clamp_usize(
            &mut config.limits.max_prepared_statements,
            MEMORY_GUARD_TIER16_MAX_PREPARED,
            "max_prepared_statements",
        );
        clamp_usize(
            &mut config.limits.max_recursive_rows,
            MEMORY_GUARD_TIER16_MAX_RECURSIVE_ROWS,
            "max_recursive_rows",
        );
        clamp_usize(
            &mut config.limits.max_recursive_iterations,
            MEMORY_GUARD_TIER16_MAX_RECURSIVE_ITERS,
            "max_recursive_iterations",
        );
        clamp_usize(
            &mut config.pgwire.engine_pool.worker_threads,
            MEMORY_GUARD_TIER16_MAX_WORKERS,
            "engine_pool.worker_threads",
        );
    }

    let os_reserve_bytes = (host_memory_bytes / 3).max(GIB);
    let mut db_budget_bytes = host_memory_bytes.saturating_sub(os_reserve_bytes);
    if db_budget_bytes < MEMORY_GUARD_MIN_DB_BUDGET_BYTES {
        db_budget_bytes = MEMORY_GUARD_MIN_DB_BUDGET_BYTES;
    }

    let per_query_cap = (db_budget_bytes / 8).clamp(
        MEMORY_GUARD_MIN_PER_QUERY_BYTES,
        MEMORY_GUARD_MAX_PER_QUERY_BYTES,
    );
    if config.limits.max_memory_bytes > per_query_cap {
        warn!(
            previous = config.limits.max_memory_bytes,
            clamped = per_query_cap,
            "clamping max_memory_bytes by memory safety guard"
        );
        config.limits.max_memory_bytes = per_query_cap;
    }

    let temp_cap = (db_budget_bytes / 4).clamp(
        MEMORY_GUARD_MIN_PER_QUERY_BYTES * 2,
        MEMORY_GUARD_MAX_TEMP_BYTES,
    );
    if config.limits.max_temp_bytes > temp_cap {
        warn!(
            previous = config.limits.max_temp_bytes,
            clamped = temp_cap,
            "clamping max_temp_bytes by memory safety guard"
        );
        config.limits.max_temp_bytes = temp_cap;
    }

    if config.pgwire.max_connections == 0 {
        config.pgwire.max_connections = 1;
    }
    if config.pgwire.max_connections_per_ip == 0 {
        config.pgwire.max_connections_per_ip = 1;
    }

    let per_connection_reservation = config
        .limits
        .max_memory_bytes
        .saturating_add(config.limits.max_temp_bytes / 4)
        .max(MEMORY_GUARD_MIN_PER_QUERY_BYTES);
    let safe_connections = (db_budget_bytes / per_connection_reservation)
        .max(1)
        .min(u64::from(u32::MAX));
    let safe_connections = u32::try_from(safe_connections).unwrap_or(u32::MAX);
    if config.pgwire.max_connections > safe_connections {
        info!(
            previous = config.pgwire.max_connections,
            clamped = safe_connections,
            "clamping max_connections by memory safety guard"
        );
        config.pgwire.max_connections = safe_connections;
    }
    if config.pgwire.max_connections_per_ip > config.pgwire.max_connections {
        info!(
            previous = config.pgwire.max_connections_per_ip,
            clamped = config.pgwire.max_connections,
            "clamping max_connections_per_ip to max_connections"
        );
        config.pgwire.max_connections_per_ip = config.pgwire.max_connections;
    }

    let safe_workers = usize::try_from(config.pgwire.max_connections.clamp(1, 8)).unwrap_or(8);
    if config.pgwire.engine_pool.worker_threads > safe_workers {
        info!(
            previous = config.pgwire.engine_pool.worker_threads,
            clamped = safe_workers,
            "clamping engine_pool.worker_threads by memory safety guard"
        );
        config.pgwire.engine_pool.worker_threads = safe_workers;
    }

    let safe_queue_depth = usize::try_from(config.pgwire.max_connections)
        .unwrap_or(usize::MAX)
        .saturating_mul(4)
        .clamp(32, 512);
    if config.pgwire.engine_pool.queue_depth > safe_queue_depth {
        info!(
            previous = config.pgwire.engine_pool.queue_depth,
            clamped = safe_queue_depth,
            "clamping engine_pool.queue_depth by memory safety guard"
        );
        config.pgwire.engine_pool.queue_depth = safe_queue_depth;
    }
}

fn clamp_u64(value: &mut u64, max: u64, label: &str) {
    if *value > max {
        info!(
            previous = *value,
            clamped = max,
            "{label} clamped by memory safety guard"
        );
        *value = max;
    }
}

fn clamp_u32(value: &mut u32, max: u32, label: &str) {
    if *value > max {
        info!(
            previous = *value,
            clamped = max,
            "{label} clamped by memory safety guard"
        );
        *value = max;
    }
}

fn clamp_usize(value: &mut usize, max: usize, label: &str) {
    if *value > max {
        info!(
            previous = *value,
            clamped = max,
            "{label} clamped by memory safety guard"
        );
        *value = max;
    }
}

fn bind_address_is_loopback(bind_address: &str) -> bool {
    let normalized = bind_address.trim_matches(|ch| ch == '[' || ch == ']');
    normalized.eq_ignore_ascii_case("localhost")
        || normalized
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

fn pgwire_listen_addr_is_loopback(listen_addr: &str) -> bool {
    let (bind_address, _) = split_listen_addr(listen_addr);
    bind_address_is_loopback(&bind_address)
}

fn log_v0_1_product_contract() {
    let summary = V0_1_PRODUCT_CONSTRAINTS.startup_warnings().join(" | ");
    warn!(
        release_line = V0_1_PRODUCT_CONSTRAINTS.release_line,
        "{summary}"
    );
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservabilityConfig {
    bind_address: String,
    port: u16,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            bind_address: DEFAULT_OBSERVABILITY_BIND_ADDRESS.to_owned(),
            port: DEFAULT_OBSERVABILITY_PORT,
        }
    }
}

fn observability_config(cli: &CliArgs) -> ObservabilityConfig {
    let mut config = ObservabilityConfig {
        bind_address: env_string(
            "AIONDB_OBSERVABILITY_BIND",
            DEFAULT_OBSERVABILITY_BIND_ADDRESS,
        ),
        port: env_u16("AIONDB_OBSERVABILITY_PORT", DEFAULT_OBSERVABILITY_PORT),
    };
    if let Some(bind_address) = &cli.observability_bind {
        config.bind_address.clone_from(bind_address);
    }
    if let Some(port) = cli.observability_port {
        config.port = port;
    }
    config
}

fn observability_fail_fast() -> bool {
    env_bool(OBSERVABILITY_FAIL_FAST_ENV, false)
}

fn fragment_transport_fail_fast() -> bool {
    env_bool(FRAGMENT_TRANSPORT_FAIL_FAST_ENV, false)
}

#[derive(Clone)]
struct ObservabilityState {
    server: Arc<PgWireServer<Engine>>,
    replica_metrics: Option<aiondb_replication::ReplicaMetrics>,
}

fn product_support_metric_value(level: ProductSupportLevel) -> u8 {
    u8::from(matches!(level, ProductSupportLevel::Supported))
}

fn observability_info_payload() -> serde_json::Value {
    json!({
        "release_line": V0_1_PRODUCT_CONSTRAINTS.release_line,
        "deployment": {
            "mode": V0_1_PRODUCT_CONSTRAINTS.topology,
            "clustering": V0_1_PRODUCT_CONSTRAINTS.clustering.as_str(),
            "summary": V0_1_PRODUCT_CONSTRAINTS.clustering_summary(),
        },
        "storage": {
            "encryption_at_rest": V0_1_PRODUCT_CONSTRAINTS.encryption_at_rest.as_str(),
            "summary": V0_1_PRODUCT_CONSTRAINTS.encryption_at_rest_summary(),
        },
        "operations": {
            "backup_restore": V0_1_PRODUCT_CONSTRAINTS.backup_restore.as_str(),
            "summary": V0_1_PRODUCT_CONSTRAINTS.backup_restore_summary(),
        },
    })
}

fn write_gauge_metric(output: &mut String, name: &str, help: &str, value: u8) {
    // `writeln!` to a `String` is infallible; ignore the `Ok` to silence unused-result warnings.
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} gauge");
    let _ = writeln!(output, "{name} {value}");
}

fn write_u64_gauge_metric(output: &mut String, name: &str, help: &str, value: u64) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} gauge");
    let _ = writeln!(output, "{name} {value}");
}

fn write_labeled_u64_gauge_metric(
    output: &mut String,
    name: &str,
    labels: &[(&str, &str)],
    value: u64,
) {
    let _ = write!(output, "{name}{{");
    for (index, (label, value)) in labels.iter().enumerate() {
        if index > 0 {
            let _ = write!(output, ",");
        }
        let _ = write!(
            output,
            "{label}=\"{}\"",
            prometheus_escape_label_value(value)
        );
    }
    let _ = writeln!(output, "}} {value}");
}

fn prometheus_escape_label_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn append_product_contract_metrics(output: &mut String) {
    write_gauge_metric(
        output,
        "aiondb_product_single_node_mode",
        "Whether the public AionDB release contract is single-node only.",
        u8::from(V0_1_PRODUCT_CONSTRAINTS.topology == "single-node"),
    );
    write_gauge_metric(
        output,
        "aiondb_product_clustering_supported",
        "Whether clustering is part of the public AionDB release contract.",
        product_support_metric_value(V0_1_PRODUCT_CONSTRAINTS.clustering),
    );
    write_gauge_metric(
        output,
        "aiondb_product_encryption_at_rest_supported",
        "Whether encryption at rest is part of the public AionDB release contract.",
        product_support_metric_value(V0_1_PRODUCT_CONSTRAINTS.encryption_at_rest),
    );
    write_gauge_metric(
        output,
        "aiondb_product_backup_restore_supported",
        "Whether backup/restore is part of the public AionDB release contract.",
        product_support_metric_value(V0_1_PRODUCT_CONSTRAINTS.backup_restore),
    );
}

fn append_distributed_remote_metrics(output: &mut String, engine: &Engine) {
    let Some(snapshot) = engine.distributed_node_registry_snapshot() else {
        write_u64_gauge_metric(
            output,
            "aiondb_distributed_remote_nodes_total",
            "Configured remote fragment execution nodes.",
            0,
        );
        return;
    };

    write_u64_gauge_metric(
        output,
        "aiondb_distributed_remote_nodes_total",
        "Configured remote fragment execution nodes.",
        snapshot.total_nodes as u64,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_distributed_remote_nodes_available",
        "Remote fragment execution nodes whose circuit is not open.",
        snapshot.available_nodes as u64,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_distributed_remote_circuits_open",
        "Remote fragment execution node circuits currently open.",
        snapshot.open_circuits as u64,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_distributed_remote_circuits_half_open",
        "Remote fragment execution node circuits currently half-open.",
        snapshot.half_open_circuits as u64,
    );

    let _ = writeln!(
        output,
        "# HELP aiondb_distributed_remote_node_available Remote fragment execution node availability by node."
    );
    let _ = writeln!(
        output,
        "# TYPE aiondb_distributed_remote_node_available gauge"
    );
    let _ = writeln!(
        output,
        "# HELP aiondb_distributed_remote_node_circuit_state Remote fragment execution circuit state by node: closed=0, half_open=1, open=2."
    );
    let _ = writeln!(
        output,
        "# TYPE aiondb_distributed_remote_node_circuit_state gauge"
    );
    let _ = writeln!(
        output,
        "# HELP aiondb_distributed_remote_node_consecutive_failures Consecutive remote fragment dispatch failures by node."
    );
    let _ = writeln!(
        output,
        "# TYPE aiondb_distributed_remote_node_consecutive_failures gauge"
    );
    for node in snapshot.nodes {
        let labels = [
            ("node_id", node.node_id.as_str()),
            ("addr", node.addr.as_str()),
        ];
        write_labeled_u64_gauge_metric(
            output,
            "aiondb_distributed_remote_node_available",
            &labels,
            u64::from(node.available),
        );
        write_labeled_u64_gauge_metric(
            output,
            "aiondb_distributed_remote_node_circuit_state",
            &labels,
            node.circuit_breaker_state.metric_value(),
        );
        write_labeled_u64_gauge_metric(
            output,
            "aiondb_distributed_remote_node_consecutive_failures",
            &labels,
            u64::from(node.consecutive_failures),
        );
    }
}

fn append_distributed_control_plane_metrics(output: &mut String, engine: &Engine) {
    let Ok(snapshot) = engine.distributed_control_plane_snapshot() else {
        return;
    };

    write_u64_gauge_metric(
        output,
        "aiondb_distributed_control_plane_nodes_total",
        "Nodes registered in the distributed control-plane membership view.",
        snapshot.total_nodes as u64,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_distributed_control_plane_nodes_live",
        "Live nodes registered in the distributed control-plane membership view.",
        snapshot.live_nodes as u64,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_distributed_control_plane_shards_total",
        "Shards registered in the distributed control-plane placement view.",
        snapshot.total_shards as u64,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_distributed_control_plane_placement_epoch",
        "Current distributed control-plane placement epoch.",
        snapshot.placement_epoch.get(),
    );

    let _ = writeln!(
        output,
        "# HELP aiondb_distributed_control_plane_node_live Control-plane node liveness by node."
    );
    let _ = writeln!(
        output,
        "# TYPE aiondb_distributed_control_plane_node_live gauge"
    );
    for node in snapshot.nodes {
        let labels = [
            ("node_id", node.node_id.as_str()),
            ("endpoint", node.rpc_endpoint.as_str()),
        ];
        write_labeled_u64_gauge_metric(
            output,
            "aiondb_distributed_control_plane_node_live",
            &labels,
            u64::from(node.is_live),
        );
    }
}

fn append_distributed_replication_metrics(output: &mut String, engine: &Engine) {
    let Ok(snapshot) = engine.distributed_replication_status_snapshot(DatabaseId::DEFAULT) else {
        return;
    };

    write_u64_gauge_metric(
        output,
        "aiondb_distributed_replication_shards_total",
        "Shards included in the distributed replication health snapshot.",
        snapshot.total_shards as u64,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_distributed_replication_shards_with_live_quorum",
        "Shards whose live voting replicas satisfy majority quorum.",
        snapshot.shards_with_live_quorum as u64,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_distributed_replication_shards_without_live_quorum",
        "Shards whose live voting replicas do not satisfy majority quorum.",
        snapshot.shards_without_live_quorum as u64,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_distributed_replication_under_replicated_shards",
        "Shards with fewer voting replicas than the configured replication target.",
        snapshot.under_replicated_shards as u64,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_distributed_replication_shards_with_down_voters",
        "Shards with at least one registered down voting replica.",
        snapshot.shards_with_down_voters as u64,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_distributed_replication_shards_with_learners",
        "Shards with at least one staged learner replica.",
        snapshot.shards_with_learners as u64,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_distributed_replication_learner_replicas",
        "Staged learner replicas across all shards.",
        snapshot.learner_replicas as u64,
    );

    let _ = writeln!(
        output,
        "# HELP aiondb_distributed_replication_node_leaders Leader replicas by node."
    );
    let _ = writeln!(
        output,
        "# TYPE aiondb_distributed_replication_node_leaders gauge"
    );
    let _ = writeln!(
        output,
        "# HELP aiondb_distributed_replication_node_voters Voting replicas by node."
    );
    let _ = writeln!(
        output,
        "# TYPE aiondb_distributed_replication_node_voters gauge"
    );
    let _ = writeln!(
        output,
        "# HELP aiondb_distributed_replication_node_learners Learner replicas by node."
    );
    let _ = writeln!(
        output,
        "# TYPE aiondb_distributed_replication_node_learners gauge"
    );
    let _ = writeln!(
        output,
        "# HELP aiondb_distributed_replication_node_down_voters Down voting replicas by node."
    );
    let _ = writeln!(
        output,
        "# TYPE aiondb_distributed_replication_node_down_voters gauge"
    );
    for status in &snapshot.node_statuses {
        let registered = status.registered.to_string();
        let live = status.is_live.to_string();
        let labels = [
            ("node_id", status.node_id.as_str()),
            ("registered", registered.as_str()),
            ("live", live.as_str()),
        ];
        write_labeled_u64_gauge_metric(
            output,
            "aiondb_distributed_replication_node_leaders",
            &labels,
            status.leader_replicas as u64,
        );
        write_labeled_u64_gauge_metric(
            output,
            "aiondb_distributed_replication_node_voters",
            &labels,
            status.voting_replicas as u64,
        );
        write_labeled_u64_gauge_metric(
            output,
            "aiondb_distributed_replication_node_learners",
            &labels,
            status.learner_replicas as u64,
        );
        write_labeled_u64_gauge_metric(
            output,
            "aiondb_distributed_replication_node_down_voters",
            &labels,
            status.down_voting_replicas as u64,
        );
    }

    let _ = writeln!(
        output,
        "# HELP aiondb_distributed_replication_shard_live_quorum Whether each shard has majority live voting quorum."
    );
    let _ = writeln!(
        output,
        "# TYPE aiondb_distributed_replication_shard_live_quorum gauge"
    );
    let _ = writeln!(
        output,
        "# HELP aiondb_distributed_replication_shard_live_voters Live voting replicas by shard."
    );
    let _ = writeln!(
        output,
        "# TYPE aiondb_distributed_replication_shard_live_voters gauge"
    );
    let _ = writeln!(
        output,
        "# HELP aiondb_distributed_replication_shard_voters Voting replicas by shard."
    );
    let _ = writeln!(
        output,
        "# TYPE aiondb_distributed_replication_shard_voters gauge"
    );
    let _ = writeln!(
        output,
        "# HELP aiondb_distributed_replication_shard_under_replicated Whether each shard is below the configured voting replica target."
    );
    let _ = writeln!(
        output,
        "# TYPE aiondb_distributed_replication_shard_under_replicated gauge"
    );
    let _ = writeln!(
        output,
        "# HELP aiondb_distributed_replication_shard_down_voters Down registered voting replicas by shard."
    );
    let _ = writeln!(
        output,
        "# TYPE aiondb_distributed_replication_shard_down_voters gauge"
    );
    let _ = writeln!(
        output,
        "# HELP aiondb_distributed_replication_shard_learners Learner replicas by shard."
    );
    let _ = writeln!(
        output,
        "# TYPE aiondb_distributed_replication_shard_learners gauge"
    );
    for status in snapshot.statuses {
        let database_id = status.database_id.get().to_string();
        let table_id = status.table_id.get().to_string();
        let shard_id = status.shard_id.get().to_string();
        let leader = status
            .leader
            .as_ref()
            .map_or("", aiondb_engine::NodeId::as_str);
        let labels = [
            ("database_id", database_id.as_str()),
            ("table_id", table_id.as_str()),
            ("shard_id", shard_id.as_str()),
            ("leader", leader),
        ];
        write_labeled_u64_gauge_metric(
            output,
            "aiondb_distributed_replication_shard_live_quorum",
            &labels,
            u64::from(status.has_live_quorum),
        );
        write_labeled_u64_gauge_metric(
            output,
            "aiondb_distributed_replication_shard_live_voters",
            &labels,
            status.live_voting_replicas as u64,
        );
        write_labeled_u64_gauge_metric(
            output,
            "aiondb_distributed_replication_shard_voters",
            &labels,
            status.voting_replicas as u64,
        );
        write_labeled_u64_gauge_metric(
            output,
            "aiondb_distributed_replication_shard_under_replicated",
            &labels,
            u64::from(status.under_replicated),
        );
        write_labeled_u64_gauge_metric(
            output,
            "aiondb_distributed_replication_shard_down_voters",
            &labels,
            status.down_voting_replicas as u64,
        );
        write_labeled_u64_gauge_metric(
            output,
            "aiondb_distributed_replication_shard_learners",
            &labels,
            status.learner_replicas as u64,
        );
    }
}

fn append_replica_runtime_metrics(
    output: &mut String,
    metrics: Option<&aiondb_replication::ReplicaMetrics>,
) {
    let Some(metrics) = metrics else {
        return;
    };
    let snapshot = metrics.snapshot();

    write_u64_gauge_metric(
        output,
        "aiondb_replica_runtime_sessions_started",
        "Replica streaming sessions started by this process.",
        snapshot.sessions_started,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_replica_runtime_sessions_succeeded",
        "Replica streaming sessions that ended cleanly.",
        snapshot.sessions_succeeded,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_replica_runtime_sessions_failed",
        "Replica streaming sessions that ended with an error.",
        snapshot.sessions_failed,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_replica_runtime_reconnects",
        "Replica streaming reconnect attempts after the first session.",
        snapshot.reconnects,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_replica_runtime_wal_bytes_received",
        "Bytes of WAL payload received by the replica streaming driver.",
        snapshot.wal_bytes_received,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_replica_runtime_standby_status_updates_sent",
        "Standby status updates sent by the replica streaming driver.",
        snapshot.standby_status_updates_sent,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_replica_runtime_last_session_started_at_us",
        "Unix timestamp in microseconds for the last replica streaming session start, or 0 when none started.",
        snapshot.last_session_started_at_us.unwrap_or(0),
    );
}

fn append_replica_wal_receiver_metrics(output: &mut String, engine: &Engine) {
    let Some(manager) = engine.replication_manager() else {
        return;
    };
    let Ok(snapshot) = manager.wal_receiver_status_snapshot() else {
        return;
    };
    let write_lsn = snapshot.write_lsn.get();
    let flush_lsn = snapshot.flush_lsn.get();
    let apply_lsn = snapshot.apply_lsn.get();

    write_u64_gauge_metric(
        output,
        "aiondb_replica_wal_receiver_write_lsn",
        "Last WAL LSN written by the replica WAL receiver.",
        write_lsn,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_replica_wal_receiver_flush_lsn",
        "Last WAL LSN durably flushed by the replica WAL receiver.",
        flush_lsn,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_replica_wal_receiver_apply_lsn",
        "Last WAL LSN applied or acknowledged as replayed by the replica WAL receiver.",
        apply_lsn,
    );
    write_u64_gauge_metric(
        output,
        "aiondb_replica_wal_receiver_write_apply_lag_lsn",
        "Difference between replica WAL receiver write_lsn and apply_lsn.",
        write_lsn.saturating_sub(apply_lsn),
    );
    write_u64_gauge_metric(
        output,
        "aiondb_replica_wal_receiver_flush_apply_lag_lsn",
        "Difference between replica WAL receiver flush_lsn and apply_lsn.",
        flush_lsn.saturating_sub(apply_lsn),
    );
}

fn observability_metrics_prometheus_text(
    server: &PgWireServer<Engine>,
    replica_metrics: Option<&aiondb_replication::ReplicaMetrics>,
) -> String {
    let mut output = server.metrics_prometheus_text();
    append_product_contract_metrics(&mut output);
    append_distributed_remote_metrics(&mut output, server.engine().as_ref());
    append_distributed_control_plane_metrics(&mut output, server.engine().as_ref());
    append_distributed_replication_metrics(&mut output, server.engine().as_ref());
    append_replica_runtime_metrics(&mut output, replica_metrics);
    append_replica_wal_receiver_metrics(&mut output, server.engine().as_ref());
    output
}

async fn metrics_handler(State(state): State<Arc<ObservabilityState>>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        observability_metrics_prometheus_text(
            state.server.as_ref(),
            state.replica_metrics.as_ref(),
        ),
    )
}

fn health_status_code(snapshot: ServerHealthSnapshot) -> StatusCode {
    if snapshot.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn health_handler(State(state): State<Arc<ObservabilityState>>) -> impl IntoResponse {
    let snapshot = state.server.health_snapshot();
    (
        health_status_code(snapshot),
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        snapshot.to_json_string(),
    )
}

async fn live_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "live": true,
            "service": "aiondb-observability",
        })),
    )
}

async fn info_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(observability_info_payload()))
}

fn observability_router(
    server: Arc<PgWireServer<Engine>>,
    replica_metrics: Option<aiondb_replication::ReplicaMetrics>,
) -> Router {
    Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/livez", get(live_handler))
        .route("/healthz", get(health_handler))
        .route("/readyz", get(health_handler))
        .route("/info", get(info_handler))
        .with_state(Arc::new(ObservabilityState {
            server,
            replica_metrics,
        }))
}

async fn spawn_observability_server(
    server: Arc<PgWireServer<Engine>>,
    replica_metrics: Option<aiondb_replication::ReplicaMetrics>,
    config: ObservabilityConfig,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<tokio::task::JoinHandle<()>, std::io::Error> {
    let addr = format!("{}:{}", config.bind_address, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(address = %addr, "observability HTTP server ready");
    let app = observability_router(server, replica_metrics);

    Ok(tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let mut shutdown_rx = shutdown_rx;
                let _ = shutdown_rx.changed().await;
            })
            .await
        {
            error!(%err, "observability HTTP server exited with error");
        }
    }))
}

fn log_observability_degraded_mode() {
    info!(
        fail_fast_env = OBSERVABILITY_FAIL_FAST_ENV,
        "continuing in degraded mode: observability HTTP server is unavailable; pgwire remains available"
    );
}

async fn init_observability_server(
    server: Arc<PgWireServer<Engine>>,
    replica_metrics: Option<aiondb_replication::ReplicaMetrics>,
    config: ObservabilityConfig,
    shutdown_rx: watch::Receiver<bool>,
    fail_fast: bool,
) -> Option<tokio::task::JoinHandle<()>> {
    if !bind_address_is_loopback(&config.bind_address) {
        if let Err(err) = enforce_observability_bind_policy(&config.bind_address) {
            error!(
                bind_address = %config.bind_address,
                fail_fast,
                %err,
                "observability bind policy check failed"
            );
            if fail_fast {
                std::process::exit(1);
            }
            log_observability_degraded_mode();
            return None;
        }
    }

    match spawn_observability_server(server, replica_metrics, config, shutdown_rx).await {
        Ok(task) => Some(task),
        Err(err) => {
            if fail_fast {
                error!(
                    fail_fast,
                    %err,
                    "failed to initialize observability HTTP server"
                );
            } else {
                warn!(
                    fail_fast,
                    %err,
                    "observability HTTP server unavailable at startup"
                );
            }
            if fail_fast {
                std::process::exit(1);
            }
            log_observability_degraded_mode();
            None
        }
    }
}

/// Minimal CLI argument parsing (no external dependency).
struct CliArgs {
    command: CliCommand,
    /// Explicit `--data-dir <path>` value, if provided.
    data_dir: Option<PathBuf>,
    /// Explicit `--storage-backend <backend>` value, if provided.
    storage_backend: Option<StorageBackend>,
    /// Explicit `--listen-addr <host:port>` value, if provided.
    listen_addr: Option<String>,
    /// `--ephemeral` flag (or `AIONDB_IN_MEMORY=true`).
    ephemeral: bool,
    bootstrap_user: Option<String>,
    bootstrap_password: Option<String>,
    allow_unencrypted_storage: bool,
    observability_bind: Option<String>,
    observability_port: Option<u16>,
    disable_observability: bool,
    dump_output: Option<String>,
    restore_input: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CliCommand {
    Serve,
    Doctor,
    Upgrade,
    Dump,
    Restore,
}

fn print_cli_help() {
    println!(
        "\
AionDB server

Usage:
  aiondb [OPTIONS]
  aiondb doctor --data-dir <path>
  aiondb upgrade --data-dir <path>
  aiondb dump --data-dir <path> --output <relative.sql>
  aiondb restore --data-dir <path> --input <relative.sql>

Options:
  --help, -h
      Show this help text and exit.

  --version, -V
      Print the AionDB server version and exit.

  --ephemeral
      Run fully in memory. Data is lost when the process exits.
      Intended for local demos, tests, and benchmarks.

  --listen-addr <host:port>
      PostgreSQL wire listen address.
      Default: 127.0.0.1:5432, or AIONDB_PGWIRE_LISTEN_ADDR when set.

  --data-dir <path>
      Directory for persistent storage state.
      Default: ./data/aiondb, or AIONDB_STORAGE_DATA_DIR when set.
      Ignored when --ephemeral or an in-memory backend is selected.

  --storage-backend <backend>
      Select the storage backend.
      Values: in_memory, durable, disk, page_engine, lsm
      Default: durable, or AIONDB_STORAGE_BACKEND when set.

  --bootstrap-user <name>
  --bootstrap-password <strong-password>
      Create a local superuser at startup without relying on environment vars.
      Dev/CI/benchmark only.

  --allow-unencrypted-storage
      Allow persistent storage on a non-encrypted filesystem.

  --observability-bind <host>
  --observability-port <port>
      Override the HTTP observability endpoint bind/port.

  --no-observability
      Disable the HTTP observability endpoint entirely.

Commands:
  doctor
      Inspect a data directory without opening it for writes. Prints storage
      format version, corruption findings, WAL/snapshot/page status, and
      whether upgrade is possible.

  upgrade
      Create an idempotent storage-format v1 manifest. Refuses ambiguous or
      corrupt state and creates a backup before modifying the data directory.

  dump
      Export the database through the canonical SQL backup format. The output
      path is relative to ./backups and is checksum-protected.

  restore
      Restore a canonical SQL backup into the selected data directory. The
      input path is relative to ./backups.

Common environment:
  AIONDB_PGWIRE_LISTEN_ADDR=127.0.0.1:5432
      PostgreSQL wire listen address used by psql and Postgres drivers.

  AIONDB_BOOTSTRAP_USER=<name>
  AIONDB_BOOTSTRAP_PASSWORD=<strong-password>
      Create a local superuser at startup. Dev/CI/benchmark only. Choose your
      own password — never reuse the example values from the docs in any
      reachable environment.

  AIONDB_BENCH_MODE=1
      DANGEROUS: replaces the server session authorizer with
      AllowAllAuthorizer, bypassing the normal startup/statement authorizer
      gate. Benchmark/CI only. Never set in any deployment that accepts
      untrusted connections.

  AIONDB_OBSERVABILITY_BIND=127.0.0.1
  AIONDB_OBSERVABILITY_PORT=9187
      HTTP observability endpoint: /livez, /healthz, /readyz, /metrics, /info.

  AIONDB_PGWIRE_TLS_MODE=disable|prefer|require
      TLS policy for pgwire. The default is prefer.

  AIONDB_ALLOW_UNENCRYPTED_STORAGE=true
      Required for persistent storage on a non-encrypted filesystem.
      Not needed with --ephemeral.

Examples:
  AIONDB_BOOTSTRAP_USER=admin \\
  AIONDB_BOOTSTRAP_PASSWORD='<choose-a-strong-password>' \\
  aiondb --ephemeral

  psql \"host=127.0.0.1 port=5432 dbname=default user=admin sslmode=prefer\"

  AIONDB_ALLOW_UNENCRYPTED_STORAGE=true \\
  aiondb --data-dir ./data/aiondb --storage-backend durable

Product contract for v0.1:
  Alpha, single-node, non-production.
  PostgreSQL wire and embedded Rust API are the recommended public surfaces.
  Graph and vector features are available but still maturing.
"
    );
}

fn print_cli_version() {
    println!(
        "aiondb {} ({})",
        env!("CARGO_PKG_VERSION"),
        V0_1_PRODUCT_CONSTRAINTS.release_line
    );
}

fn parse_cli_args_from(args: &[String]) -> CliArgs {
    let mut command = CliCommand::Serve;
    let mut data_dir: Option<PathBuf> = None;
    let mut storage_backend: Option<StorageBackend> = None;
    let mut listen_addr = None;
    let mut ephemeral = false;
    let mut bootstrap_user = None;
    let mut bootstrap_password = None;
    let mut allow_unencrypted_storage = false;
    let mut observability_bind = None;
    let mut observability_port = None;
    let mut disable_observability = false;
    let mut dump_output = None;
    let mut restore_input = None;
    let mut i = 1;
    if let Some(first) = args.get(1) {
        match first.as_str() {
            "doctor" => {
                command = CliCommand::Doctor;
                i = 2;
            }
            "upgrade" => {
                command = CliCommand::Upgrade;
                i = 2;
            }
            "dump" => {
                command = CliCommand::Dump;
                i = 2;
            }
            "restore" => {
                command = CliCommand::Restore;
                i = 2;
            }
            _ => {}
        }
    }
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_cli_help();
                std::process::exit(0);
            }
            "--version" | "-V" => {
                print_cli_version();
                std::process::exit(0);
            }
            "--listen-addr" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --listen-addr requires a host:port argument");
                    std::process::exit(1);
                }
                listen_addr = Some(args[i].clone());
            }
            "--data-dir" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --data-dir requires a path argument");
                    std::process::exit(1);
                }
                data_dir = Some(PathBuf::from(&args[i]));
            }
            "--storage-backend" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --storage-backend requires a backend name");
                    std::process::exit(1);
                }
                storage_backend = Some(StorageBackend::parse(&args[i]).unwrap_or_else(|| {
                    eprintln!(
                        "error: invalid storage backend '{}'; expected one of: in_memory, durable, disk, page_engine, lsm",
                        args[i]
                    );
                    std::process::exit(1);
                }));
            }
            "--ephemeral" => {
                ephemeral = true;
            }
            "--bootstrap-user" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --bootstrap-user requires a role name");
                    std::process::exit(1);
                }
                bootstrap_user = Some(args[i].clone());
            }
            "--bootstrap-password" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --bootstrap-password requires a password");
                    std::process::exit(1);
                }
                bootstrap_password = Some(args[i].clone());
            }
            "--allow-unencrypted-storage" => {
                allow_unencrypted_storage = true;
            }
            "--observability-bind" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --observability-bind requires a host argument");
                    std::process::exit(1);
                }
                observability_bind = Some(args[i].clone());
            }
            "--observability-port" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --observability-port requires a port argument");
                    std::process::exit(1);
                }
                observability_port = Some(args[i].parse().unwrap_or_else(|_| {
                    eprintln!("error: invalid observability port '{}'", args[i]);
                    std::process::exit(1);
                }));
            }
            "--no-observability" => {
                disable_observability = true;
            }
            "--output" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --output requires a relative backup path");
                    std::process::exit(1);
                }
                dump_output = Some(args[i].clone());
            }
            "--input" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --input requires a relative backup path");
                    std::process::exit(1);
                }
                restore_input = Some(args[i].clone());
            }
            other => {
                eprintln!("error: unknown argument: {other}");
                eprintln!(
                    "usage: aiondb [--help] [--version] [--data-dir <path>] [--storage-backend <backend>] [--ephemeral]"
                );
                std::process::exit(1);
            }
        }
        i += 1;
    }

    // Also honour the existing environment variable.
    if !ephemeral {
        ephemeral = std::env::var("AIONDB_IN_MEMORY")
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or(false);
    }

    if bootstrap_user.is_some() ^ bootstrap_password.is_some() {
        eprintln!("error: --bootstrap-user and --bootstrap-password must be provided together");
        std::process::exit(1);
    }

    CliArgs {
        command,
        data_dir,
        storage_backend,
        listen_addr,
        ephemeral,
        bootstrap_user,
        bootstrap_password,
        allow_unencrypted_storage,
        observability_bind,
        observability_port,
        disable_observability,
        dump_output,
        restore_input,
    }
}

fn parse_cli_args() -> CliArgs {
    let args: Vec<String> = std::env::args().collect();
    parse_cli_args_from(&args)
}

fn apply_cli_runtime_overrides(cli: &CliArgs, config: &mut RuntimeConfig) {
    if let Some(listen_addr) = &cli.listen_addr {
        config.pgwire.listen_addr.clone_from(listen_addr);
    }
}

fn print_doctor_report(report: &aiondb_storage_engine::StorageDoctorReport) {
    println!("data_dir={}", report.data_dir.display());
    match (report.format_major, report.format_minor) {
        (Some(major), Some(minor)) => println!("storage_format=v{major}.{minor}"),
        _ => println!("storage_format=unknown"),
    }
    println!("manifest_present={}", report.manifest_present);
    println!("stable_files={}", report.stable_files);
    println!("wal_segments={}", report.wal_segments);
    println!("catalog_snapshots={}", report.catalog_snapshots);
    println!("storage_snapshots={}", report.storage_snapshots);
    println!("paged_table_files={}", report.paged_table_files);
    println!("fpw_journals={}", report.fpw_journals);
    println!("checkpoint_manifests={}", report.checkpoint_manifests);
    println!("heap_page_files={}", report.heap_page_files);
    println!("index_page_files={}", report.index_page_files);
    println!("mixed_page_files={}", report.mixed_page_files);
    println!("empty_page_files={}", report.empty_page_files);
    println!("experimental_files={}", report.experimental_files);
    if !report.stable_paths.is_empty() {
        println!("stable_paths:");
        for path in &report.stable_paths {
            println!("  - {path}");
        }
    }
    if !report.experimental_paths.is_empty() {
        println!("experimental_paths:");
        for path in &report.experimental_paths {
            println!("  - {path}");
        }
    }
    println!("upgrade_possible={}", report.upgrade_status());
    if !report.warnings.is_empty() {
        println!("warnings:");
        for warning in &report.warnings {
            println!("  - {warning}");
        }
    }
    if !report.errors.is_empty() {
        println!("errors:");
        for error in &report.errors {
            println!("  - {error}");
        }
    }
    println!("status={}", if report.ok() { "ok" } else { "corrupt" });
}

fn escape_backup_sql_path(path: &str) -> String {
    path.replace('\'', "''")
}

fn run_offline_backup_command(
    data_dir: PathBuf,
    mut runtime_config: RuntimeConfig,
    storage_backend: StorageBackend,
    sql: String,
) -> Result<(), String> {
    if !storage_backend.is_persistent() {
        return Err("dump/restore requires a persistent storage backend".to_owned());
    }
    runtime_config.storage.backend = storage_backend;
    let engine = EngineBuilder::new_with_config(data_dir, runtime_config)
        .map_err(|error| format!("failed to open data-dir: {error}"))?
        .with_authorizer(Arc::new(AllowAllAuthorizer))
        .with_allow_ephemeral_users(true)
        .build()
        .map_err(|error| format!("failed to build offline engine: {error}"))?;
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("aiondb-cli-storage".to_owned()),
            options: Default::default(),
            credential: aiondb_engine::Credential::Anonymous {
                user: "alice".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .map_err(|error| format!("failed to start offline session: {error}"))?;
    engine
        .execute_sql(&session, &sql)
        .map_err(|error| format!("offline command failed: {error}"))?;
    Ok(())
}

fn resolve_storage_backend(
    cli_backend: Option<StorageBackend>,
    ephemeral: bool,
    config: &RuntimeConfig,
) -> StorageBackend {
    if ephemeral {
        StorageBackend::InMemory
    } else {
        cli_backend.unwrap_or(config.storage.backend)
    }
}

/// Detect whether `path` resides on a dm-crypt/LUKS-encrypted block device.
///
/// The check reads `/proc/mounts` to find the backing device for the mount
/// point that contains `path`, then inspects
/// `/sys/dev/block/<major>:<minor>/dm/uuid` for the `CRYPT-` prefix that the
/// kernel device-mapper sets on every dm-crypt volume.
fn path_is_on_encrypted_device(path: &Path) -> bool {
    // Resolve to an absolute path so prefix-matching against mount points works.
    let abs = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(_) => {
            // The directory may not exist yet; try the parent.
            match path.parent().and_then(|p| std::fs::canonicalize(p).ok()) {
                Some(p) => p,
                None => return false,
            }
        }
    };

    // ── 1. Find the mount entry whose mount-point is the longest prefix of
    //       the resolved path.  Format: `device mountpoint fstype options …`
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
        return false; // not on Linux, or /proc unavailable
    };

    let mut best_device = String::new();
    let mut best_len: usize = 0;
    for line in mounts.lines() {
        let mut parts = line.split_whitespace();
        let Some(device) = parts.next() else {
            continue;
        };
        let Some(mount_point) = parts.next() else {
            continue;
        };
        let mp = Path::new(mount_point);
        if abs.starts_with(mp) && mount_point.len() > best_len {
            best_len = mount_point.len();
            device.clone_into(&mut best_device);
        }
    }

    if best_device.is_empty() {
        return false;
    }

    // ── 2. Stat the device to obtain major:minor numbers.
    let dev_path = Path::new(&best_device);
    if !dev_path.exists() {
        return false;
    }

    // Use nix::sys::stat or a raw libc::stat to read the device numbers.
    // Since we deny unsafe code, we fall back to parsing `/sys/block` by
    // iterating dm devices and comparing the device path.

    // ── 3. Walk /sys/block/dm-*/dm/uuid looking for CRYPT- prefix.
    //       If the best_device is /dev/mapper/<name>, we can also resolve
    //       through /dev/mapper/<name> → /sys/devices/.../dm/uuid.
    //
    //       Simplest: if the device path lives under /dev/mapper/ or /dev/dm-*,
    //       resolve its dm/uuid via the sysfs symlink.
    if let Some(dm_name) = best_device.strip_prefix("/dev/mapper/") {
        // /sys/devices/virtual/block/dm-N - we can look it up via
        // /sys/block/dm-*/dm/name and match against dm_name.
        if let Ok(entries) = std::fs::read_dir("/sys/block") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if !name_str.starts_with("dm-") {
                    continue;
                }
                // Check if this dm device has the matching name.
                let dm_name_path = entry.path().join("dm/name");
                if let Ok(n) = std::fs::read_to_string(&dm_name_path) {
                    if n.trim() != dm_name {
                        continue;
                    }
                }
                // Read the uuid - CRYPT- prefix means dm-crypt/LUKS.
                let uuid_path = entry.path().join("dm/uuid");
                if let Ok(uuid) = std::fs::read_to_string(&uuid_path) {
                    if uuid.trim().starts_with("CRYPT-") {
                        return true;
                    }
                }
            }
        }
    }

    // Fallback: check ALL dm devices for CRYPT prefix whose backing device
    // matches.  This handles unusual device paths (e.g., /dev/dm-0).
    if let Ok(entries) = std::fs::read_dir("/sys/block") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with("dm-") {
                continue;
            }
            let uuid_path = entry.path().join("dm/uuid");
            if let Ok(uuid) = std::fs::read_to_string(&uuid_path) {
                if uuid.trim().starts_with("CRYPT-") {
                    // Check if this dm device corresponds to our best_device.
                    let dev_node = PathBuf::from("/dev").join(entry.file_name());
                    if dev_node == Path::new(&best_device) {
                        return true;
                    }
                }
            }
        }
    }

    false
}

fn enforce_storage_encryption_policy(
    storage_backend: StorageBackend,
    data_dir: &Path,
    allow_unencrypted_storage: bool,
) -> Result<(), String> {
    if !storage_backend.is_persistent() {
        return Ok(());
    }

    // Check for filesystem-level encryption (LUKS/dm-crypt).
    if path_is_on_encrypted_device(data_dir) {
        info!(
            data_dir = %data_dir.display(),
            "filesystem encryption detected (dm-crypt/LUKS) — persistent storage allowed"
        );
        return Ok(());
    }

    // Explicit opt-in for unencrypted persistent storage.
    if allow_unencrypted_storage || env_bool("AIONDB_ALLOW_UNENCRYPTED_STORAGE", false) {
        warn!(
            data_dir = %data_dir.display(),
            "AIONDB_ALLOW_UNENCRYPTED_STORAGE=true — persistent storage allowed WITHOUT \
             filesystem encryption.  Data will be written UNENCRYPTED to disk.  \
             Use LUKS/dm-crypt in production."
        );
        return Ok(());
    }

    Err(format!(
        "persistent storage refused: the data directory ({}) is not on a LUKS/dm-crypt \
         encrypted filesystem.  Either:\n  \
         1. Use LUKS/dm-crypt on the partition hosting the data directory, or\n  \
         2. Set AIONDB_ALLOW_UNENCRYPTED_STORAGE=true to accept unencrypted storage, or\n  \
         3. Run with --ephemeral for in-memory mode",
        data_dir.display()
    ))
}

fn enforce_observability_bind_policy_with_override(
    bind_address: &str,
    allow_public_observability: bool,
) -> Result<(), String> {
    if bind_address_is_loopback(bind_address) || allow_public_observability {
        Ok(())
    } else {
        Err("refusing to bind observability HTTP server on a non-loopback address".to_owned())
    }
}

fn enforce_observability_bind_policy(bind_address: &str) -> Result<(), String> {
    if env_bool("AIONDB_ALLOW_PUBLIC_OBSERVABILITY", false) {
        warn!(
            "AIONDB_ALLOW_PUBLIC_OBSERVABILITY is ignored; non-loopback observability binds remain blocked"
        );
    }
    enforce_observability_bind_policy_with_override(bind_address, false)
}

fn resolve_data_dir(cli_data_dir: Option<PathBuf>, config: &RuntimeConfig) -> PathBuf {
    // Precedence: --data-dir flag > AIONDB_STORAGE_DATA_DIR env var > config
    // default > built-in default.
    if let Some(dir) = cli_data_dir {
        return dir;
    }
    let dir = &config.storage.data_dir;
    // StorageConfig defaults to "./data"; use our more specific default when
    // the user hasn't customized it and AIONDB_STORAGE_DATA_DIR is unset.
    if dir.as_path() == Path::new("./data") && std::env::var("AIONDB_STORAGE_DATA_DIR").is_err() {
        PathBuf::from(DEFAULT_SERVER_STORAGE_DATA_DIR)
    } else {
        dir.clone()
    }
}

fn resolve_server_tls(config: &RuntimeConfig) -> Result<(Option<TlsConfig>, bool), String> {
    let listen_is_loopback = pgwire_listen_addr_is_loopback(&config.pgwire.listen_addr);
    let allow_plaintext_public = env_bool("AIONDB_ALLOW_PLAINTEXT_PUBLIC", false);
    if !listen_is_loopback && !matches!(config.pgwire.tls_mode, TlsMode::Require) {
        if allow_plaintext_public {
            warn!(
                listen_addr = %config.pgwire.listen_addr,
                tls_mode = ?config.pgwire.tls_mode,
                "AIONDB_ALLOW_PLAINTEXT_PUBLIC=1: pgwire is bound to a non-loopback address WITHOUT \
                 TLS. Credentials and query payloads travel in cleartext. Use this only for local \
                 evaluation behind trusted infrastructure."
            );
        } else {
            return Err(
                "pgwire listen_addr is not loopback; remote exposure requires tls_mode=require with configured TLS cert/key paths (or set AIONDB_ALLOW_PLAINTEXT_PUBLIC=1 for local evaluation only)"
                    .to_owned(),
            );
        }
    }
    let tls_paths = match (
        config.pgwire.tls_cert_path.as_ref(),
        config.pgwire.tls_key_path.as_ref(),
    ) {
        (Some(cert_path), Some(key_path)) => Some(TlsConfig {
            cert_path: cert_path.display().to_string(),
            key_path: key_path.display().to_string(),
            client_ca_path: config
                .pgwire
                .tls_client_ca_path
                .as_ref()
                .map(|path| path.display().to_string()),
        }),
        (None, None) => None,
        _ => {
            return Err(
                "pgwire TLS configuration is incomplete: set both cert and key paths".to_owned(),
            );
        }
    };

    match config.pgwire.tls_mode {
        TlsMode::Disable => {
            if tls_paths.is_some() {
                warn!(
                    "pgwire TLS cert/key are configured but tls_mode=disable; starting without TLS"
                );
            }
            Ok((None, false))
        }
        TlsMode::Prefer => {
            if let Some(tls) = tls_paths {
                validate_tls_config(&tls)
                    .map_err(|err| format!("invalid pgwire TLS configuration: {err}"))?;
                Ok((Some(tls), false))
            } else if listen_is_loopback || allow_plaintext_public {
                if listen_is_loopback {
                    info!(
                        "pgwire tls_mode=prefer with no TLS cert/key; loopback plaintext remains enabled"
                    );
                } else {
                    warn!(
                        "pgwire tls_mode=prefer but no TLS cert/key are configured; accepting plaintext connections"
                    );
                }
                Ok((None, false))
            } else {
                Err(
                    "pgwire listen_addr is not loopback and tls_mode=prefer has no TLS cert/key configured; configure TLS or bind pgwire to loopback"
                        .to_owned(),
                )
            }
        }
        TlsMode::Require => {
            let tls = tls_paths.ok_or_else(|| {
                "pgwire tls_mode=require requires TLS cert/key paths to be configured".to_owned()
            })?;
            validate_tls_config(&tls)
                .map_err(|err| format!("invalid pgwire TLS configuration: {err}"))?;
            Ok((Some(tls), true))
        }
    }
}

fn apply_server_security_baseline(config: &mut RuntimeConfig, storage_backend: StorageBackend) {
    config.security.require_tls_for_password = true;
    config.security.reject_role_name_as_password = true;
    config.security.password_min_length = config
        .security
        .password_min_length
        .max(SERVER_MIN_PASSWORD_LENGTH);
    config.security.password_require_lowercase = true;
    config.security.password_require_uppercase = true;
    config.security.password_require_digit = true;
    config.security.password_require_symbol = true;
    config.security.ddl_audit_enabled = true;

    if config.security.max_session_idle_timeout.is_none() {
        config.security.max_session_idle_timeout = Some(SERVER_DEFAULT_MAX_SESSION_IDLE_TIMEOUT);
    }
    if config.security.max_session_lifetime.is_none() {
        config.security.max_session_lifetime = Some(SERVER_DEFAULT_MAX_SESSION_LIFETIME);
    }
    if config.security.max_transaction_idle_timeout.is_none() {
        config.security.max_transaction_idle_timeout =
            Some(SERVER_DEFAULT_MAX_TRANSACTION_IDLE_TIMEOUT);
    }

    let session_cap = config
        .security
        .max_concurrent_sessions_per_role
        .unwrap_or(SERVER_DEFAULT_MAX_SESSIONS_PER_ROLE)
        .clamp(1, config.pgwire.max_connections.max(1));
    config.security.max_concurrent_sessions_per_role = Some(session_cap);

    if storage_backend.is_persistent() {
        config.security.durable_auth_lockout = true;
        config.security.durable_auth_audit = true;
    }
}

fn validate_remote_exposure_security(config: &RuntimeConfig) -> Result<(), String> {
    if pgwire_listen_addr_is_loopback(&config.pgwire.listen_addr) {
        return Ok(());
    }

    config
        .security
        .validate_production_requirements()
        .map_err(|issues| {
            format!(
                "pgwire listen_addr is not loopback; remote exposure requires production-like security settings:\n- {}",
                issues.join("\n- ")
            )
        })
}

fn distributed_fragment_transport_enabled(config: &RuntimeConfig) -> bool {
    !config.distributed.remote_nodes.is_empty()
        || config
            .distributed
            .inter_node_auth_token
            .as_ref()
            .is_some_and(|token| !token.trim().is_empty())
        || config.distributed.fragment_transport_port
            != DistributedConfig::default().fragment_transport_port
}

fn experimental_distributed_requested(config: &RuntimeConfig) -> bool {
    config.ha.enabled
        || !config.ha.cluster_nodes.is_empty()
        || config.distributed.sharding.enabled
        || distributed_fragment_transport_enabled(config)
}

fn validate_experimental_release_gates(config: &RuntimeConfig) -> Result<(), String> {
    if experimental_distributed_requested(config) && !env_bool(EXPERIMENTAL_DISTRIBUTED_ENV, false)
    {
        return Err(format!(
            "distributed/sharding/HA runtime is experimental in AionDB v0.1; \
             set {EXPERIMENTAL_DISTRIBUTED_ENV}=true to opt in explicitly"
        ));
    }
    Ok(())
}

fn format_bind_addr(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn fragment_transport_listen_addr(config: &RuntimeConfig) -> String {
    let (bind_address, _) = split_listen_addr(&config.pgwire.listen_addr);
    format_bind_addr(&bind_address, config.distributed.fragment_transport_port)
}

async fn preflight_fragment_transport_bind(listen_addr: &str) -> Result<(), std::io::Error> {
    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    drop(listener);
    Ok(())
}

fn build_fragment_server_tls_config(
    config: &DistributedConfig,
) -> Option<aiondb_fragment_transport::tls::TlsServerConfig> {
    let cert_path = config.tls_cert_path.as_ref()?;
    let key_path = config.tls_key_path.as_ref()?;
    Some(aiondb_fragment_transport::tls::TlsServerConfig {
        cert_path: cert_path.clone(),
        key_path: key_path.clone(),
        client_ca_path: config.tls_ca_cert_path.clone(),
    })
}

fn spawn_fragment_transport_server(
    engine: Arc<Engine>,
    config: &RuntimeConfig,
    shutdown_rx: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let listen_addr = fragment_transport_listen_addr(config);
    let fragment_server = FragmentServer::new(
        FragmentServerConfig {
            listen_addr: listen_addr.clone(),
            auth_token: AuthToken::new(
                config
                    .distributed
                    .inter_node_auth_token
                    .clone()
                    .unwrap_or_default(),
            ),
            tls: build_fragment_server_tls_config(&config.distributed),
            max_connections: config.pgwire.max_connections,
            request_timeout: config.limits.statement_timeout,
            max_concurrent_executions: None, // defaults to max_connections
        },
        engine,
    );

    info!(addr = %listen_addr, "starting fragment transport server");
    tokio::spawn(async move {
        if let Err(err) = fragment_server.run(shutdown_rx).await {
            error!(addr = %listen_addr, %err, "fragment transport server exited with error");
        }
    })
}

fn log_fragment_transport_degraded_mode() {
    warn!(
        fail_fast_env = FRAGMENT_TRANSPORT_FAIL_FAST_ENV,
        "continuing in degraded mode: fragment transport listener is unavailable; pgwire remains available"
    );
}

async fn init_fragment_transport_server(
    engine: Arc<Engine>,
    config: &RuntimeConfig,
    shutdown_rx: watch::Receiver<bool>,
    fail_fast: bool,
) -> Option<tokio::task::JoinHandle<()>> {
    if !distributed_fragment_transport_enabled(config) {
        return None;
    }

    if config
        .distributed
        .inter_node_auth_token
        .as_ref()
        .map_or(true, |token| token.trim().is_empty())
    {
        error!("inter_node_auth_token must be configured before fragment transport can start");
        if fail_fast {
            std::process::exit(1);
        }
        log_fragment_transport_degraded_mode();
        return None;
    }

    if config.distributed.require_tls
        && !config.distributed.remote_nodes.is_empty()
        && config.distributed.tls_cert_path.is_none()
    {
        error!(
            "TLS must be configured for inter-node transport when remote_nodes are present \
             (set tls_cert_path, tls_key_path, tls_ca_cert_path)"
        );
        if fail_fast {
            std::process::exit(1);
        }
        log_fragment_transport_degraded_mode();
        return None;
    }

    // Production security profile: refuse to start fragment transport
    // without TLS and mTLS even if require_tls is false.
    if config.security.profile == SecurityProfile::Production
        && !config.distributed.remote_nodes.is_empty()
    {
        if config.distributed.tls_cert_path.is_none() {
            error!(
                "production security profile requires TLS for inter-node fragment transport \
                 (configure tls_cert_path, tls_key_path, tls_ca_cert_path)"
            );
            if fail_fast {
                std::process::exit(1);
            }
            log_fragment_transport_degraded_mode();
            return None;
        }
        if config.distributed.tls_ca_cert_path.is_none() {
            error!(
                "production security profile requires mutual TLS (mTLS) for inter-node transport \
                 (configure tls_ca_cert_path for client certificate verification)"
            );
            if fail_fast {
                std::process::exit(1);
            }
            log_fragment_transport_degraded_mode();
            return None;
        }
    }

    let listen_addr = fragment_transport_listen_addr(config);
    if let Err(err) = preflight_fragment_transport_bind(&listen_addr).await {
        error!(
            addr = %listen_addr,
            fail_fast,
            %err,
            "failed to initialize fragment transport listener"
        );
        if fail_fast {
            std::process::exit(1);
        }
        log_fragment_transport_degraded_mode();
        return None;
    }

    Some(spawn_fragment_transport_server(engine, config, shutdown_rx))
}

/// Optionally provision a superuser from `AIONDB_BOOTSTRAP_USER` /
/// `AIONDB_BOOTSTRAP_PASSWORD`. Intended for local development, CI, and the
/// benchmark harness - never enable this in a multi-tenant deployment.
///
/// Idempotent: if the role already exists (`SqlState::UniqueViolation`), this
/// returns `Ok(())` and only logs at info level.
fn maybe_bootstrap_role_from_cli_or_env(engine: &Engine, cli: &CliArgs) {
    let user = cli
        .bootstrap_user
        .clone()
        .or_else(|| std::env::var("AIONDB_BOOTSTRAP_USER").ok());
    let password = cli
        .bootstrap_password
        .clone()
        .or_else(|| std::env::var("AIONDB_BOOTSTRAP_PASSWORD").ok());
    let Some(user) = user else {
        return;
    };
    let Some(password) = password else {
        warn!("AIONDB_BOOTSTRAP_USER is set but AIONDB_BOOTSTRAP_PASSWORD is missing; ignoring");
        return;
    };
    match engine.bootstrap_role(&user, &password, true) {
        Ok(()) => warn!(
            role = %user,
            "bootstrapped superuser from AIONDB_BOOTSTRAP_USER/PASSWORD — dev/CI only, not for production"
        ),
        Err(err) if err.sqlstate() == SqlState::UniqueViolation => {
            info!(role = %user, "bootstrap role already exists, leaving unchanged");
        }
        Err(err) => {
            error!(%err, role = %user, "failed to bootstrap role from env");
            std::process::exit(1);
        }
    }
}

fn server_authorizer(bench_mode: bool) -> Arc<dyn Authorizer> {
    if bench_mode {
        warn!(
            "AIONDB_BENCH_MODE=1 — using AllowAllAuthorizer; \
             the server session authorizer gate is bypassed. Benchmark/CI use \
             only, NEVER for any deployment that accepts untrusted connections."
        );
        Arc::new(AllowAllAuthorizer)
    } else {
        Arc::new(ServerSessionAuthorizer)
    }
}

async fn bootstrap_replica_from_primary_if_needed(
    data_dir: &Path,
    config: &RuntimeConfig,
) -> DbResult<()> {
    let metadata_dir = data_dir.join("replication");
    let sysid_path = metadata_dir.join("system_id");
    if sysid_path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        // Replica already bootstrapped; engine recovery will resume streaming
        // from the persisted flush_lsn.
        return Ok(());
    }

    let Some(conninfo_raw) = config.replication.primary_conninfo.as_deref() else {
        return Err(aiondb_engine::DbError::feature_not_supported(
            "replica role requires AIONDB_REPLICATION_PRIMARY_CONNINFO to be set",
        ));
    };
    let conninfo = aiondb_replication::ConnInfo::parse(conninfo_raw)?;

    // Write the storage manifest into the empty data dir BEFORE BASE_BACKUP
    // populates wal/, otherwise the engine sees streamed WAL segments
    // without a manifest and refuses to open.
    aiondb_storage_engine::ensure_storage_contract_for_open(
        data_dir,
        aiondb_storage_engine::StorageBackendKind::Durable,
    )?;

    let wal_dir = data_dir.join("wal");
    std::fs::create_dir_all(&wal_dir).map_err(|err| {
        aiondb_engine::DbError::internal(format!(
            "failed to create replica wal dir {}: {err}",
            wal_dir.display()
        ))
    })?;

    info!(
        target = %wal_dir.display(),
        "fresh replica detected; fetching BASE_BACKUP from primary"
    );
    let header = aiondb_replication::fetch_base_backup(conninfo, wal_dir.clone()).await?;
    info!(
        system_id = %header.system_identifier,
        timeline = header.timeline,
        wal_start_lsn = header.wal_start_lsn.get(),
        "BASE_BACKUP completed; seeding replica replication metadata"
    );

    std::fs::create_dir_all(&metadata_dir).map_err(|err| {
        aiondb_engine::DbError::internal(format!(
            "failed to create replication metadata dir {}: {err}",
            metadata_dir.display()
        ))
    })?;
    std::fs::write(&sysid_path, header.system_identifier.as_bytes()).map_err(|err| {
        aiondb_engine::DbError::internal(format!(
            "failed to seed replication system_id {}: {err}",
            sysid_path.display()
        ))
    })?;
    let timeline_path = metadata_dir.join("timeline");
    std::fs::write(&timeline_path, header.timeline.to_string().as_bytes()).map_err(|err| {
        aiondb_engine::DbError::internal(format!(
            "failed to seed replication timeline {}: {err}",
            timeline_path.display()
        ))
    })?;
    Ok(())
}

fn build_server_engine(
    data_dir_override: Option<PathBuf>,
    config: &RuntimeConfig,
    storage_backend: StorageBackend,
    bench_mode: bool,
) -> DbResult<Arc<Engine>> {
    let connect_authorizer = server_authorizer(bench_mode);

    if storage_backend == StorageBackend::InMemory {
        warn!(
            "starting in ephemeral (in-memory) mode -- data will NOT be persisted. \
             Use --data-dir <path> without --ephemeral for production."
        );
        return Ok(Arc::new(
            EngineBuilder::new_in_memory()
                .with_runtime_config(config.clone())
                .with_authorizer(connect_authorizer)
                .build()?,
        ));
    }

    let data_dir = resolve_data_dir(data_dir_override, config);
    let mut runtime_config = config.clone();
    runtime_config.storage.backend = storage_backend;
    if bench_mode
        && storage_backend == StorageBackend::Durable
        && runtime_config.storage.durable_wal_commit_policy == DurableWalCommitPolicy::Always
        && std::env::var("AIONDB_STORAGE_DURABLE_WAL_COMMIT_POLICY").is_err()
    {
        runtime_config.storage.durable_wal_commit_policy = DurableWalCommitPolicy::Every(64);
        warn!(
            "bench mode active: using AionDB durable WAL commit policy every:64; \
             set AIONDB_STORAGE_DURABLE_WAL_COMMIT_POLICY=always for fsync-on-every-commit"
        );
    }
    info!(
        backend = storage_backend.as_str(),
        data_dir = %data_dir.display(),
        "starting with configured storage backend"
    );

    let builder = EngineBuilder::new_with_config(data_dir, runtime_config)?
        .with_authorizer(connect_authorizer);
    Ok(Arc::new(builder.build()?))
}

fn main() {
    runtime_bootstrap::block_on_server(async_main());
}

async fn async_main() {
    let cli = parse_cli_args();

    let mut config = runtime_config();
    apply_cli_runtime_overrides(&cli, &mut config);
    if matches!(
        cli.command,
        CliCommand::Doctor | CliCommand::Upgrade | CliCommand::Dump | CliCommand::Restore
    ) {
        let data_dir = resolve_data_dir(cli.data_dir.clone(), &config);
        match cli.command {
            CliCommand::Doctor => {
                let report = aiondb_storage_engine::doctor_data_dir(&data_dir);
                print_doctor_report(&report);
                std::process::exit(if report.ok() { 0 } else { 2 });
            }
            CliCommand::Upgrade => match aiondb_storage_engine::upgrade_data_dir(&data_dir) {
                Ok(path) => {
                    println!("upgrade=ok");
                    println!("data_dir={}", data_dir.display());
                    println!("backup_or_manifest={}", path.display());
                    let report = aiondb_storage_engine::doctor_data_dir(&data_dir);
                    print_doctor_report(&report);
                    std::process::exit(0);
                }
                Err(err) => {
                    eprintln!("upgrade=refused");
                    eprintln!("{err}");
                    let report = aiondb_storage_engine::doctor_data_dir(&data_dir);
                    print_doctor_report(&report);
                    std::process::exit(2);
                }
            },
            CliCommand::Dump => {
                let Some(output) = cli.dump_output.clone() else {
                    eprintln!("dump=refused");
                    eprintln!("missing --output <relative.sql>");
                    std::process::exit(2);
                };
                let storage_backend =
                    resolve_storage_backend(cli.storage_backend, cli.ephemeral, &config);
                let sql = format!(
                    "BACKUP DATABASE TO '{}'",
                    escape_backup_sql_path(output.as_str())
                );
                match run_offline_backup_command(data_dir.clone(), config, storage_backend, sql) {
                    Ok(()) => {
                        println!("dump=ok");
                        println!("data_dir={}", data_dir.display());
                        println!("output=backups/{output}");
                        std::process::exit(0);
                    }
                    Err(error) => {
                        eprintln!("dump=refused");
                        eprintln!("{error}");
                        std::process::exit(2);
                    }
                }
            }
            CliCommand::Restore => {
                let Some(input) = cli.restore_input.clone() else {
                    eprintln!("restore=refused");
                    eprintln!("missing --input <relative.sql>");
                    std::process::exit(2);
                };
                let storage_backend =
                    resolve_storage_backend(cli.storage_backend, cli.ephemeral, &config);
                let sql = format!(
                    "RESTORE DATABASE FROM '{}'",
                    escape_backup_sql_path(input.as_str())
                );
                match run_offline_backup_command(data_dir.clone(), config, storage_backend, sql) {
                    Ok(()) => {
                        println!("restore=ok");
                        println!("data_dir={}", data_dir.display());
                        println!("input=backups/{input}");
                        std::process::exit(0);
                    }
                    Err(error) => {
                        eprintln!("restore=refused");
                        eprintln!("{error}");
                        std::process::exit(2);
                    }
                }
            }
            CliCommand::Serve => {}
        }
    }

    init_tracing();
    log_v0_1_product_contract();

    apply_memory_safety_guard(&mut config);
    let storage_backend = resolve_storage_backend(cli.storage_backend, cli.ephemeral, &config);
    apply_server_security_baseline(&mut config, storage_backend);
    let effective_data_dir = resolve_data_dir(cli.data_dir.clone(), &config);
    enforce_storage_encryption_policy(
        storage_backend,
        &effective_data_dir,
        cli.allow_unencrypted_storage,
    )
    .unwrap_or_else(|err| {
        error!(%err, "storage encryption policy check failed");
        std::process::exit(1);
    });
    validate_remote_exposure_security(&config).unwrap_or_else(|err| {
        error!(%err, "remote exposure security validation failed");
        std::process::exit(1);
    });
    validate_experimental_release_gates(&config).unwrap_or_else(|err| {
        error!(%err, "experimental feature gate validation failed");
        std::process::exit(1);
    });

    if cli.ephemeral && config.storage.backend != StorageBackend::InMemory {
        warn!(
            configured_backend = config.storage.backend.as_str(),
            "--ephemeral overrides the configured persistent storage backend"
        );
    }

    if !storage_backend.is_persistent() && cli.data_dir.is_some() {
        warn!(
            "--data-dir is ignored when the in-memory backend is selected \
             data will NOT be persisted)"
        );
    }

    // SECURITY: bench mode swaps in AllowAllAuthorizer for the server session
    // gate. It must be opted into with an explicit env var, never derived from
    // the mere presence of AIONDB_BOOTSTRAP_USER (which is also used for
    // normal operator-driven role provisioning under the standard authorizer).
    let bench_mode = env_bool("AIONDB_BENCH_MODE", false);

    if config.replication.role == ReplicationRole::Replica && storage_backend.is_persistent() {
        if let Err(err) =
            bootstrap_replica_from_primary_if_needed(&effective_data_dir, &config).await
        {
            error!(%err, "replica bootstrap from primary failed");
            std::process::exit(1);
        }
    }

    let engine = build_server_engine(cli.data_dir.clone(), &config, storage_backend, bench_mode)
        .unwrap_or_else(|err| {
            error!(backend = storage_backend.as_str(), %err, "failed to build server engine");
            std::process::exit(1);
        });
    maybe_bootstrap_role_from_cli_or_env(&engine, &cli);
    let fragment_transport_engine = Arc::clone(&engine);
    let replica_runtime_engine = Arc::clone(&engine);
    let ha_runtime_engine = Arc::clone(&engine);

    let (tls, require_tls) = resolve_server_tls(&config).unwrap_or_else(|err| {
        error!(%err, "failed to configure pgwire TLS");
        std::process::exit(1);
    });

    let (bind_address, port) = split_listen_addr(&config.pgwire.listen_addr);
    let pgwire_config = PgWireConfig {
        bind_address,
        port,
        max_connections: config.pgwire.max_connections,
        max_connections_per_ip: config.pgwire.max_connections_per_ip,
        startup_timeout: config.pgwire.startup_timeout,
        shutdown_timeout: Duration::from_secs(30),
        auth_failure_backoff: config.pgwire.auth_failure_backoff,
        engine_pool: config.pgwire.engine_pool.clone(),
        idle_timeout: config.pgwire.idle_timeout,
        max_portals: config.limits.max_portals,
        tls,
        require_tls,
        fail_on_weak_rng: true,
    };

    let server = Arc::new(
        PgWireServer::new(engine, pgwire_config).unwrap_or_else(|err| {
            error!(%err, "failed to initialize pgwire server");
            std::process::exit(1);
        }),
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let fragment_transport_shutdown_rx = shutdown_rx.clone();
    let observability_shutdown_rx = shutdown_rx.clone();
    let replica_runtime_shutdown_rx = shutdown_rx.clone();
    let _fragment_transport_task = init_fragment_transport_server(
        fragment_transport_engine,
        &config,
        fragment_transport_shutdown_rx,
        fragment_transport_fail_fast(),
    )
    .await;

    let replica_runtime = match replica_runtime::init_replica_runtime(
        replica_runtime_engine,
        &config,
        replica_runtime_shutdown_rx,
    ) {
        Ok(handle) => handle,
        Err(err) => {
            error!(%err, "failed to start replica runtime");
            std::process::exit(1);
        }
    };
    let replica_metrics = replica_runtime
        .as_ref()
        .map(|runtime| runtime.metrics.clone());

    let ha_runtime_shutdown_rx = shutdown_rx.clone();
    let _ha_task =
        match ha_runtime::init_ha_runtime(ha_runtime_engine, &config, ha_runtime_shutdown_rx).await
        {
            Ok(handle) => handle,
            Err(err) => {
                error!(%err, "failed to start HA runtime");
                std::process::exit(1);
            }
        };

    let _observability_task = if cli.disable_observability {
        info!("observability HTTP server disabled by CLI flag");
        None
    } else {
        let obs_config = observability_config(&cli);
        init_observability_server(
            Arc::clone(&server),
            replica_metrics,
            obs_config,
            observability_shutdown_rx,
            observability_fail_fast(),
        )
        .await
    };

    // Listen for SIGINT (Ctrl+C) AND SIGTERM (the default `kill`/Docker/k8s
    // stop signal). Without SIGTERM the container runtime nine-tenths of the
    // time bypasses our graceful shutdown, leaving connections half-closed
    // and PG-compat caches unflushed.
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut term = match signal(SignalKind::terminate()) {
                Ok(stream) => stream,
                Err(err) => {
                    error!(%err, "failed to install SIGTERM handler");
                    return;
                }
            };
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    if let Err(err) = result {
                        error!(%err, "failed to listen for Ctrl+C signal");
                        return;
                    }
                    info!("received SIGINT, initiating shutdown");
                }
                _ = term.recv() => {
                    info!("received SIGTERM, initiating shutdown");
                }
            }
        }
        #[cfg(not(unix))]
        {
            if let Err(err) = tokio::signal::ctrl_c().await {
                error!(%err, "failed to listen for Ctrl+C signal");
                return;
            }
            info!("received Ctrl+C, initiating shutdown");
        }
        let _ = shutdown_tx.send(true);
    });

    if let Err(err) = server.start(shutdown_rx).await {
        error!(%err, "server exited with error");
        std::process::exit(1);
    }

    if let Some(replica_runtime) = replica_runtime {
        match tokio::time::timeout(Duration::from_secs(5), replica_runtime.join()).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                warn!(%err, "replica runtime exited with error during shutdown");
            }
            Err(_) => {
                warn!("timed out waiting for replica runtime shutdown");
            }
        }
    }

    info!("server shut down cleanly");
}

#[cfg(test)]
mod tests;
