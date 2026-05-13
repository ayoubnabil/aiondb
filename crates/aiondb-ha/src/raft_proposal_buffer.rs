//! Bounded Raft proposal buffer.
//!
//! Holds proposals from the moment the leader accepts them until
//! they are durably committed. Beyond `capacity`, new proposals are
//! NACKed so the writer applies back-pressure instead of building
//! unbounded queues that risk OOM.

use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    pub id: u64,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferDecision {
    Accepted,
    BufferFull,
    Duplicate,
}

#[derive(Clone, Debug)]
pub struct RaftProposalBuffer {
    inner: Arc<std::sync::Mutex<BufferState>>,
    capacity: usize,
}

#[derive(Default, Debug)]
struct BufferState {
    pending: VecDeque<Proposal>,
    committed_high_water: u64,
    seen_ids: std::collections::BTreeSet<u64>,
}

impl RaftProposalBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(BufferState::default())),
            capacity: capacity.max(1),
        }
    }

    pub fn accept(&self, proposal: Proposal) -> BufferDecision {
        let mut g = self.inner.lock().unwrap();
        if g.pending.iter().any(|p| p.id == proposal.id) || g.seen_ids.contains(&proposal.id) {
            return BufferDecision::Duplicate;
        }
        if g.pending.len() >= self.capacity {
            return BufferDecision::BufferFull;
        }
        g.pending.push_back(proposal);
        BufferDecision::Accepted
    }

    pub fn commit(&self, id: u64) -> bool {
        let mut g = self.inner.lock().unwrap();
        let Some(pos) = g.pending.iter().position(|p| p.id == id) else {
            return false;
        };
        g.pending.remove(pos);
        if id > g.committed_high_water {
            g.committed_high_water = id;
        }
        g.seen_ids.insert(id);
        if g.seen_ids.len() > self.capacity * 8 {
            // Trim oldest from seen_ids to bound memory.
            let first = *g.seen_ids.iter().next().unwrap();
            g.seen_ids.remove(&first);
        }
        true
    }

    pub fn abort(&self, id: u64) -> bool {
        let mut g = self.inner.lock().unwrap();
        let Some(pos) = g.pending.iter().position(|p| p.id == id) else {
            return false;
        };
        g.pending.remove(pos);
        true
    }

    pub fn pending(&self) -> usize {
        self.inner.lock().unwrap().pending.len()
    }

    pub fn high_water_mark(&self) -> u64 {
        self.inner.lock().unwrap().committed_high_water
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: u64) -> Proposal {
        Proposal {
            id,
            payload: vec![],
        }
    }

    #[test]
    fn accept_within_capacity() {
        let b = RaftProposalBuffer::new(2);
        assert_eq!(b.accept(p(1)), BufferDecision::Accepted);
        assert_eq!(b.accept(p(2)), BufferDecision::Accepted);
    }

    #[test]
    fn full_buffer_rejects() {
        let b = RaftProposalBuffer::new(2);
        b.accept(p(1));
        b.accept(p(2));
        assert_eq!(b.accept(p(3)), BufferDecision::BufferFull);
    }

    #[test]
    fn duplicate_id_rejected() {
        let b = RaftProposalBuffer::new(4);
        b.accept(p(1));
        assert_eq!(b.accept(p(1)), BufferDecision::Duplicate);
    }

    #[test]
    fn commit_removes_pending_and_bumps_water() {
        let b = RaftProposalBuffer::new(4);
        b.accept(p(1));
        b.accept(p(2));
        assert!(b.commit(1));
        assert_eq!(b.pending(), 1);
        assert_eq!(b.high_water_mark(), 1);
    }

    #[test]
    fn abort_removes_pending() {
        let b = RaftProposalBuffer::new(4);
        b.accept(p(1));
        assert!(b.abort(1));
        assert_eq!(b.pending(), 0);
    }

    #[test]
    fn commit_after_commit_dedups() {
        let b = RaftProposalBuffer::new(4);
        b.accept(p(1));
        b.commit(1);
        assert_eq!(b.accept(p(1)), BufferDecision::Duplicate);
    }
}
