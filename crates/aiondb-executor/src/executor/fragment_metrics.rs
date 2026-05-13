//! Per-fragment execution metrics for distributed query observability.
//!
//! Tracks timing, row counts, and byte counts for each fragment in a
//! distributed execution. Used by EXPLAIN ANALYZE to show per-node
//! performance breakdown.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use super::FragmentTarget;

/// Execution statistics for a single fragment.
#[derive(Clone, Debug)]
pub struct FragmentStats {
    pub fragment_index: usize,
    pub target: FragmentTarget,
    pub rows_returned: u64,
    pub execution_time: Duration,
    pub is_remote: bool,
}

/// Collects metrics from distributed fragment execution.
#[derive(Clone, Debug, Default)]
pub struct FragmentStatsCollector {
    stats: Arc<Mutex<Vec<FragmentStats>>>,
}

impl FragmentStatsCollector {
    /// Create a new empty collector.
    #[must_use]
    pub fn new() -> Self {
        Self {
            stats: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Record a single fragment's execution statistics.
    pub fn record(&self, stat: FragmentStats) {
        let mut guard = self.stats_guard();
        guard.push(stat);
    }

    /// Drain and return all recorded stats, sorted by `fragment_index`.
    pub fn take(&self) -> Vec<FragmentStats> {
        let mut guard = self.stats_guard();
        let mut collected: Vec<FragmentStats> = guard.drain(..).collect();
        collected.sort_by_key(|s| s.fragment_index);
        collected
    }

    /// Sum of all fragment execution times.
    #[must_use]
    pub fn total_execution_time(&self) -> Duration {
        let guard = self.stats_guard();
        guard.iter().map(|s| s.execution_time).sum()
    }

    /// Sum of all rows returned across fragments.
    #[must_use]
    pub fn total_rows(&self) -> u64 {
        let guard = self.stats_guard();
        guard.iter().map(|s| s.rows_returned).sum()
    }

    /// Number of fragments recorded so far.
    #[must_use]
    pub fn fragment_count(&self) -> usize {
        let guard = self.stats_guard();
        guard.len()
    }

    /// Return the fragment with the highest execution time, if any.
    #[must_use]
    pub fn slowest_fragment(&self) -> Option<FragmentStats> {
        let guard = self.stats_guard();
        guard.iter().max_by_key(|s| s.execution_time).cloned()
    }

    /// Produce a human-readable summary of all recorded fragment stats.
    #[must_use]
    pub fn format_summary(&self) -> String {
        let guard = self.stats_guard();
        let mut sorted: Vec<&FragmentStats> = guard.iter().collect();
        sorted.sort_by_key(|s| s.fragment_index);

        let total_rows: u64 = sorted.iter().map(|s| s.rows_returned).sum();
        let total_time: Duration = sorted.iter().map(|s| s.execution_time).sum();
        let total_ms = duration_to_ms(total_time);

        let mut out = format!(
            "Distributed Execution: {} fragments, {total_rows} rows total, {total_ms}ms total",
            sorted.len(),
        );

        // Stream each per-fragment line directly into `out` instead of
        // allocating a transient `format!` String per fragment.
        use std::fmt::Write;
        for stat in &sorted {
            let target_label = format_target_label(&stat.target);
            let frag_ms = duration_to_ms(stat.execution_time);
            let _ = write!(
                out,
                "\n  Fragment {} ({target_label}): {} rows, {frag_ms}ms",
                stat.fragment_index, stat.rows_returned,
            );
        }

        out
    }

    fn stats_guard(&self) -> MutexGuard<'_, Vec<FragmentStats>> {
        self.stats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn format_target_label(target: &FragmentTarget) -> String {
    match target {
        FragmentTarget::Local => "local".to_owned(),
        FragmentTarget::Remote(node_id) => format!("remote({node_id})"),
    }
}

fn duration_to_ms(d: Duration) -> f64 {
    // Truncate to one decimal place for readability.
    let raw = d.as_secs_f64() * 1000.0;
    (raw * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_stat(index: usize, target: FragmentTarget, rows: u64, ms: u64) -> FragmentStats {
        let is_remote = matches!(target, FragmentTarget::Remote(_));
        FragmentStats {
            fragment_index: index,
            target,
            rows_returned: rows,
            execution_time: Duration::from_millis(ms),
            is_remote,
        }
    }

    #[test]
    fn test_new_collector_is_empty() {
        let c = FragmentStatsCollector::new();
        assert_eq!(c.fragment_count(), 0);
        assert_eq!(c.total_rows(), 0);
        assert_eq!(c.total_execution_time(), Duration::ZERO);
        assert!(c.slowest_fragment().is_none());
    }

    #[test]
    fn test_record_and_take_sorts_by_index() {
        let c = FragmentStatsCollector::new();
        c.record(make_stat(2, FragmentTarget::Remote("n2".into()), 30, 10));
        c.record(make_stat(0, FragmentTarget::Local, 50, 20));
        c.record(make_stat(1, FragmentTarget::Remote("n1".into()), 40, 15));

        let taken = c.take();
        assert_eq!(taken.len(), 3);
        assert_eq!(taken[0].fragment_index, 0);
        assert_eq!(taken[1].fragment_index, 1);
        assert_eq!(taken[2].fragment_index, 2);

        // After take, collector should be empty.
        assert_eq!(c.fragment_count(), 0);
    }

    #[test]
    fn test_total_rows_and_time() {
        let c = FragmentStatsCollector::new();
        c.record(make_stat(0, FragmentTarget::Local, 50, 12));
        c.record(make_stat(1, FragmentTarget::Remote("n1".into()), 50, 18));
        c.record(make_stat(2, FragmentTarget::Remote("n2".into()), 50, 15));

        assert_eq!(c.total_rows(), 150);
        assert_eq!(c.total_execution_time(), Duration::from_millis(45));
        assert_eq!(c.fragment_count(), 3);
    }

    #[test]
    fn test_slowest_fragment() {
        let c = FragmentStatsCollector::new();
        c.record(make_stat(0, FragmentTarget::Local, 10, 5));
        c.record(make_stat(1, FragmentTarget::Remote("n1".into()), 20, 50));
        c.record(make_stat(2, FragmentTarget::Remote("n2".into()), 30, 25));

        let slowest = c.slowest_fragment().expect("should have a slowest");
        assert_eq!(slowest.fragment_index, 1);
        assert_eq!(slowest.execution_time, Duration::from_millis(50));
    }

    #[test]
    fn test_format_summary_output() {
        let c = FragmentStatsCollector::new();
        c.record(make_stat(0, FragmentTarget::Local, 50, 12));
        c.record(make_stat(
            1,
            FragmentTarget::Remote("node-1".into()),
            50,
            18,
        ));
        c.record(make_stat(
            2,
            FragmentTarget::Remote("node-2".into()),
            50,
            15,
        ));

        let summary = c.format_summary();
        assert!(summary.starts_with("Distributed Execution: 3 fragments"));
        assert!(summary.contains("150 rows total"));
        assert!(summary.contains("45ms total"));
        assert!(summary.contains("Fragment 0 (local): 50 rows, 12ms"));
        assert!(summary.contains("Fragment 1 (remote(node-1)): 50 rows, 18ms"));
        assert!(summary.contains("Fragment 2 (remote(node-2)): 50 rows, 15ms"));
    }

    #[test]
    fn test_default_collector() {
        let c = FragmentStatsCollector::default();
        assert_eq!(c.fragment_count(), 0);
        c.record(make_stat(0, FragmentTarget::Local, 1, 1));
        assert_eq!(c.fragment_count(), 1);
    }

    #[test]
    fn test_take_drains_collector() {
        let c = FragmentStatsCollector::new();
        c.record(make_stat(0, FragmentTarget::Local, 10, 5));
        assert_eq!(c.fragment_count(), 1);

        let taken = c.take();
        assert_eq!(taken.len(), 1);
        assert_eq!(c.fragment_count(), 0);
        assert_eq!(c.total_rows(), 0);

        // Take again returns empty.
        let taken2 = c.take();
        assert!(taken2.is_empty());
    }

    #[test]
    fn test_clone_shares_state() {
        let c1 = FragmentStatsCollector::new();
        let c2 = c1.clone();
        c1.record(make_stat(0, FragmentTarget::Local, 10, 5));
        assert_eq!(c2.fragment_count(), 1);
    }
}
