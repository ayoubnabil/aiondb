//! Read repair.
//!
//! When a quorum read collects diverging replica replies for the
//! same key, the most-recent value (per HLC) is returned to the
//! client and queued for write-back to the stale replicas. The
//! repair runs in the background so the client doesn't pay the
//! latency cost.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::range_descriptor::ReplicaId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplicaRead {
    pub replica: ReplicaId,
    pub timestamp_ns: u64,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepairTask {
    pub replica: ReplicaId,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub timestamp_ns: u64,
}

pub fn compute_repairs(key: Vec<u8>, reads: &[ReplicaRead]) -> (Option<Vec<u8>>, Vec<RepairTask>) {
    if reads.is_empty() {
        return (None, Vec::new());
    }
    let latest = reads.iter().max_by_key(|r| r.timestamp_ns).unwrap().clone();
    let mut repairs = Vec::new();
    for r in reads {
        if r.value != latest.value || r.timestamp_ns < latest.timestamp_ns {
            repairs.push(RepairTask {
                replica: r.replica,
                key: key.clone(),
                value: latest.value.clone(),
                timestamp_ns: latest.timestamp_ns,
            });
        }
    }
    (Some(latest.value), repairs)
}

#[derive(Clone, Debug, Default)]
pub struct ReadRepairQueue {
    inner: Arc<std::sync::Mutex<BTreeMap<(ReplicaId, Vec<u8>), RepairTask>>>,
}

impl ReadRepairQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enqueue(&self, task: RepairTask) {
        let key = (task.replica, task.key.clone());
        let mut g = self.inner.lock().unwrap();
        // Keep only the newest timestamp per (replica, key).
        match g.get_mut(&key) {
            Some(existing) if existing.timestamp_ns >= task.timestamp_ns => {}
            _ => {
                g.insert(key, task);
            }
        }
    }

    pub fn drain(&self) -> Vec<RepairTask> {
        let mut g = self.inner.lock().unwrap();
        let out: Vec<RepairTask> = g.values().cloned().collect();
        g.clear();
        out
    }

    pub fn pending(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(n: u64) -> ReplicaId {
        ReplicaId::new(n)
    }

    #[test]
    fn unanimous_quorum_emits_no_repairs() {
        let reads = vec![
            ReplicaRead {
                replica: rep(1),
                timestamp_ns: 10,
                value: b"v".to_vec(),
            },
            ReplicaRead {
                replica: rep(2),
                timestamp_ns: 10,
                value: b"v".to_vec(),
            },
            ReplicaRead {
                replica: rep(3),
                timestamp_ns: 10,
                value: b"v".to_vec(),
            },
        ];
        let (v, repairs) = compute_repairs(b"k".to_vec(), &reads);
        assert_eq!(v.as_deref(), Some(&b"v"[..]));
        assert!(repairs.is_empty());
    }

    #[test]
    fn stale_replica_gets_repair() {
        let reads = vec![
            ReplicaRead {
                replica: rep(1),
                timestamp_ns: 10,
                value: b"old".to_vec(),
            },
            ReplicaRead {
                replica: rep(2),
                timestamp_ns: 20,
                value: b"new".to_vec(),
            },
        ];
        let (v, repairs) = compute_repairs(b"k".to_vec(), &reads);
        assert_eq!(v.as_deref(), Some(&b"new"[..]));
        assert_eq!(repairs.len(), 1);
        assert_eq!(repairs[0].replica, rep(1));
        assert_eq!(repairs[0].value, b"new");
    }

    #[test]
    fn empty_reads_returns_none() {
        let (v, repairs) = compute_repairs(b"k".to_vec(), &[]);
        assert!(v.is_none());
        assert!(repairs.is_empty());
    }

    #[test]
    fn queue_deduplicates_per_replica_key() {
        let q = ReadRepairQueue::new();
        q.enqueue(RepairTask {
            replica: rep(1),
            key: b"k".to_vec(),
            value: b"a".to_vec(),
            timestamp_ns: 1,
        });
        q.enqueue(RepairTask {
            replica: rep(1),
            key: b"k".to_vec(),
            value: b"b".to_vec(),
            timestamp_ns: 2,
        });
        assert_eq!(q.pending(), 1);
        let tasks = q.drain();
        assert_eq!(tasks[0].value, b"b");
    }

    #[test]
    fn queue_keeps_newest_timestamp() {
        let q = ReadRepairQueue::new();
        q.enqueue(RepairTask {
            replica: rep(1),
            key: b"k".to_vec(),
            value: b"new".to_vec(),
            timestamp_ns: 100,
        });
        q.enqueue(RepairTask {
            replica: rep(1),
            key: b"k".to_vec(),
            value: b"old".to_vec(),
            timestamp_ns: 50,
        });
        let tasks = q.drain();
        assert_eq!(tasks[0].value, b"new");
    }

    #[test]
    fn drain_empties_queue() {
        let q = ReadRepairQueue::new();
        q.enqueue(RepairTask {
            replica: rep(1),
            key: b"k".to_vec(),
            value: b"v".to_vec(),
            timestamp_ns: 1,
        });
        q.drain();
        assert_eq!(q.pending(), 0);
    }
}
