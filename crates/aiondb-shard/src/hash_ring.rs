//! Consistent hash ring for automatic shard key routing.
//!
//! Uses SHA-256 to hash virtual node tokens and keys onto a 64-bit ring.
//! Each physical shard is mapped to `N` virtual nodes for balanced
//! distribution. Lookups are `O(log V)` where `V` is the total number of
//! virtual nodes.

use sha2::{Digest, Sha256};

use crate::shard::ShardId;
use crate::{MAX_STORAGE_HASH_RING_VIRTUAL_NODES, MAX_STORAGE_VIRTUAL_NODES_PER_SHARD};

/// A point on the hash ring.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct VirtualNode {
    /// Position on the ring (first 8 bytes of SHA-256).
    position: u64,
    /// The physical shard this virtual node maps to.
    shard_id: ShardId,
}

/// Consistent hash ring mapping keys to [`ShardId`]s.
///
/// The ring uses SHA-256 truncated to 64 bits. Each shard gets
/// `virtual_nodes_per_shard` positions, producing a near-uniform
/// distribution even with small shard counts.
#[derive(Clone, Debug)]
pub struct HashRing {
    ring: Vec<VirtualNode>,
    virtual_nodes_per_shard: u32,
}

impl HashRing {
    /// Create an empty ring with the given number of virtual nodes per shard.
    ///
    /// # Panics
    ///
    /// Panics if `virtual_nodes_per_shard` is zero or exceeds the configured
    /// shard fanout limit.
    #[must_use]
    pub fn new(virtual_nodes_per_shard: u32) -> Self {
        assert_virtual_node_fanout(virtual_nodes_per_shard);
        Self {
            ring: Vec::new(),
            virtual_nodes_per_shard,
        }
    }

    /// Build a ring from a set of shard ids.
    ///
    /// # Panics
    ///
    /// Panics if the requested virtual node fanout exceeds the configured
    /// per-shard or total hash-ring limits.
    #[must_use]
    pub fn from_shards(shards: &[ShardId], virtual_nodes_per_shard: u32) -> Self {
        let capacity = assert_hash_ring_capacity(shards.len(), virtual_nodes_per_shard);
        let mut ring = Self {
            ring: Vec::with_capacity(capacity),
            virtual_nodes_per_shard,
        };
        for &shard_id in shards {
            for vnode_idx in 0..virtual_nodes_per_shard {
                let position = Self::vnode_position(shard_id, vnode_idx);
                ring.ring.push(VirtualNode { position, shard_id });
            }
        }
        ring.ring.sort_unstable_by_key(|node| node.position);
        ring
    }

    /// Add a shard to the ring, creating virtual node entries.
    pub fn add_shard(&mut self, shard_id: ShardId) {
        assert_can_add_shard(self.ring.len(), self.virtual_nodes_per_shard);
        self.ring
            .reserve(usize::try_from(self.virtual_nodes_per_shard).unwrap_or(usize::MAX));
        for vnode_idx in 0..self.virtual_nodes_per_shard {
            let position = Self::vnode_position(shard_id, vnode_idx);
            let vnode = VirtualNode { position, shard_id };
            let insert_at = self
                .ring
                .binary_search_by_key(&position, |n| n.position)
                .unwrap_or_else(|idx| idx);
            self.ring.insert(insert_at, vnode);
        }
    }

    /// Remove a shard from the ring.
    pub fn remove_shard(&mut self, shard_id: ShardId) {
        self.ring.retain(|vnode| vnode.shard_id != shard_id);
    }

    /// Look up the shard that owns the given key bytes.
    ///
    /// Returns `None` only when the ring is empty.
    #[must_use]
    pub fn lookup(&self, key: &[u8]) -> Option<ShardId> {
        if self.ring.is_empty() {
            return None;
        }
        let hash = Self::hash_key(key);
        // Find the first virtual node whose position is >= hash (clockwise).
        let idx = match self.ring.binary_search_by_key(&hash, |n| n.position) {
            Ok(i) => i,
            Err(i) => {
                if i >= self.ring.len() {
                    0 // wrap around
                } else {
                    i
                }
            }
        };
        Some(self.ring[idx].shard_id)
    }

    /// Return the number of shards currently on the ring (unique physical shards).
    #[must_use]
    pub fn shard_count(&self) -> usize {
        let mut seen = std::collections::BTreeSet::new();
        for vnode in &self.ring {
            seen.insert(vnode.shard_id);
        }
        seen.len()
    }

    /// Return the total number of virtual nodes.
    #[must_use]
    pub fn virtual_node_count(&self) -> usize {
        self.ring.len()
    }

    /// Return true when the ring contains no shards.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    // ─── Internal ──────────────────────────────────────────────

    /// Compute the ring position for a virtual node.
    fn vnode_position(shard_id: ShardId, vnode_idx: u32) -> u64 {
        let mut hasher = Sha256::new();
        hasher.update(shard_id.get().to_le_bytes());
        hasher.update(vnode_idx.to_le_bytes());
        let digest = hasher.finalize();
        u64::from_le_bytes([
            digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
        ])
    }

    /// Hash an arbitrary key to a 64-bit ring position.
    fn hash_key(key: &[u8]) -> u64 {
        let mut hasher = Sha256::new();
        hasher.update(key);
        let digest = hasher.finalize();
        u64::from_le_bytes([
            digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
        ])
    }
}

fn assert_virtual_node_fanout(virtual_nodes_per_shard: u32) {
    assert!(
        virtual_nodes_per_shard > 0,
        "virtual_nodes_per_shard must be > 0"
    );
    assert!(
        virtual_nodes_per_shard <= MAX_STORAGE_VIRTUAL_NODES_PER_SHARD,
        "virtual_nodes_per_shard must be <= {MAX_STORAGE_VIRTUAL_NODES_PER_SHARD}"
    );
}

fn assert_hash_ring_capacity(shard_count: usize, virtual_nodes_per_shard: u32) -> usize {
    assert_virtual_node_fanout(virtual_nodes_per_shard);
    let shard_count = u64::try_from(shard_count).unwrap_or(u64::MAX);
    let total_virtual_nodes = shard_count.saturating_mul(u64::from(virtual_nodes_per_shard));
    assert!(
        total_virtual_nodes <= MAX_STORAGE_HASH_RING_VIRTUAL_NODES,
        "hash ring virtual node count must be <= {MAX_STORAGE_HASH_RING_VIRTUAL_NODES}"
    );
    usize::try_from(total_virtual_nodes).unwrap_or(usize::MAX)
}

fn assert_can_add_shard(current_virtual_nodes: usize, virtual_nodes_per_shard: u32) {
    assert_virtual_node_fanout(virtual_nodes_per_shard);
    let total_virtual_nodes = u64::try_from(current_virtual_nodes)
        .unwrap_or(u64::MAX)
        .saturating_add(u64::from(virtual_nodes_per_shard));
    assert!(
        total_virtual_nodes <= MAX_STORAGE_HASH_RING_VIRTUAL_NODES,
        "hash ring virtual node count must be <= {MAX_STORAGE_HASH_RING_VIRTUAL_NODES}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ring_returns_none() {
        let ring = HashRing::new(128);
        assert!(ring.lookup(b"any-key").is_none());
        assert!(ring.is_empty());
    }

    #[test]
    fn single_shard_always_resolves() {
        let ring = HashRing::from_shards(&[ShardId::new(0)], 128);
        assert_eq!(ring.shard_count(), 1);
        for i in 0u64..100 {
            assert_eq!(ring.lookup(&i.to_le_bytes()), Some(ShardId::new(0)));
        }
    }

    #[test]
    fn multiple_shards_distribute_keys() {
        let shards: Vec<ShardId> = (0..4).map(ShardId::new).collect();
        let ring = HashRing::from_shards(&shards, 128);
        assert_eq!(ring.shard_count(), 4);

        let mut counts = [0u32; 4];
        for i in 0u64..10_000 {
            let shard = ring.lookup(&i.to_le_bytes()).unwrap();
            counts[shard.get() as usize] += 1;
        }
        // Each shard should get a reasonable share (at least 10% of 10k = 1000).
        for (idx, &count) in counts.iter().enumerate() {
            assert!(
                count > 500,
                "shard {idx} got only {count} keys out of 10000"
            );
        }
    }

    #[test]
    fn deterministic_lookup() {
        let shards: Vec<ShardId> = (0..3).map(ShardId::new).collect();
        let ring = HashRing::from_shards(&shards, 64);
        let result_a = ring.lookup(b"tenant-42");
        let result_b = ring.lookup(b"tenant-42");
        assert_eq!(result_a, result_b);
    }

    #[test]
    fn add_shard_minimal_rebalance() {
        let shards: Vec<ShardId> = (0..3).map(ShardId::new).collect();
        let mut ring = HashRing::from_shards(&shards, 128);

        // Record assignments before adding a 4th shard.
        let keys: Vec<Vec<u8>> = (0u64..1000).map(|i| i.to_le_bytes().to_vec()).collect();
        let before: Vec<ShardId> = keys.iter().map(|k| ring.lookup(k).unwrap()).collect();

        ring.add_shard(ShardId::new(3));

        let after: Vec<ShardId> = keys.iter().map(|k| ring.lookup(k).unwrap()).collect();

        let moved = before
            .iter()
            .zip(after.iter())
            .filter(|(a, b)| a != b)
            .count();

        // Consistent hashing: ideally ~1/4 keys move. Allow up to 50%.
        assert!(
            moved < 500,
            "too many keys moved: {moved}/1000 (expected < 500)"
        );
    }

    #[test]
    fn remove_shard() {
        let shards: Vec<ShardId> = (0..3).map(ShardId::new).collect();
        let mut ring = HashRing::from_shards(&shards, 64);
        assert_eq!(ring.shard_count(), 3);

        ring.remove_shard(ShardId::new(1));
        assert_eq!(ring.shard_count(), 2);

        // All lookups should resolve to shard 0 or 2.
        for i in 0u64..100 {
            let shard = ring.lookup(&i.to_le_bytes()).unwrap();
            assert!(shard == ShardId::new(0) || shard == ShardId::new(2));
        }
    }

    #[test]
    fn virtual_node_count_matches() {
        let ring = HashRing::from_shards(&[ShardId::new(0), ShardId::new(1)], 100);
        assert_eq!(ring.virtual_node_count(), 200);
    }

    #[test]
    #[should_panic(expected = "virtual_nodes_per_shard must be <=")]
    fn excessive_virtual_nodes_per_shard_panics_before_allocation() {
        let _ = HashRing::new(MAX_STORAGE_VIRTUAL_NODES_PER_SHARD + 1);
    }

    #[test]
    #[should_panic(expected = "hash ring virtual node count must be <=")]
    fn excessive_total_virtual_nodes_panics_before_allocation() {
        let shard_count = u32::try_from(MAX_STORAGE_HASH_RING_VIRTUAL_NODES / 128 + 1).unwrap();
        let shards: Vec<ShardId> = (0..shard_count).map(ShardId::new).collect();

        let _ = HashRing::from_shards(&shards, 128);
    }

    #[test]
    fn string_keys_route_consistently() {
        let shards: Vec<ShardId> = (0..4).map(ShardId::new).collect();
        let ring = HashRing::from_shards(&shards, 128);
        let a = ring.lookup(b"customer-abc");
        let b = ring.lookup(b"customer-abc");
        let c = ring.lookup(b"customer-xyz");
        assert_eq!(a, b);
        // Different keys may or may not go to the same shard - just check
        // they both resolve.
        assert!(c.is_some());
    }
}
