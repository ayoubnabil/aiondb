//! Distributed sequence generator.
//!
//! Allocates blocks of N unique monotonic ids per node from a single
//! cluster-wide counter. Each node reserves a block (e.g. 1000 ids)
//! upfront and serves local `nextval()` calls out of that block
//! without further coordination. When a block is exhausted, the node
//! reserves a fresh one.
//!
//! Two layers :
//!
//! - [`SequenceAllocator`] : authoritative cluster source, bumped via
//!   the control plane / Raft. Provides `reserve_block(node, size)`.
//! - [`LocalSequenceCache`] : per-node cache that hands out ids from
//!   the reserved block, calling back into the allocator when empty.
//!
//! The model matches CockroachDB's `unique_rowid()` and TiDB's
//! `AUTO_RANDOM`.

use std::sync::Arc;

use aiondb_core::DbResult;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SequenceBlock {
    pub start_inclusive: u64,
    pub end_exclusive: u64,
}

impl SequenceBlock {
    pub fn size(&self) -> u64 {
        self.end_exclusive.saturating_sub(self.start_inclusive)
    }
}

/// Cluster-wide allocator. Backed by a single atomic counter -- in
/// production this is fed by a Raft proposal so all nodes see a
/// consistent global watermark.
#[derive(Clone, Debug, Default)]
pub struct SequenceAllocator {
    counter: Arc<std::sync::atomic::AtomicU64>,
    /// Minimum block size. Production deployments often pick 1_000 to
    /// 10_000 so the reservation rate stays low.
    min_block_size: u64,
}

impl SequenceAllocator {
    pub fn new(initial: u64, min_block_size: u64) -> Self {
        Self {
            counter: Arc::new(std::sync::atomic::AtomicU64::new(initial)),
            min_block_size: min_block_size.max(1),
        }
    }

    pub fn reserve(&self, desired_size: u64) -> SequenceBlock {
        let size = desired_size.max(self.min_block_size);
        let start = self
            .counter
            .fetch_add(size, std::sync::atomic::Ordering::SeqCst);
        SequenceBlock {
            start_inclusive: start,
            end_exclusive: start.saturating_add(size),
        }
    }

    pub fn peek(&self) -> u64 {
        self.counter.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Per-node sequence cache. Holds a reserved block and hands out ids
/// out of it. Cheap to clone.
#[derive(Clone, Debug)]
pub struct LocalSequenceCache {
    allocator: SequenceAllocator,
    state: Arc<std::sync::Mutex<CacheState>>,
    block_size: u64,
}

#[derive(Debug)]
struct CacheState {
    block: SequenceBlock,
    next: u64,
}

impl LocalSequenceCache {
    pub fn new(allocator: SequenceAllocator, block_size: u64) -> Self {
        let block = allocator.reserve(block_size);
        Self {
            allocator,
            state: Arc::new(std::sync::Mutex::new(CacheState {
                next: block.start_inclusive,
                block,
            })),
            block_size,
        }
    }

    /// Return the next id from the local cache, reserving a new block
    /// if the current one is exhausted.
    pub fn nextval(&self) -> DbResult<u64> {
        let mut guard = self.state.lock().unwrap();
        if guard.next >= guard.block.end_exclusive {
            let fresh = self.allocator.reserve(self.block_size);
            guard.block = fresh;
            guard.next = fresh.start_inclusive;
        }
        let id = guard.next;
        guard.next = guard.next.saturating_add(1);
        Ok(id)
    }

    pub fn current_block(&self) -> SequenceBlock {
        self.state.lock().unwrap().block
    }

    pub fn remaining(&self) -> u64 {
        let guard = self.state.lock().unwrap();
        guard.block.end_exclusive.saturating_sub(guard.next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::Ordering;

    #[test]
    fn allocator_reserves_disjoint_blocks() {
        let a = SequenceAllocator::new(100, 10);
        let b1 = a.reserve(10);
        let b2 = a.reserve(10);
        assert_eq!(b1.start_inclusive, 100);
        assert_eq!(b1.end_exclusive, 110);
        assert_eq!(b2.start_inclusive, 110);
        assert_eq!(b2.end_exclusive, 120);
    }

    #[test]
    fn local_cache_emits_block_then_reserves_more() {
        let alloc = SequenceAllocator::new(0, 5);
        let cache = LocalSequenceCache::new(alloc.clone(), 5);
        let block0 = cache.current_block();
        assert_eq!(block0.size(), 5);
        for _ in 0..5 {
            cache.nextval().unwrap();
        }
        // Exhausted. Next call must reserve another block.
        cache.nextval().unwrap();
        let block1 = cache.current_block();
        assert!(block1.start_inclusive >= block0.end_exclusive);
    }

    #[test]
    fn concurrent_caches_never_emit_duplicates() {
        let alloc = SequenceAllocator::new(0, 100);
        let mut handles = Vec::new();
        let seen = Arc::new(std::sync::Mutex::new(HashSet::new()));
        for _ in 0..8 {
            let alloc = alloc.clone();
            let cache = LocalSequenceCache::new(alloc, 50);
            let seen = Arc::clone(&seen);
            handles.push(std::thread::spawn(move || {
                for _ in 0..200 {
                    let id = cache.nextval().unwrap();
                    assert!(seen.lock().unwrap().insert(id), "duplicate id {id}");
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let total = seen.lock().unwrap().len();
        assert_eq!(total, 8 * 200);
    }

    #[test]
    fn min_block_size_enforced() {
        let alloc = SequenceAllocator::new(0, 100);
        let block = alloc.reserve(1);
        assert_eq!(block.size(), 100, "min_block_size respected");
    }

    #[test]
    fn peek_does_not_advance() {
        let alloc = SequenceAllocator::new(50, 10);
        let p1 = alloc.peek();
        let p2 = alloc.peek();
        assert_eq!(p1, p2);
        assert_eq!(p1, 50);
        // Reserve a block -- now peek shifts.
        alloc.reserve(10);
        assert!(alloc.peek() >= p1 + 10);
        // Force the counter not to overflow.
        let _ = alloc.counter.load(Ordering::SeqCst);
    }
}
