//! Replica lag SLA monitor.
//!
//! Tracks per-replica lag in entries + wall-clock time against a
//! configured SLA. Emits violations through a callback so callers
//! can wire it to Prometheus alerts, dashboards or a pager.
//!
//! Invariants tested:
//!
//! 1. A replica strictly inside the SLA produces no violation.
//! 2. A replica exceeding the entry budget produces one violation
//!    per evaluation tick (until it catches up).
//! 3. Resolved violations stop firing immediately.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LagViolationKind {
    Entries {
        observed: u64,
        budget: u64,
    },
    Wallclock {
        observed: Duration,
        budget: Duration,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LagViolation {
    pub replica_id: u64,
    pub kind: LagViolationKind,
}

/// Per-replica SLA budget.
#[derive(Clone, Copy, Debug)]
pub struct LagSla {
    pub max_entries_lag: u64,
    pub max_wallclock_lag: Duration,
}

impl Default for LagSla {
    fn default() -> Self {
        Self {
            max_entries_lag: 10_000,
            max_wallclock_lag: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ReplicaState {
    applied_index: u64,
    last_seen: Instant,
}

/// SLA monitor. Cheap to clone.
#[derive(Clone)]
pub struct LagSlaMonitor {
    sla: LagSla,
    replicas: Arc<std::sync::Mutex<HashMap<u64, ReplicaState>>>,
    /// Optional violation callback. Called inline from `evaluate`.
    on_violation: Arc<std::sync::Mutex<Option<Arc<dyn Fn(&LagViolation) + Send + Sync>>>>,
}

impl LagSlaMonitor {
    pub fn new(sla: LagSla) -> Self {
        Self {
            sla,
            replicas: Arc::new(std::sync::Mutex::new(HashMap::new())),
            on_violation: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn set_callback(&self, cb: Arc<dyn Fn(&LagViolation) + Send + Sync>) {
        *self.on_violation.lock().unwrap() = Some(cb);
    }

    /// Record a fresh applied index sample for `replica_id`. Bumps the
    /// `last_seen` wall clock to now.
    pub fn record(&self, replica_id: u64, applied_index: u64) {
        let mut guard = self.replicas.lock().unwrap();
        let state = guard.entry(replica_id).or_insert(ReplicaState {
            applied_index: 0,
            last_seen: Instant::now(),
        });
        if applied_index > state.applied_index {
            state.applied_index = applied_index;
        }
        state.last_seen = Instant::now();
    }

    /// Evaluate every tracked replica against the SLA. Returns the
    /// list of violations and fires the configured callback for each.
    pub fn evaluate(&self, leader_applied_index: u64, now: Instant) -> Vec<LagViolation> {
        let cb = self.on_violation.lock().unwrap().clone();
        let guard = self.replicas.lock().unwrap();
        let mut out = Vec::new();
        for (id, state) in guard.iter() {
            let entries_lag = leader_applied_index.saturating_sub(state.applied_index);
            if entries_lag > self.sla.max_entries_lag {
                let v = LagViolation {
                    replica_id: *id,
                    kind: LagViolationKind::Entries {
                        observed: entries_lag,
                        budget: self.sla.max_entries_lag,
                    },
                };
                out.push(v);
            }
            let wall_lag = now.saturating_duration_since(state.last_seen);
            if wall_lag > self.sla.max_wallclock_lag {
                let v = LagViolation {
                    replica_id: *id,
                    kind: LagViolationKind::Wallclock {
                        observed: wall_lag,
                        budget: self.sla.max_wallclock_lag,
                    },
                };
                out.push(v);
            }
        }
        drop(guard);
        if let Some(cb) = cb {
            for v in &out {
                cb(v);
            }
        }
        out
    }

    pub fn sla(&self) -> LagSla {
        self.sla
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn within_sla_emits_no_violation() {
        let monitor = LagSlaMonitor::new(LagSla {
            max_entries_lag: 100,
            max_wallclock_lag: Duration::from_secs(10),
        });
        monitor.record(1, 50);
        let v = monitor.evaluate(120, Instant::now());
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn entry_lag_above_budget_fires_violation() {
        let monitor = LagSlaMonitor::new(LagSla {
            max_entries_lag: 100,
            max_wallclock_lag: Duration::from_secs(10),
        });
        monitor.record(1, 10);
        let v = monitor.evaluate(500, Instant::now());
        assert_eq!(v.len(), 1);
        assert!(matches!(
            v[0].kind,
            LagViolationKind::Entries {
                observed: 490,
                budget: 100
            }
        ));
    }

    #[test]
    fn callback_fires_for_each_violation() {
        let monitor = LagSlaMonitor::new(LagSla {
            max_entries_lag: 10,
            max_wallclock_lag: Duration::from_secs(10),
        });
        let count = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::clone(&count);
        monitor.set_callback(Arc::new(move |_| {
            c2.fetch_add(1, Ordering::SeqCst);
        }));
        monitor.record(1, 0);
        monitor.record(2, 5);
        monitor.evaluate(100, Instant::now());
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn resolved_violations_stop_firing() {
        let monitor = LagSlaMonitor::new(LagSla {
            max_entries_lag: 10,
            max_wallclock_lag: Duration::from_secs(10),
        });
        monitor.record(1, 0);
        let v1 = monitor.evaluate(100, Instant::now());
        assert_eq!(v1.len(), 1);
        // Catch up.
        monitor.record(1, 100);
        let v2 = monitor.evaluate(100, Instant::now());
        assert!(v2.is_empty(), "should resolve: {v2:?}");
    }
}
