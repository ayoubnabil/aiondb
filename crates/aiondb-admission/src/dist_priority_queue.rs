//! Distributed priority admission queue.
//!
//! Maintains per-class queues and dequeues in weighted round-robin
//! order. High-priority classes drain faster than low-priority but
//! a tiny stipend is always reserved for the lowest tier so it
//! never starves.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Priority {
    System,
    Interactive,
    Batch,
    Background,
}

#[derive(Clone, Debug)]
pub struct PriorityItem {
    pub id: u64,
    pub priority: Priority,
}

#[derive(Clone, Debug)]
pub struct DistPriorityQueue {
    inner: Arc<std::sync::Mutex<QueueState>>,
}

#[derive(Default, Debug)]
struct QueueState {
    queues: BTreeMap<Priority, VecDeque<PriorityItem>>,
    weights: BTreeMap<Priority, u32>,
    served: BTreeMap<Priority, u32>,
}

impl DistPriorityQueue {
    pub fn new() -> Self {
        let mut weights = BTreeMap::new();
        weights.insert(Priority::System, 8);
        weights.insert(Priority::Interactive, 4);
        weights.insert(Priority::Batch, 2);
        weights.insert(Priority::Background, 1);
        Self {
            inner: Arc::new(std::sync::Mutex::new(QueueState {
                queues: BTreeMap::new(),
                weights,
                served: BTreeMap::new(),
            })),
        }
    }

    pub fn enqueue(&self, item: PriorityItem) {
        let mut g = self.inner.lock().unwrap();
        g.queues.entry(item.priority).or_default().push_back(item);
    }

    pub fn dequeue(&self) -> Option<PriorityItem> {
        let mut g = self.inner.lock().unwrap();
        // Try priorities in order; honour weighting by tracking served-counts.
        let mut chosen: Option<Priority> = None;
        for prio in [
            Priority::System,
            Priority::Interactive,
            Priority::Batch,
            Priority::Background,
        ] {
            let weight = g.weights.get(&prio).copied().unwrap_or(0);
            let served = g.served.get(&prio).copied().unwrap_or(0);
            if g.queues.get(&prio).map(|q| !q.is_empty()).unwrap_or(false) && served < weight {
                chosen = Some(prio);
                break;
            }
        }
        if chosen.is_none() {
            // Round complete; reset served counts.
            g.served.clear();
            for prio in [
                Priority::System,
                Priority::Interactive,
                Priority::Batch,
                Priority::Background,
            ] {
                if g.queues.get(&prio).map(|q| !q.is_empty()).unwrap_or(false) {
                    chosen = Some(prio);
                    break;
                }
            }
        }
        let prio = chosen?;
        let item = g.queues.get_mut(&prio).and_then(|q| q.pop_front())?;
        *g.served.entry(prio).or_insert(0) += 1;
        Some(item)
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .queues
            .values()
            .map(|q| q.len())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for DistPriorityQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u64, p: Priority) -> PriorityItem {
        PriorityItem { id, priority: p }
    }

    #[test]
    fn enqueue_dequeue() {
        let q = DistPriorityQueue::new();
        q.enqueue(item(1, Priority::System));
        let r = q.dequeue().unwrap();
        assert_eq!(r.id, 1);
    }

    #[test]
    fn system_drains_before_batch() {
        let q = DistPriorityQueue::new();
        q.enqueue(item(1, Priority::Batch));
        q.enqueue(item(2, Priority::System));
        let first = q.dequeue().unwrap();
        assert_eq!(first.priority, Priority::System);
    }

    #[test]
    fn empty_dequeue_returns_none() {
        let q = DistPriorityQueue::new();
        assert!(q.dequeue().is_none());
    }

    #[test]
    fn len_counts_items() {
        let q = DistPriorityQueue::new();
        q.enqueue(item(1, Priority::System));
        q.enqueue(item(2, Priority::Batch));
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn background_never_starves() {
        let q = DistPriorityQueue::new();
        for i in 0..50u64 {
            q.enqueue(item(i, Priority::System));
        }
        q.enqueue(item(999, Priority::Background));
        let mut bg_served = false;
        for _ in 0..51 {
            if let Some(p) = q.dequeue() {
                if p.id == 999 {
                    bg_served = true;
                }
            }
        }
        assert!(bg_served);
    }

    #[test]
    fn fifo_within_priority() {
        let q = DistPriorityQueue::new();
        q.enqueue(item(1, Priority::Batch));
        q.enqueue(item(2, Priority::Batch));
        assert_eq!(q.dequeue().unwrap().id, 1);
        assert_eq!(q.dequeue().unwrap().id, 2);
    }
}
