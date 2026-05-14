//! SLA-aware request prioritisation queue.
//!
//! Min-heap keyed by `(deadline, submission_seq)`. The scheduler
//! drains in order so requests with the earliest deadlines (most
//! urgent) execute first. Use to drive request scheduling when the
//! admission controller has space but you want SLA-aware ordering.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct ScheduledRequest<P: Clone + std::fmt::Debug> {
    pub deadline: Instant,
    pub seq: u64,
    pub payload: P,
}

#[derive(Clone, Debug)]
pub struct SlaScheduler<P: Clone + std::fmt::Debug> {
    inner: Arc<std::sync::Mutex<Inner<P>>>,
}

#[derive(Debug)]
struct Inner<P: Clone + std::fmt::Debug> {
    heap: BinaryHeap<Reverse<HeapEntry<P>>>,
    next_seq: u64,
}

impl<P: Clone + std::fmt::Debug> Default for Inner<P> {
    fn default() -> Self {
        Self {
            heap: BinaryHeap::new(),
            next_seq: 0,
        }
    }
}

#[derive(Debug)]
struct HeapEntry<P: Clone + std::fmt::Debug> {
    deadline: Instant,
    seq: u64,
    payload: P,
}

impl<P: Clone + std::fmt::Debug> PartialEq for HeapEntry<P> {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.seq == other.seq
    }
}

impl<P: Clone + std::fmt::Debug> Eq for HeapEntry<P> {}

impl<P: Clone + std::fmt::Debug> PartialOrd for HeapEntry<P> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<P: Clone + std::fmt::Debug> Ord for HeapEntry<P> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.deadline
            .cmp(&other.deadline)
            .then_with(|| self.seq.cmp(&other.seq))
    }
}

impl<P: Clone + std::fmt::Debug> Default for SlaScheduler<P> {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
        }
    }
}

impl<P: Clone + std::fmt::Debug> SlaScheduler<P> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn submit(&self, deadline: Instant, payload: P) -> u64 {
        let mut guard = self.inner.lock().unwrap();
        guard.next_seq = guard.next_seq.saturating_add(1);
        let seq = guard.next_seq;
        guard.heap.push(Reverse(HeapEntry {
            deadline,
            seq,
            payload,
        }));
        seq
    }

    pub fn pop_next(&self) -> Option<ScheduledRequest<P>> {
        let mut guard = self.inner.lock().unwrap();
        guard.heap.pop().map(|Reverse(e)| ScheduledRequest {
            deadline: e.deadline,
            seq: e.seq,
            payload: e.payload,
        })
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().heap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().heap.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn pop_returns_earliest_deadline_first() {
        let s: SlaScheduler<&'static str> = SlaScheduler::new();
        let now = Instant::now();
        s.submit(now + Duration::from_secs(10), "later");
        s.submit(now + Duration::from_secs(1), "soon");
        s.submit(now + Duration::from_secs(5), "mid");
        assert_eq!(s.pop_next().unwrap().payload, "soon");
        assert_eq!(s.pop_next().unwrap().payload, "mid");
        assert_eq!(s.pop_next().unwrap().payload, "later");
    }

    #[test]
    fn tie_break_by_seq() {
        let s: SlaScheduler<u32> = SlaScheduler::new();
        let now = Instant::now() + Duration::from_secs(1);
        s.submit(now, 1);
        s.submit(now, 2);
        assert_eq!(s.pop_next().unwrap().payload, 1);
        assert_eq!(s.pop_next().unwrap().payload, 2);
    }

    #[test]
    fn pop_on_empty_returns_none() {
        let s: SlaScheduler<()> = SlaScheduler::new();
        assert!(s.pop_next().is_none());
    }

    #[test]
    fn submit_advances_seq_monotonically() {
        let s: SlaScheduler<()> = SlaScheduler::new();
        let a = s.submit(Instant::now(), ());
        let b = s.submit(Instant::now(), ());
        assert!(b > a);
    }

    #[test]
    fn len_reflects_pending() {
        let s: SlaScheduler<u8> = SlaScheduler::new();
        assert!(s.is_empty());
        s.submit(Instant::now(), 1);
        s.submit(Instant::now(), 2);
        assert_eq!(s.len(), 2);
    }
}
