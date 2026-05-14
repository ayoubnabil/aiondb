//! Cluster event log.
//!
//! Append-only timeline of significant cluster events :
//!
//! - Node join / leave
//! - Lease transfer
//! - Range split / merge
//! - Failover trigger
//! - Schema migration phase change
//!
//! Bounded ring buffer so memory usage stays predictable. Older
//! events get evicted when the capacity is reached.

use std::collections::VecDeque;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ClusterEventKind {
    NodeJoined {
        node_id: u64,
    },
    NodeLeft {
        node_id: u64,
    },
    LeaseTransferred {
        range_id: u64,
        from_node: u64,
        to_node: u64,
    },
    RangeSplit {
        range_id: u64,
        new_range_id: u64,
    },
    RangeMerged {
        left_range_id: u64,
        right_range_id: u64,
    },
    FailoverTriggered {
        failed_node: u64,
    },
    SchemaPhaseChanged {
        element_key: String,
        phase: String,
    },
    Custom {
        tag: String,
        payload: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClusterEvent {
    pub id: u64,
    pub at_us: u64,
    pub kind: ClusterEventKind,
}

#[derive(Clone, Debug, Default)]
pub struct ClusterEventLog {
    inner: Arc<std::sync::Mutex<Inner>>,
    capacity: usize,
}

#[derive(Debug, Default)]
struct Inner {
    events: VecDeque<ClusterEvent>,
    next_id: u64,
}

impl ClusterEventLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::default(),
            capacity: capacity.max(1),
        }
    }

    pub fn append(&self, kind: ClusterEventKind) -> u64 {
        let now_us = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_micros()).ok())
            .unwrap_or(0);
        let mut guard = self.inner.lock().unwrap();
        guard.next_id = guard.next_id.saturating_add(1);
        let id = guard.next_id;
        guard.events.push_back(ClusterEvent {
            id,
            at_us: now_us,
            kind,
        });
        while guard.events.len() > self.capacity {
            guard.events.pop_front();
        }
        id
    }

    pub fn tail(&self, n: usize) -> Vec<ClusterEvent> {
        let guard = self.inner.lock().unwrap();
        let start = guard.events.len().saturating_sub(n);
        guard.events.iter().skip(start).cloned().collect()
    }

    pub fn count(&self) -> usize {
        self.inner.lock().unwrap().events.len()
    }

    pub fn all(&self) -> Vec<ClusterEvent> {
        self.inner.lock().unwrap().events.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_returns_monotonic_ids() {
        let log = ClusterEventLog::new(10);
        let id1 = log.append(ClusterEventKind::NodeJoined { node_id: 1 });
        let id2 = log.append(ClusterEventKind::NodeJoined { node_id: 2 });
        assert!(id2 > id1);
    }

    #[test]
    fn capacity_evicts_oldest() {
        let log = ClusterEventLog::new(3);
        for i in 1..=5 {
            log.append(ClusterEventKind::NodeJoined { node_id: i });
        }
        assert_eq!(log.count(), 3);
        let all = log.all();
        // Oldest events were evicted; surviving ids are 3, 4, 5.
        let ids: Vec<u64> = all.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![3, 4, 5]);
    }

    #[test]
    fn tail_returns_most_recent_events() {
        let log = ClusterEventLog::new(10);
        log.append(ClusterEventKind::NodeJoined { node_id: 1 });
        log.append(ClusterEventKind::NodeJoined { node_id: 2 });
        log.append(ClusterEventKind::NodeJoined { node_id: 3 });
        let tail = log.tail(2);
        let ids: Vec<u64> = tail.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![2, 3]);
    }

    #[test]
    fn event_kinds_serialise() {
        let log = ClusterEventLog::new(10);
        log.append(ClusterEventKind::LeaseTransferred {
            range_id: 1,
            from_node: 2,
            to_node: 3,
        });
        let all = log.all();
        let json = serde_json::to_string(&all[0]).unwrap();
        assert!(json.contains("LeaseTransferred"));
    }

    #[test]
    fn custom_events_record_arbitrary_payload() {
        let log = ClusterEventLog::new(10);
        log.append(ClusterEventKind::Custom {
            tag: "operator".into(),
            payload: "manual restart".into(),
        });
        assert_eq!(log.count(), 1);
    }
}
