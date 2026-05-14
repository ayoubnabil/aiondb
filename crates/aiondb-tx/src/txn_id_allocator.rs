//! Cluster-unique transaction ID allocator.
//!
//! Each node has a 16-bit prefix encoded into the high bits of the
//! 64-bit txn id. The low 48 bits come from a monotonic local
//! counter. With this scheme any node can mint a globally unique
//! id without coordinating with the cluster.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct TxnIdAllocator {
    node_prefix: u64,
    counter: Arc<AtomicU64>,
}

impl TxnIdAllocator {
    pub fn new(node_id: u16) -> Self {
        Self {
            node_prefix: (node_id as u64) << 48,
            counter: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn allocate(&self) -> u64 {
        let local = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        debug_assert!(local < (1u64 << 48));
        self.node_prefix | (local & 0x0000_FFFF_FFFF_FFFF)
    }

    pub fn install_floor(&self, lower_bound: u64) {
        let mut cur = self.counter.load(Ordering::Relaxed);
        loop {
            if lower_bound <= cur {
                return;
            }
            match self.counter.compare_exchange(
                cur,
                lower_bound,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(actual) => cur = actual,
            }
        }
    }

    pub fn extract_node(id: u64) -> u16 {
        ((id >> 48) & 0xFFFF) as u16
    }

    pub fn extract_sequence(id: u64) -> u64 {
        id & 0x0000_FFFF_FFFF_FFFF
    }

    pub fn local_high_water(&self) -> u64 {
        self.counter.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_monotonic_increasing() {
        let a = TxnIdAllocator::new(7);
        let id1 = a.allocate();
        let id2 = a.allocate();
        assert!(id2 > id1);
    }

    #[test]
    fn node_prefix_encoded() {
        let a = TxnIdAllocator::new(0xABCD);
        let id = a.allocate();
        assert_eq!(TxnIdAllocator::extract_node(id), 0xABCD);
    }

    #[test]
    fn sequence_starts_at_one() {
        let a = TxnIdAllocator::new(0);
        let id = a.allocate();
        assert_eq!(TxnIdAllocator::extract_sequence(id), 1);
    }

    #[test]
    fn distinct_nodes_distinct_ids() {
        let a = TxnIdAllocator::new(1);
        let b = TxnIdAllocator::new(2);
        let ia = a.allocate();
        let ib = b.allocate();
        assert_ne!(ia, ib);
        assert_eq!(
            TxnIdAllocator::extract_sequence(ia),
            TxnIdAllocator::extract_sequence(ib)
        );
    }

    #[test]
    fn install_floor_only_advances() {
        let a = TxnIdAllocator::new(0);
        a.allocate();
        a.allocate();
        a.install_floor(100);
        let id = a.allocate();
        assert_eq!(TxnIdAllocator::extract_sequence(id), 101);
    }

    #[test]
    fn install_floor_below_current_noop() {
        let a = TxnIdAllocator::new(0);
        a.install_floor(100);
        a.install_floor(5);
        assert_eq!(a.local_high_water(), 100);
    }

    #[test]
    fn thread_safe_allocation() {
        use std::sync::Arc as Sa;
        let a = Sa::new(TxnIdAllocator::new(1));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let a = a.clone();
            handles.push(std::thread::spawn(move || {
                let mut ids = Vec::new();
                for _ in 0..100 {
                    ids.push(a.allocate());
                }
                ids
            }));
        }
        let mut all: Vec<u64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 800);
    }
}
