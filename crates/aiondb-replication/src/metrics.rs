//! Replica driver runtime metrics.
//!
//! Lightweight counters tracked by [`crate::client::run`] so operators can
//! tell at a glance how many reconnects the driver has gone through, how
//! many bytes of WAL it has streamed in, and when the last successful
//! session ended. Backed by `AtomicU64` so they are safe to read from any
//! thread without locking. The supervisor exposes a [`MetricsSnapshot`]
//! and a `Display` impl suitable for the `/metrics` text exposition format.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Shared counter handle used by the driver and external metrics scrapers.
#[derive(Clone, Debug, Default)]
pub struct ReplicaMetrics {
    inner: Arc<ReplicaMetricsInner>,
}

#[derive(Debug, Default)]
struct ReplicaMetricsInner {
    sessions_started: AtomicU64,
    sessions_succeeded: AtomicU64,
    sessions_failed: AtomicU64,
    reconnects: AtomicU64,
    wal_bytes_received: AtomicU64,
    standby_status_updates_sent: AtomicU64,
    last_session_started_at_us: AtomicU64,
}

impl ReplicaMetrics {
    /// Construct a fresh counter set.
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn note_session_started(&self) {
        saturating_fetch_add(&self.inner.sessions_started, 1);
        let now = current_unix_micros();
        self.inner
            .last_session_started_at_us
            .store(now, Ordering::Relaxed);
    }

    pub(crate) fn note_session_succeeded(&self) {
        saturating_fetch_add(&self.inner.sessions_succeeded, 1);
    }

    pub(crate) fn note_session_failed(&self) {
        saturating_fetch_add(&self.inner.sessions_failed, 1);
    }

    pub(crate) fn note_reconnect(&self) {
        saturating_fetch_add(&self.inner.reconnects, 1);
    }

    pub(crate) fn note_wal_bytes(&self, bytes: u64) {
        saturating_fetch_add(&self.inner.wal_bytes_received, bytes);
    }

    pub(crate) fn note_status_update_sent(&self) {
        saturating_fetch_add(&self.inner.standby_status_updates_sent, 1);
    }

    /// Read a consistent snapshot of all counters. Counters are monotonic
    /// `u64`s read with `Relaxed` ordering -- callers that need stronger
    /// ordering should read directly via [`ReplicaMetrics::snapshot`].
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            sessions_started: self.inner.sessions_started.load(Ordering::Relaxed),
            sessions_succeeded: self.inner.sessions_succeeded.load(Ordering::Relaxed),
            sessions_failed: self.inner.sessions_failed.load(Ordering::Relaxed),
            reconnects: self.inner.reconnects.load(Ordering::Relaxed),
            wal_bytes_received: self.inner.wal_bytes_received.load(Ordering::Relaxed),
            standby_status_updates_sent: self
                .inner
                .standby_status_updates_sent
                .load(Ordering::Relaxed),
            last_session_started_at_us: {
                let raw = self
                    .inner
                    .last_session_started_at_us
                    .load(Ordering::Relaxed);
                if raw == 0 {
                    None
                } else {
                    Some(raw)
                }
            },
        }
    }
}

fn saturating_fetch_add(counter: &AtomicU64, value: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(value))
    });
}

/// Read-only counter snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MetricsSnapshot {
    pub sessions_started: u64,
    pub sessions_succeeded: u64,
    pub sessions_failed: u64,
    pub reconnects: u64,
    pub wal_bytes_received: u64,
    pub standby_status_updates_sent: u64,
    pub last_session_started_at_us: Option<u64>,
}

fn current_unix_micros() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_starts_zero() {
        let metrics = ReplicaMetrics::new();
        let snap = metrics.snapshot();
        assert_eq!(snap.sessions_started, 0);
        assert_eq!(snap.reconnects, 0);
        assert!(snap.last_session_started_at_us.is_none());
    }

    #[test]
    fn note_session_started_increments_and_timestamps() {
        let metrics = ReplicaMetrics::new();
        metrics.note_session_started();
        metrics.note_session_started();
        let snap = metrics.snapshot();
        assert_eq!(snap.sessions_started, 2);
        assert!(snap.last_session_started_at_us.is_some());
    }

    #[test]
    fn note_wal_bytes_aggregates_across_calls() {
        let metrics = ReplicaMetrics::new();
        metrics.note_wal_bytes(1024);
        metrics.note_wal_bytes(2048);
        assert_eq!(metrics.snapshot().wal_bytes_received, 3072);
    }

    #[test]
    fn counters_saturate_instead_of_wrapping() {
        let metrics = ReplicaMetrics::new();
        metrics
            .inner
            .wal_bytes_received
            .store(u64::MAX - 1, Ordering::Relaxed);
        metrics.note_wal_bytes(16);
        assert_eq!(metrics.snapshot().wal_bytes_received, u64::MAX);

        metrics
            .inner
            .sessions_failed
            .store(u64::MAX, Ordering::Relaxed);
        metrics.note_session_failed();
        assert_eq!(metrics.snapshot().sessions_failed, u64::MAX);
    }

    #[test]
    fn clone_shares_same_atomic_storage() {
        let metrics = ReplicaMetrics::new();
        let clone = metrics.clone();
        metrics.note_reconnect();
        assert_eq!(clone.snapshot().reconnects, 1);
    }
}
