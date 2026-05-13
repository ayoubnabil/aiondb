//! Append-only leader change log.
//!
//! Records every leadership transition with timestamp and cause.
//! Operators consult it post-incident to understand why a range
//! changed leaders during an outage.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionCause {
    Election,
    AdminTransfer,
    HeartbeatTimeout,
    NodeShutdown,
    ConfigChange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaderTransition {
    pub range_id: u64,
    pub old_leader: Option<u64>,
    pub new_leader: u64,
    pub term: u64,
    pub at: SystemTime,
    pub cause: TransitionCause,
}

#[derive(Clone, Debug)]
pub struct LeaderChangeLog {
    inner: Arc<std::sync::Mutex<VecDeque<LeaderTransition>>>,
    capacity: usize,
}

impl LeaderChangeLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(VecDeque::new())),
            capacity: capacity.max(1),
        }
    }

    pub fn record(&self, t: LeaderTransition) {
        let mut g = self.inner.lock().unwrap();
        if g.len() >= self.capacity {
            g.pop_front();
        }
        g.push_back(t);
    }

    pub fn recent(&self, limit: usize) -> Vec<LeaderTransition> {
        let g = self.inner.lock().unwrap();
        let n = limit.min(g.len());
        g.iter().rev().take(n).cloned().collect()
    }

    pub fn for_range(&self, range_id: u64) -> Vec<LeaderTransition> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.range_id == range_id)
            .cloned()
            .collect()
    }

    pub fn count_by_cause(&self) -> std::collections::BTreeMap<TransitionCause, u32> {
        let mut counts = std::collections::BTreeMap::new();
        for t in self.inner.lock().unwrap().iter() {
            *counts.entry(t.cause).or_insert(0) += 1;
        }
        counts
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl PartialOrd for TransitionCause {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for TransitionCause {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (*self as u8).cmp(&(*other as u8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(range: u64, new: u64, term: u64, cause: TransitionCause) -> LeaderTransition {
        LeaderTransition {
            range_id: range,
            old_leader: None,
            new_leader: new,
            term,
            at: SystemTime::now(),
            cause,
        }
    }

    #[test]
    fn record_then_recent() {
        let l = LeaderChangeLog::new(10);
        l.record(t(1, 100, 1, TransitionCause::Election));
        let recent = l.recent(5);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].range_id, 1);
    }

    #[test]
    fn capacity_caps_history() {
        let l = LeaderChangeLog::new(2);
        l.record(t(1, 100, 1, TransitionCause::Election));
        l.record(t(2, 200, 1, TransitionCause::Election));
        l.record(t(3, 300, 1, TransitionCause::Election));
        assert_eq!(l.len(), 2);
        let recent = l.recent(5);
        assert_eq!(recent[0].range_id, 3);
        assert_eq!(recent[1].range_id, 2);
    }

    #[test]
    fn for_range_filters() {
        let l = LeaderChangeLog::new(10);
        l.record(t(1, 100, 1, TransitionCause::Election));
        l.record(t(2, 200, 1, TransitionCause::Election));
        l.record(t(1, 101, 2, TransitionCause::HeartbeatTimeout));
        let r1 = l.for_range(1);
        assert_eq!(r1.len(), 2);
    }

    #[test]
    fn count_by_cause_aggregates() {
        let l = LeaderChangeLog::new(10);
        l.record(t(1, 100, 1, TransitionCause::Election));
        l.record(t(1, 101, 2, TransitionCause::Election));
        l.record(t(2, 200, 1, TransitionCause::HeartbeatTimeout));
        let counts = l.count_by_cause();
        assert_eq!(counts[&TransitionCause::Election], 2);
        assert_eq!(counts[&TransitionCause::HeartbeatTimeout], 1);
    }

    #[test]
    fn empty_log_returns_empty() {
        let l = LeaderChangeLog::new(10);
        assert!(l.is_empty());
        assert!(l.recent(5).is_empty());
    }
}
