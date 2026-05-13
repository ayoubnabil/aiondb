//! Engine performance metrics with lock-free atomic counters.
//!
//! Provides cumulative counters for query execution, failures, row counts,
//! and distributed fragment operations. Supports Prometheus text format
//! and JSON export.
//!
//! # Types
//!
//! - [`EngineMetrics`] -- mutable counters (atomic `fetch_add`), held as
//!   `Arc<EngineMetrics>` inside the engine. Thread-safe without locking.
//! - [`EngineMetricsSnapshot`] -- immutable point-in-time copy of all
//!   counters. Cheaply cloneable, implements `Default` and `Eq`.

#![allow(clippy::must_use_candidate, clippy::too_many_lines)]

use std::sync::atomic::{AtomicU64, Ordering};

use super::support::u64_to_f64;

const QUERY_DURATION_BUCKET_UPPER_BOUNDS_MICROS: [u64; 12] = [
    100, 250, 500, 1_000, 2_500, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 1_000_000,
];
const QUERY_DURATION_BUCKET_COUNT: usize = QUERY_DURATION_BUCKET_UPPER_BOUNDS_MICROS.len() + 1;

fn ceil_clamped_f64_to_u64(value: f64) -> u64 {
    if !value.is_finite() {
        return u64::MAX;
    }
    if value <= 0.0 {
        return 0;
    }
    let ceiling = value.ceil();
    let max_u64 = 18_446_744_073_709_551_615.0;
    if ceiling >= max_u64 {
        u64::MAX
    } else {
        let rendered = format!("{ceiling:.0}");
        rendered.parse::<u64>().unwrap_or(u64::MAX)
    }
}

/// Engine-level query execution metrics (lock-free via atomics).
pub struct EngineMetrics {
    pub(crate) queries_total: AtomicU64,
    pub(crate) queries_failed: AtomicU64,
    pub(crate) rows_returned_total: AtomicU64,
    pub(crate) rows_affected_total: AtomicU64,
    pub(crate) query_duration_micros_total: AtomicU64,
    query_duration_histogram_buckets: Box<[AtomicU64]>,
    inflight_queries: AtomicU64,
    inflight_queries_peak: AtomicU64,
    session_lock_wait_micros_total: AtomicU64,
    session_lock_wait_count: AtomicU64,
    session_lock_wait_micros_max: AtomicU64,
    /// Total number of graph DDL operations (CREATE/DROP NODE/EDGE LABEL).
    pub(crate) graph_ddl_operations: AtomicU64,
    /// Total number of distributed fragments executed.
    pub(crate) distributed_fragments_total: AtomicU64,
    /// Total number of distributed fragment errors.
    pub(crate) distributed_fragment_errors: AtomicU64,
}

pub(crate) struct InflightQueryGuard<'a> {
    metrics: &'a EngineMetrics,
}

impl Drop for InflightQueryGuard<'_> {
    fn drop(&mut self) {
        self.metrics
            .inflight_queries
            .fetch_sub(1, Ordering::Relaxed);
    }
}

/// Point-in-time snapshot of engine metrics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EngineMetricsSnapshot {
    pub queries_total: u64,
    pub queries_failed: u64,
    pub rows_returned_total: u64,
    pub rows_affected_total: u64,
    pub query_duration_micros_total: u64,
    pub query_duration_histogram_buckets: Vec<u64>,
    pub query_duration_p50_micros: u64,
    pub query_duration_p95_micros: u64,
    pub query_duration_p99_micros: u64,
    pub query_queue_depth_current: u64,
    pub query_queue_depth_peak: u64,
    pub session_lock_wait_total: u64,
    pub session_lock_wait_micros_total: u64,
    pub session_lock_wait_micros_max: u64,
    /// Total number of graph DDL operations (CREATE/DROP NODE/EDGE LABEL).
    pub graph_ddl_operations: u64,
    /// Total number of distributed fragments executed.
    pub distributed_fragments_total: u64,
    /// Total number of distributed fragment errors.
    pub distributed_fragment_errors: u64,
}

impl EngineMetrics {
    /// Create a new zeroed metrics instance.
    pub fn new() -> Self {
        Self {
            queries_total: AtomicU64::new(0),
            queries_failed: AtomicU64::new(0),
            rows_returned_total: AtomicU64::new(0),
            rows_affected_total: AtomicU64::new(0),
            query_duration_micros_total: AtomicU64::new(0),
            query_duration_histogram_buckets: (0..QUERY_DURATION_BUCKET_COUNT)
                .map(|_| AtomicU64::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            inflight_queries: AtomicU64::new(0),
            inflight_queries_peak: AtomicU64::new(0),
            session_lock_wait_micros_total: AtomicU64::new(0),
            session_lock_wait_count: AtomicU64::new(0),
            session_lock_wait_micros_max: AtomicU64::new(0),
            graph_ddl_operations: AtomicU64::new(0),
            distributed_fragments_total: AtomicU64::new(0),
            distributed_fragment_errors: AtomicU64::new(0),
        }
    }

    /// Record a successful query execution.
    pub fn record_query(&self, duration_micros: u64, rows_returned: u64, rows_affected: u64) {
        self.queries_total.fetch_add(1, Ordering::Relaxed);
        self.rows_returned_total
            .fetch_add(rows_returned, Ordering::Relaxed);
        self.rows_affected_total
            .fetch_add(rows_affected, Ordering::Relaxed);
        self.query_duration_micros_total
            .fetch_add(duration_micros, Ordering::Relaxed);
        let bucket_index = query_duration_bucket_index(duration_micros);
        if let Some(bucket) = self.query_duration_histogram_buckets.get(bucket_index) {
            bucket.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record a failed query execution.
    pub fn record_failure(&self) {
        self.queries_total.fetch_add(1, Ordering::Relaxed);
        self.queries_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn track_inflight_query(&self) -> InflightQueryGuard<'_> {
        let current = self
            .inflight_queries
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        update_max(&self.inflight_queries_peak, current);
        InflightQueryGuard { metrics: self }
    }

    pub(crate) fn record_session_lock_wait(&self, waited_micros: u64) {
        self.session_lock_wait_count.fetch_add(1, Ordering::Relaxed);
        self.session_lock_wait_micros_total
            .fetch_add(waited_micros, Ordering::Relaxed);
        update_max(&self.session_lock_wait_micros_max, waited_micros);
    }

    /// Record a successful graph DDL operation (CREATE/DROP NODE/EDGE LABEL).
    pub fn record_graph_ddl(&self) {
        self.graph_ddl_operations.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a distributed fragment execution.
    pub fn record_distributed_fragment(&self) {
        self.distributed_fragments_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record a distributed fragment error.
    pub fn record_distributed_fragment_error(&self) {
        self.distributed_fragment_errors
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Take a point-in-time snapshot of all counters.
    pub fn snapshot(&self) -> EngineMetricsSnapshot {
        let query_duration_histogram_buckets = self
            .query_duration_histogram_buckets
            .iter()
            .map(|bucket| bucket.load(Ordering::Relaxed))
            .collect::<Vec<_>>();

        let query_duration_p50_micros =
            approximate_histogram_quantile_micros(&query_duration_histogram_buckets, 0.50);
        let query_duration_p95_micros =
            approximate_histogram_quantile_micros(&query_duration_histogram_buckets, 0.95);
        let query_duration_p99_micros =
            approximate_histogram_quantile_micros(&query_duration_histogram_buckets, 0.99);

        EngineMetricsSnapshot {
            queries_total: self.queries_total.load(Ordering::Relaxed),
            queries_failed: self.queries_failed.load(Ordering::Relaxed),
            rows_returned_total: self.rows_returned_total.load(Ordering::Relaxed),
            rows_affected_total: self.rows_affected_total.load(Ordering::Relaxed),
            query_duration_micros_total: self.query_duration_micros_total.load(Ordering::Relaxed),
            query_duration_histogram_buckets,
            query_duration_p50_micros,
            query_duration_p95_micros,
            query_duration_p99_micros,
            query_queue_depth_current: self.inflight_queries.load(Ordering::Relaxed),
            query_queue_depth_peak: self.inflight_queries_peak.load(Ordering::Relaxed),
            session_lock_wait_total: self.session_lock_wait_count.load(Ordering::Relaxed),
            session_lock_wait_micros_total: self
                .session_lock_wait_micros_total
                .load(Ordering::Relaxed),
            session_lock_wait_micros_max: self.session_lock_wait_micros_max.load(Ordering::Relaxed),
            graph_ddl_operations: self.graph_ddl_operations.load(Ordering::Relaxed),
            distributed_fragments_total: self.distributed_fragments_total.load(Ordering::Relaxed),
            distributed_fragment_errors: self.distributed_fragment_errors.load(Ordering::Relaxed),
        }
    }
}

impl Default for EngineMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineMetricsSnapshot {
    /// Format as Prometheus exposition text.
    pub fn to_prometheus_text(&self) -> String {
        // Stream every metric line into one growing buffer via
        // `write!(out, ...)` instead of `out.push_str(&format!(...))`,
        // saves ~20 transient String allocations per /metrics scrape.
        use std::fmt::Write;
        let mut out = String::new();

        out.push_str("# HELP aiondb_queries_total Total number of queries executed.\n");
        out.push_str("# TYPE aiondb_queries_total counter\n");
        let _ = writeln!(out, "aiondb_queries_total {}", self.queries_total);

        out.push_str("# HELP aiondb_queries_failed_total Total number of failed queries.\n");
        out.push_str("# TYPE aiondb_queries_failed_total counter\n");
        let _ = writeln!(out, "aiondb_queries_failed_total {}", self.queries_failed);

        out.push_str(
            "# HELP aiondb_rows_returned_total Total number of rows returned by queries.\n",
        );
        out.push_str("# TYPE aiondb_rows_returned_total counter\n");
        let _ = writeln!(
            out,
            "aiondb_rows_returned_total {}",
            self.rows_returned_total
        );

        out.push_str(
            "# HELP aiondb_rows_affected_total Total number of rows affected by DML statements.\n",
        );
        out.push_str("# TYPE aiondb_rows_affected_total counter\n");
        let _ = writeln!(
            out,
            "aiondb_rows_affected_total {}",
            self.rows_affected_total
        );

        out.push_str(
            "# HELP aiondb_query_duration_micros_total Cumulative query execution time in microseconds.\n",
        );
        out.push_str("# TYPE aiondb_query_duration_micros_total counter\n");
        let _ = writeln!(
            out,
            "aiondb_query_duration_micros_total {}",
            self.query_duration_micros_total
        );

        out.push_str("# HELP aiondb_query_duration_micros Query execution latency histogram in microseconds.\n");
        out.push_str("# TYPE aiondb_query_duration_micros histogram\n");
        let mut cumulative = 0u64;
        for (index, upper_bound) in QUERY_DURATION_BUCKET_UPPER_BOUNDS_MICROS.iter().enumerate() {
            cumulative = cumulative.saturating_add(
                self.query_duration_histogram_buckets
                    .get(index)
                    .copied()
                    .unwrap_or(0),
            );
            let _ = writeln!(
                out,
                "aiondb_query_duration_micros_bucket{{le=\"{upper_bound}\"}} {cumulative}"
            );
        }
        cumulative = cumulative.saturating_add(
            self.query_duration_histogram_buckets
                .last()
                .copied()
                .unwrap_or(0),
        );
        let _ = writeln!(
            out,
            "aiondb_query_duration_micros_bucket{{le=\"+Inf\"}} {cumulative}"
        );
        let _ = writeln!(
            out,
            "aiondb_query_duration_micros_sum {}",
            self.query_duration_micros_total
        );
        let _ = writeln!(out, "aiondb_query_duration_micros_count {cumulative}");

        out.push_str(
            "# HELP aiondb_query_duration_micros_p50 Approximate p50 query latency in microseconds.\n",
        );
        out.push_str("# TYPE aiondb_query_duration_micros_p50 gauge\n");
        let _ = writeln!(
            out,
            "aiondb_query_duration_micros_p50 {}",
            self.query_duration_p50_micros
        );

        out.push_str(
            "# HELP aiondb_query_duration_micros_p95 Approximate p95 query latency in microseconds.\n",
        );
        out.push_str("# TYPE aiondb_query_duration_micros_p95 gauge\n");
        let _ = writeln!(
            out,
            "aiondb_query_duration_micros_p95 {}",
            self.query_duration_p95_micros
        );

        out.push_str(
            "# HELP aiondb_query_duration_micros_p99 Approximate p99 query latency in microseconds.\n",
        );
        out.push_str("# TYPE aiondb_query_duration_micros_p99 gauge\n");
        let _ = writeln!(
            out,
            "aiondb_query_duration_micros_p99 {}",
            self.query_duration_p99_micros
        );

        out.push_str(
            "# HELP aiondb_query_queue_depth_current Current concurrent queries in progress.\n",
        );
        out.push_str("# TYPE aiondb_query_queue_depth_current gauge\n");
        let _ = writeln!(
            out,
            "aiondb_query_queue_depth_current {}",
            self.query_queue_depth_current
        );

        out.push_str(
            "# HELP aiondb_query_queue_depth_peak Peak concurrent queries in progress since startup.\n",
        );
        out.push_str("# TYPE aiondb_query_queue_depth_peak gauge\n");
        let _ = writeln!(
            out,
            "aiondb_query_queue_depth_peak {}",
            self.query_queue_depth_peak
        );

        out.push_str(
            "# HELP aiondb_session_lock_wait_total Total session lock acquisition attempts measured.\n",
        );
        out.push_str("# TYPE aiondb_session_lock_wait_total counter\n");
        let _ = writeln!(
            out,
            "aiondb_session_lock_wait_total {}",
            self.session_lock_wait_total
        );

        out.push_str("# HELP aiondb_session_lock_wait_micros_total Total time spent waiting on session locks in microseconds.\n");
        out.push_str("# TYPE aiondb_session_lock_wait_micros_total counter\n");
        let _ = writeln!(
            out,
            "aiondb_session_lock_wait_micros_total {}",
            self.session_lock_wait_micros_total
        );

        out.push_str(
            "# HELP aiondb_session_lock_wait_micros_max Maximum observed session lock wait in microseconds.\n",
        );
        out.push_str("# TYPE aiondb_session_lock_wait_micros_max gauge\n");
        let _ = writeln!(
            out,
            "aiondb_session_lock_wait_micros_max {}",
            self.session_lock_wait_micros_max
        );

        out.push_str(
            "# HELP aiondb_graph_ddl_operations_total Total graph DDL operations (CREATE/DROP label).\n",
        );
        out.push_str("# TYPE aiondb_graph_ddl_operations_total counter\n");
        let _ = writeln!(
            out,
            "aiondb_graph_ddl_operations_total {}",
            self.graph_ddl_operations
        );

        out.push_str(
            "# HELP aiondb_distributed_fragments_total Total distributed fragments executed.\n",
        );
        out.push_str("# TYPE aiondb_distributed_fragments_total counter\n");
        let _ = writeln!(
            out,
            "aiondb_distributed_fragments_total {}",
            self.distributed_fragments_total
        );

        out.push_str(
            "# HELP aiondb_distributed_fragment_errors_total Total distributed fragment errors.\n",
        );
        out.push_str("# TYPE aiondb_distributed_fragment_errors_total counter\n");
        let _ = writeln!(
            out,
            "aiondb_distributed_fragment_errors_total {}",
            self.distributed_fragment_errors
        );

        out
    }

    /// Format as a JSON string.
    pub fn to_json_string(&self) -> String {
        serde_json::json!({
            "queries_total": self.queries_total,
            "queries_failed": self.queries_failed,
            "rows_returned_total": self.rows_returned_total,
            "rows_affected_total": self.rows_affected_total,
            "query_duration_micros_total": self.query_duration_micros_total,
            "query_duration_histogram_buckets": self.query_duration_histogram_buckets,
            "query_duration_p50_micros": self.query_duration_p50_micros,
            "query_duration_p95_micros": self.query_duration_p95_micros,
            "query_duration_p99_micros": self.query_duration_p99_micros,
            "query_queue_depth_current": self.query_queue_depth_current,
            "query_queue_depth_peak": self.query_queue_depth_peak,
            "session_lock_wait_total": self.session_lock_wait_total,
            "session_lock_wait_micros_total": self.session_lock_wait_micros_total,
            "session_lock_wait_micros_max": self.session_lock_wait_micros_max,
            "graph_ddl_operations": self.graph_ddl_operations,
            "distributed_fragments_total": self.distributed_fragments_total,
            "distributed_fragment_errors": self.distributed_fragment_errors,
        })
        .to_string()
    }
}

fn query_duration_bucket_index(duration_micros: u64) -> usize {
    QUERY_DURATION_BUCKET_UPPER_BOUNDS_MICROS
        .iter()
        .position(|upper_bound| duration_micros <= *upper_bound)
        .unwrap_or(QUERY_DURATION_BUCKET_UPPER_BOUNDS_MICROS.len())
}

fn approximate_histogram_quantile_micros(buckets: &[u64], quantile: f64) -> u64 {
    let clamped_quantile = quantile.clamp(0.0, 1.0);
    let total = buckets.iter().copied().sum::<u64>();
    if total == 0 {
        return 0;
    }

    let target = ceil_clamped_f64_to_u64(u64_to_f64(total) * clamped_quantile);
    let target = target.max(1);

    let mut cumulative = 0u64;
    for (index, count) in buckets.iter().copied().enumerate() {
        cumulative = cumulative.saturating_add(count);
        if cumulative >= target {
            return if index < QUERY_DURATION_BUCKET_UPPER_BOUNDS_MICROS.len() {
                QUERY_DURATION_BUCKET_UPPER_BOUNDS_MICROS[index]
            } else {
                QUERY_DURATION_BUCKET_UPPER_BOUNDS_MICROS
                    .last()
                    .copied()
                    .unwrap_or(0)
            };
        }
    }

    QUERY_DURATION_BUCKET_UPPER_BOUNDS_MICROS
        .last()
        .copied()
        .unwrap_or(0)
}

fn update_max(target: &AtomicU64, candidate: u64) {
    let mut current = target.load(Ordering::Relaxed);
    while candidate > current {
        match target.compare_exchange_weak(current, candidate, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}
