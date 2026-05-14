//! Metrics by `CompatCommand`.
//!
//! Lightweight instrumentation per compat command variant:
//! - call counter,
//! - parse / bind / execute histogram (durations in microseconds),
//! - "fallback" counter (hook returns `Some(result)` - actual compat-layer
//!   handling vs falling through to the normal planner).
//!
//! The engine (`aiondb-engine`) instantiates a `CompatCommandMetrics` and
//! exposes it via `engine_metrics` (Prometheus / JSON). This crate does not
//! depend on Prometheus in order to stay lightweight; the snapshot is
//! manually serializable.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use crate::command::CompatCommand;

/// Histogram snapshot: fixed buckets in microseconds.
///
/// Buckets inspired by typical latencies: `<10µs, <100µs, <1ms, <10ms,
/// <100ms, <1s, >=1s`. A small number of buckets for reasonable Prometheus
/// cardinality.
pub const LATENCY_BUCKETS_US: &[u64] = &[10, 100, 1_000, 10_000, 100_000, 1_000_000];

#[derive(Debug, Default)]
pub struct Histogram {
    /// `counts[i]` = samples ≤ `LATENCY_BUCKETS_US[i]`.
    /// `counts[N]` (where N = len) = samples > the last bucket.
    counts: [AtomicU64; 7],
    total_count: AtomicU64,
    total_sum_us: AtomicU64,
}

impl Histogram {
    pub fn observe(&self, duration_us: u64) {
        self.total_count.fetch_add(1, Ordering::Relaxed);
        self.total_sum_us.fetch_add(duration_us, Ordering::Relaxed);
        for (i, &limit) in LATENCY_BUCKETS_US.iter().enumerate() {
            if duration_us <= limit {
                self.counts[i].fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        // overflow bucket
        self.counts[LATENCY_BUCKETS_US.len()].fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> HistogramSnapshot {
        let mut bucket_counts = [0u64; 7];
        for (slot, count) in bucket_counts.iter_mut().zip(self.counts.iter()) {
            *slot = count.load(Ordering::Relaxed);
        }
        HistogramSnapshot {
            buckets_us: LATENCY_BUCKETS_US,
            bucket_counts,
            total_count: self.total_count.load(Ordering::Relaxed),
            total_sum_us: self.total_sum_us.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HistogramSnapshot {
    pub buckets_us: &'static [u64],
    pub bucket_counts: [u64; 7],
    pub total_count: u64,
    pub total_sum_us: u64,
}

impl HistogramSnapshot {
    /// Moyenne en microsecondes.
    pub fn mean_us(&self) -> f64 {
        if self.total_count == 0 {
            0.0
        } else {
            u64_to_f64(self.total_sum_us) / u64_to_f64(self.total_count)
        }
    }
}

fn u64_to_f64(value: u64) -> f64 {
    let upper = u32::try_from(value >> 32).unwrap_or(u32::MAX);
    let lower = u32::try_from(value & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(upper) * 4_294_967_296.0 + f64::from(lower)
}

#[derive(Debug, Default)]
struct PerCommandEntry {
    calls: AtomicU64,
    fallbacks: AtomicU64,
    parse_hist: Histogram,
    bind_hist: Histogram,
    execute_hist: Histogram,
}

/// Central registry for compat metrics.
///
/// Thread-safe sans lock sur le chemin chaud (atomics par compteur). Le
/// The `Mutex` is only taken when inserting a new variant - which happens
/// only at startup and then never again (the `CompatCommand` variants are
/// finite).
#[derive(Debug, Default)]
pub struct CompatCommandMetrics {
    entries: Mutex<BTreeMap<CompatCommand, &'static PerCommandEntry>>,
}

impl CompatCommandMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_entries(&self) -> MutexGuard<'_, BTreeMap<CompatCommand, &'static PerCommandEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn entry(&self, command: CompatCommand) -> &'static PerCommandEntry {
        let mut guard = self.lock_entries();
        if let Some(&entry) = guard.get(&command) {
            return entry;
        }
        let entry: &'static PerCommandEntry = Box::leak(Box::new(PerCommandEntry::default()));
        guard.insert(command, entry);
        entry
    }

    pub fn record_call(&self, command: CompatCommand) {
        self.entry(command).calls.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_fallback(&self, command: CompatCommand) {
        self.entry(command)
            .fallbacks
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn observe_parse(&self, command: CompatCommand, duration_us: u64) {
        self.entry(command).parse_hist.observe(duration_us);
    }

    pub fn observe_bind(&self, command: CompatCommand, duration_us: u64) {
        self.entry(command).bind_hist.observe(duration_us);
    }

    pub fn observe_execute(&self, command: CompatCommand, duration_us: u64) {
        self.entry(command).execute_hist.observe(duration_us);
    }

    pub fn snapshot(&self) -> Vec<CompatCommandSnapshot> {
        let guard = self.lock_entries();
        guard
            .iter()
            .map(|(command, entry)| CompatCommandSnapshot {
                command: *command,
                calls: entry.calls.load(Ordering::Relaxed),
                fallbacks: entry.fallbacks.load(Ordering::Relaxed),
                parse: entry.parse_hist.snapshot(),
                bind: entry.bind_hist.snapshot(),
                execute: entry.execute_hist.snapshot(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CompatCommandSnapshot {
    pub command: CompatCommand,
    pub calls: u64,
    pub fallbacks: u64,
    pub parse: HistogramSnapshot,
    pub bind: HistogramSnapshot,
    pub execute: HistogramSnapshot,
}

/// Serializes snapshots in Prometheus text format (minimal exposition,
/// no complex labels).
pub fn render_prometheus(snapshots: &[CompatCommandSnapshot]) -> String {
    // Stream every per-snapshot line into the output via writeln!
    // instead of `push_str(&format!(...))`. Saves up to 4 transient
    // String allocs per snapshot per scrape.
    use std::fmt::Write;
    let mut out = String::new();
    out.push_str("# HELP aiondb_compat_calls_total Calls to compat commands by tag.\n");
    out.push_str("# TYPE aiondb_compat_calls_total counter\n");
    for s in snapshots {
        let _ = writeln!(
            out,
            "aiondb_compat_calls_total{{command=\"{}\"}} {}",
            s.command.as_tag(),
            s.calls
        );
    }
    out.push_str("# HELP aiondb_compat_fallback_total Fallback count (hook returned Some).\n");
    out.push_str("# TYPE aiondb_compat_fallback_total counter\n");
    for s in snapshots {
        let _ = writeln!(
            out,
            "aiondb_compat_fallback_total{{command=\"{}\"}} {}",
            s.command.as_tag(),
            s.fallbacks
        );
    }
    out.push_str("# HELP aiondb_compat_execute_us Execute latency (microseconds).\n");
    out.push_str("# TYPE aiondb_compat_execute_us summary\n");
    for s in snapshots {
        let _ = writeln!(
            out,
            "aiondb_compat_execute_us_count{{command=\"{}\"}} {}",
            s.command.as_tag(),
            s.execute.total_count
        );
        let _ = writeln!(
            out,
            "aiondb_compat_execute_us_sum{{command=\"{}\"}} {}",
            s.command.as_tag(),
            s.execute.total_sum_us
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_baseline_snapshot_is_empty() {
        let metrics = CompatCommandMetrics::new();
        assert!(metrics.snapshot().is_empty());
    }

    #[test]
    fn histogram_overflow_bucket() {
        let h = Histogram::default();
        // 2 seconds → overflow
        h.observe(2_000_000);
        let snap = h.snapshot();
        assert_eq!(snap.bucket_counts[LATENCY_BUCKETS_US.len()], 1);
        assert_eq!(snap.total_count, 1);
    }

    #[test]
    fn prometheus_render_contains_metric_headers() {
        let output = render_prometheus(&[]);
        assert!(output.contains("aiondb_compat_calls_total"));
        assert!(output.contains("aiondb_compat_execute_us"));
    }
}
