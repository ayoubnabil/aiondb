//! Online MVCC vacuum.
//!
//! Background garbage collector for MVCC versions whose timestamp is
//! below the system-wide GC horizon (the oldest still-running txn's
//! start ts, capped by the cluster's closed-timestamp watermark).
//!
//! This module owns the **scheduling** + **bookkeeping** side; the
//! actual byte removal lives in the storage engine.

use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VacuumTask {
    pub range_id: u64,
    pub horizon_us: u64,
    pub estimated_bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct OnlineVacuumScheduler {
    inner: Arc<std::sync::Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    pending: BTreeMap<u64, VacuumTask>,
    completed: u64,
    bytes_reclaimed: u64,
}

impl OnlineVacuumScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn schedule(&self, task: VacuumTask) {
        self.inner
            .lock()
            .unwrap()
            .pending
            .insert(task.range_id, task);
    }

    pub fn next(&self) -> Option<VacuumTask> {
        let mut guard = self.inner.lock().unwrap();
        let key = guard.pending.keys().next().copied()?;
        guard.pending.remove(&key)
    }

    pub fn complete(&self, task: &VacuumTask, bytes_reclaimed: u64) {
        let mut guard = self.inner.lock().unwrap();
        guard.completed = guard.completed.saturating_add(1);
        guard.bytes_reclaimed = guard.bytes_reclaimed.saturating_add(bytes_reclaimed);
        let _ = task;
    }

    pub fn pending_count(&self) -> usize {
        self.inner.lock().unwrap().pending.len()
    }

    pub fn completed_count(&self) -> u64 {
        self.inner.lock().unwrap().completed
    }

    pub fn bytes_reclaimed(&self) -> u64 {
        self.inner.lock().unwrap().bytes_reclaimed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(range: u64) -> VacuumTask {
        VacuumTask {
            range_id: range,
            horizon_us: 1000,
            estimated_bytes: 4096,
        }
    }

    #[test]
    fn schedule_then_next_pops_in_id_order() {
        let s = OnlineVacuumScheduler::new();
        s.schedule(task(3));
        s.schedule(task(1));
        s.schedule(task(2));
        assert_eq!(s.next().unwrap().range_id, 1);
        assert_eq!(s.next().unwrap().range_id, 2);
        assert_eq!(s.next().unwrap().range_id, 3);
        assert!(s.next().is_none());
    }

    #[test]
    fn schedule_overwrites_same_range() {
        let s = OnlineVacuumScheduler::new();
        let mut t = task(1);
        t.estimated_bytes = 100;
        s.schedule(t);
        let mut t2 = task(1);
        t2.estimated_bytes = 200;
        s.schedule(t2);
        assert_eq!(s.pending_count(), 1);
        assert_eq!(s.next().unwrap().estimated_bytes, 200);
    }

    #[test]
    fn complete_tallies_counters() {
        let s = OnlineVacuumScheduler::new();
        s.schedule(task(1));
        let t = s.next().unwrap();
        s.complete(&t, 1024);
        assert_eq!(s.completed_count(), 1);
        assert_eq!(s.bytes_reclaimed(), 1024);
    }

    #[test]
    fn empty_scheduler_yields_none() {
        let s = OnlineVacuumScheduler::new();
        assert!(s.next().is_none());
    }
}
