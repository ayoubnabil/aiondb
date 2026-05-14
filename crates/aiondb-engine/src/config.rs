use std::sync::OnceLock;

use aiondb_config::{LimitsConfig, RuntimeConfig, SecurityConfig};

use crate::SessionLimits;

pub const MAX_IDENTIFIER_LENGTH: usize = 1024;
pub const MAX_SQL_LENGTH: usize = 16 * 1024 * 1024; // 16 MiB

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;
const MEMORY_GUARD_FALLBACK_HOST_BYTES: u64 = 8 * GIB;
const MEMORY_GUARD_MIN_DB_BUDGET_BYTES: u64 = 128 * MIB;
const MEMORY_GUARD_MIN_PER_QUERY_MEMORY_BYTES: u64 = 16 * MIB;
const MEMORY_GUARD_MAX_PER_QUERY_MEMORY_BYTES: u64 = 512 * MIB;
const MEMORY_GUARD_MIN_PER_QUERY_TEMP_BYTES: u64 = 32 * MIB;
const MEMORY_GUARD_MAX_PER_QUERY_TEMP_BYTES: u64 = GIB;
const MEMORY_GUARD_MIN_RESULT_BYTES: u64 = 512 * KIB;
const MEMORY_GUARD_MAX_RESULT_BYTES: u64 = 64 * MIB;
// Analytical queries can legitimately need >150k intermediate rows.
// Keep a hard cap, but raise it to avoid premature executor aborts.
const MEMORY_GUARD_MAX_RESULT_ROWS: u64 = 2_000_000;

#[derive(Clone, Copy, Debug)]
#[allow(clippy::struct_field_names)]
struct ExecutionLimitCaps {
    max_result_rows: u64,
    max_result_bytes: u64,
    max_memory_bytes: u64,
    max_temp_bytes: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EngineConfig {
    pub default_limits: SessionLimits,
    pub require_password: bool,
    pub security: SecurityConfig,
}

impl From<&RuntimeConfig> for EngineConfig {
    fn from(value: &RuntimeConfig) -> Self {
        Self {
            default_limits: session_limits_from_config(&value.limits),
            require_password: !value.security.allow_anonymous_local,
            security: value.security.clone(),
        }
    }
}

#[must_use]
pub fn session_limits_from_config(limits: &LimitsConfig) -> SessionLimits {
    let (max_result_rows, max_result_bytes, max_memory_bytes, max_temp_bytes) =
        guard_execution_limits(
            limits.max_result_rows,
            limits.max_result_bytes,
            limits.max_memory_bytes,
            limits.max_temp_bytes,
        );

    SessionLimits {
        statement_timeout: limits.statement_timeout,
        lock_timeout: limits.lock_timeout,
        max_result_rows,
        max_result_bytes,
        max_memory_bytes,
        max_temp_bytes,
        max_parallel_workers_per_query: limits.max_parallel_workers_per_query,
        max_portals: limits.max_portals,
        max_prepared_statements: limits.max_prepared_statements,
        max_recursive_iterations: limits.max_recursive_iterations,
        max_recursive_rows: limits.max_recursive_rows,
    }
}

#[must_use]
pub(crate) fn guard_execution_limits(
    max_result_rows: u64,
    max_result_bytes: u64,
    max_memory_bytes: u64,
    max_temp_bytes: u64,
) -> (u64, u64, u64, u64) {
    let caps = execution_limit_caps();
    let guarded_memory = max_memory_bytes.clamp(1, caps.max_memory_bytes);
    let temp_upper = caps.max_temp_bytes.max(1);
    let guarded_temp = max_temp_bytes.clamp(1, temp_upper);
    let guarded_result_rows = max_result_rows.clamp(1, caps.max_result_rows);
    let result_bytes_upper = caps.max_result_bytes.min(guarded_memory).max(1);
    let guarded_result_bytes = max_result_bytes.clamp(1, result_bytes_upper);

    (
        guarded_result_rows,
        guarded_result_bytes,
        guarded_memory,
        guarded_temp,
    )
}

fn execution_limit_caps() -> ExecutionLimitCaps {
    static CACHED_CAPS: OnceLock<ExecutionLimitCaps> = OnceLock::new();
    *CACHED_CAPS.get_or_init(|| {
        let host_memory_bytes =
            detect_host_memory_bytes().unwrap_or(MEMORY_GUARD_FALLBACK_HOST_BYTES);
        let os_reserve_bytes = (host_memory_bytes / 3).max(512 * MIB);
        let db_budget_bytes = host_memory_bytes
            .saturating_sub(os_reserve_bytes)
            .max(MEMORY_GUARD_MIN_DB_BUDGET_BYTES);
        let per_query_memory_cap = (db_budget_bytes / 8).clamp(
            MEMORY_GUARD_MIN_PER_QUERY_MEMORY_BYTES,
            MEMORY_GUARD_MAX_PER_QUERY_MEMORY_BYTES,
        );
        let per_query_temp_cap = (db_budget_bytes / 4).clamp(
            MEMORY_GUARD_MIN_PER_QUERY_TEMP_BYTES,
            MEMORY_GUARD_MAX_PER_QUERY_TEMP_BYTES,
        );
        let result_bytes_cap = (per_query_memory_cap / 2)
            .clamp(MEMORY_GUARD_MIN_RESULT_BYTES, MEMORY_GUARD_MAX_RESULT_BYTES);
        ExecutionLimitCaps {
            max_result_rows: MEMORY_GUARD_MAX_RESULT_ROWS,
            max_result_bytes: result_bytes_cap,
            max_memory_bytes: per_query_memory_cap,
            max_temp_bytes: per_query_temp_cap.max(per_query_memory_cap),
        }
    })
}

fn detect_host_memory_bytes() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    meminfo.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some("MemTotal:"), Some(value_kib)) => value_kib
                .parse::<u64>()
                .ok()
                .map(|kib| kib.saturating_mul(KIB)),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_execution_limits_caps_unbounded_inputs() {
        let (rows, result_bytes, memory_bytes, temp_bytes) =
            guard_execution_limits(u64::MAX, u64::MAX, u64::MAX, u64::MAX);

        assert!(rows <= MEMORY_GUARD_MAX_RESULT_ROWS);
        assert!(result_bytes <= MEMORY_GUARD_MAX_RESULT_BYTES);
        assert!(memory_bytes <= MEMORY_GUARD_MAX_PER_QUERY_MEMORY_BYTES);
        assert!(temp_bytes <= MEMORY_GUARD_MAX_PER_QUERY_TEMP_BYTES);
        assert!(result_bytes <= memory_bytes);
        assert!(temp_bytes >= memory_bytes);
    }

    #[test]
    fn guard_execution_limits_preserves_small_safe_values() {
        let requested_rows = 1_000;
        let requested_result_bytes = 128 * KIB;
        let requested_memory_bytes = 4 * MIB;
        let requested_temp_bytes = 8 * MIB;

        let (rows, result_bytes, memory_bytes, temp_bytes) = guard_execution_limits(
            requested_rows,
            requested_result_bytes,
            requested_memory_bytes,
            requested_temp_bytes,
        );

        assert_eq!(rows, requested_rows);
        assert_eq!(result_bytes, requested_result_bytes);
        assert_eq!(memory_bytes, requested_memory_bytes);
        assert_eq!(temp_bytes, requested_temp_bytes);
    }

    #[test]
    fn session_limits_from_config_applies_execution_guard() {
        let limits = LimitsConfig {
            statement_timeout: std::time::Duration::from_secs(5),
            lock_timeout: std::time::Duration::from_secs(2),
            max_result_rows: u64::MAX,
            max_result_bytes: u64::MAX,
            max_memory_bytes: u64::MAX,
            max_temp_bytes: u64::MAX,
            max_parallel_workers_per_query: 4,
            max_portals: 8,
            max_prepared_statements: 16,
            max_recursive_iterations: 32,
            max_recursive_rows: 64,
        };

        let session_limits = session_limits_from_config(&limits);

        assert!(session_limits.max_result_rows <= MEMORY_GUARD_MAX_RESULT_ROWS);
        assert!(session_limits.max_result_bytes <= MEMORY_GUARD_MAX_RESULT_BYTES);
        assert!(session_limits.max_memory_bytes <= MEMORY_GUARD_MAX_PER_QUERY_MEMORY_BYTES);
        assert!(session_limits.max_temp_bytes <= MEMORY_GUARD_MAX_PER_QUERY_TEMP_BYTES);
        assert!(session_limits.max_result_bytes <= session_limits.max_memory_bytes);
        assert!(session_limits.max_temp_bytes >= session_limits.max_memory_bytes);
        assert_eq!(session_limits.max_parallel_workers_per_query, 4);
        assert_eq!(session_limits.max_portals, 8);
        assert_eq!(session_limits.max_prepared_statements, 16);
        assert_eq!(session_limits.max_recursive_iterations, 32);
        assert_eq!(session_limits.max_recursive_rows, 64);
    }
}
