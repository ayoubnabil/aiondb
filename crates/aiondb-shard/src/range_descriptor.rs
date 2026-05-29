//! Range descriptor registry: the `meta2` of `AionDB`.
//!
//! In CockroachDB, the cluster metadata layer is a special bootstrapped
//! range called `meta2` that maps the *start key* of every range to a
//! [`RangeDescriptor`] describing which replicas serve it. Routing a
//! key to its owning range becomes a single in-memory `BTreeMap::range`
//! lookup: find the largest start_key ≤ the requested key, that's the
//! owning range.
//!
//! This is the in-memory analog. It is intentionally not durable -- the
//! higher layer loads descriptors from the catalog at startup and
//! persists mutations through Raft. The registry only maintains the live
//! in-memory index used by the SQL coordinator and the shard router.
//!
//! # Key model
//!
//! Keys are opaque byte strings (`Vec<u8>`). The registry treats them
//! as a sorted dictionary, identical to how CockroachDB's KV layer
//! treats range boundaries. A `[start_key, end_key)` half-open interval
//! avoids the off-by-one ambiguity that closed-on-both-ends ranges
//! invite.
//!
//! ## Invariants
//!
//! - Ranges in the registry are non-overlapping.
//! - Every range has `start_key < end_key`. The sentinel `end_key`
//!   for the last range is the empty byte string `b""` interpreted as
//!   "infinity" (matching CockroachDB's `KeyMax`).
//! - Descriptors are uniquely identified by [`RangeId`]; the registry
//!   panics if two distinct ids claim overlapping key ranges.
//! - Every mutation bumps the descriptor's `generation`, allowing
//!   stale routers to detect they have observed an out-of-date
//!   placement.
//!
//! # Operations
//!
//! - `upsert` -- insert or replace a single descriptor. Bumps
//!   generation if a previous descriptor for that range_id existed.
//! - `lookup(key)` -- find the range that contains a key.
//! - `lookup_range(start, end)` -- find every range that overlaps a
//!   user key span. Used by scans that may straddle multiple ranges.
//! - `split(range_id, split_key)` -- atomically replace one range with
//!   two halves sharing the same replica set.
//! - `remove(range_id)` -- drop a descriptor (range is being
//!   merged or decommissioned).

use std::collections::BTreeMap;
use std::sync::Arc;

use aiondb_core::{DbError, DbResult};

use crate::lease::Lease;
use crate::ShardId;

/// Sentinel for "infinity" upper bound (matches CockroachDB `KeyMax`).
pub const KEY_MAX: &[u8] = b"";

/// Range identifier. Distinct from [`ShardId`] because one shard may
/// host multiple ranges in a key-range partitioned design.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RangeId(u64);

impl RangeId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for RangeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "range-{}", self.0)
    }
}

/// Identifier of a single replica within a range. Replicas are members
/// of a Raft group; the leaseholder is one of them.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReplicaId(u64);

impl ReplicaId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Single replica entry in a [`RangeDescriptor`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplicaDescriptor {
    pub replica_id: ReplicaId,
    pub node_id: String,
    /// `true` when this replica is still a learner -- counts toward
    /// raft state replication but not toward quorum.
    pub is_learner: bool,
}

/// Cluster's view of a single key range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RangeDescriptor {
    pub range_id: RangeId,
    /// Inclusive start of the range's key span.
    pub start_key: Vec<u8>,
    /// Exclusive end. Empty byte string means "infinity".
    pub end_key: Vec<u8>,
    pub replicas: Vec<ReplicaDescriptor>,
    /// Logical shard that owns this range's data. A many-to-one mapping
    /// from range to shard is supported so the storage layer can host
    /// multiple ranges per shard while the registry keeps fine-grained
    /// routing.
    pub shard: ShardId,
    /// Current lease snapshot, if known.
    pub lease: Option<Lease>,
    /// Bumped on every mutation. Stale routers compare against this
    /// to know they need to refresh their cache.
    pub generation: u64,
}

impl RangeDescriptor {
    /// `true` when the range contains `key` according to the half-open
    /// interval `[start_key, end_key)`. The empty `end_key` means
    /// "infinity" and matches every key ≥ `start_key`.
    pub fn contains(&self, key: &[u8]) -> bool {
        if key < self.start_key.as_slice() {
            return false;
        }
        if self.end_key.is_empty() {
            return true;
        }
        key < self.end_key.as_slice()
    }
}

/// Range descriptor registry. Cheap to clone.
#[derive(Clone, Debug, Default)]
pub struct RangeDescriptorRegistry {
    inner: Arc<std::sync::Mutex<RegistryInner>>,
}

#[derive(Debug, Default)]
struct RegistryInner {
    /// `start_key -> RangeDescriptor`. BTreeMap so we can do
    /// `range(..=key).next_back()` to find the owning range.
    by_start_key: BTreeMap<Vec<u8>, RangeDescriptor>,
    /// `range_id -> start_key` reverse index for `O(log n)` lookups
    /// by id.
    by_id: BTreeMap<RangeId, Vec<u8>>,
}

impl RangeDescriptorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Upsert a range descriptor. Replaces any existing descriptor for
    /// the same `range_id` and bumps its `generation`. Errors out when
    /// the new descriptor would overlap with another distinct range.
    ///
    /// # Errors
    /// Returns `DbError::internal` when:
    /// - `start_key >= end_key` (with empty end_key treated as
    ///   infinity).
    /// - The descriptor overlaps with a different existing range.
    pub fn upsert(&self, mut descriptor: RangeDescriptor) -> DbResult<()> {
        validate_bounds(&descriptor)?;

        let mut guard = self.lock();

        // Drop the old slot for this range_id, if present, so we can
        // re-insert at the new start_key without overlap. Preserve
        // its `generation` so we can ensure the upsert is monotonic
        // even when the caller passed a fresh `descriptor` with
        // generation = 0.
        let mut prior_generation = 0;
        if let Some(old_start_key) = guard.by_id.remove(&descriptor.range_id) {
            if let Some(old) = guard.by_start_key.remove(&old_start_key) {
                prior_generation = old.generation;
            }
        }

        // Overlap check vs every neighbouring range.
        let conflict: Option<RangeId> = guard
            .by_start_key
            .iter()
            .find(|(_, existing)| {
                existing.range_id != descriptor.range_id
                    && overlaps(
                        &descriptor.start_key,
                        &descriptor.end_key,
                        &existing.start_key,
                        &existing.end_key,
                    )
            })
            .map(|(_, existing)| existing.range_id);

        if let Some(other_id) = conflict {
            return Err(DbError::internal(format!(
                "range descriptor for {} overlaps existing {}",
                descriptor.range_id, other_id,
            )));
        }

        descriptor.generation = descriptor
            .generation
            .max(prior_generation)
            .saturating_add(1);
        let start_key = descriptor.start_key.clone();
        guard.by_id.insert(descriptor.range_id, start_key.clone());
        guard.by_start_key.insert(start_key, descriptor);
        Ok(())
    }

    /// Look up the range that owns `key`. Returns `None` when the
    /// registry has no descriptor covering it.
    pub fn lookup(&self, key: &[u8]) -> Option<RangeDescriptor> {
        let guard = self.lock();
        let (_, candidate) = guard.by_start_key.range(..=key.to_vec()).next_back()?;
        if candidate.contains(key) {
            Some(candidate.clone())
        } else {
            None
        }
    }

    /// Look up every range that overlaps `[start, end)`. Returned in
    /// `start_key` order. Empty `end` means "infinity".
    pub fn lookup_range(&self, start: &[u8], end: &[u8]) -> Vec<RangeDescriptor> {
        let guard = self.lock();
        guard
            .by_start_key
            .iter()
            .filter(|(_, descriptor)| {
                overlaps(&descriptor.start_key, &descriptor.end_key, start, end)
            })
            .map(|(_, descriptor)| descriptor.clone())
            .collect()
    }

    /// Get a descriptor by id.
    pub fn get(&self, range_id: RangeId) -> Option<RangeDescriptor> {
        let guard = self.lock();
        let start_key = guard.by_id.get(&range_id)?;
        guard.by_start_key.get(start_key).cloned()
    }

    /// Split `range_id` at `split_key`. The left half keeps the
    /// original `range_id`; the right half receives `new_range_id`.
    /// Both halves inherit the replica set, shard binding and lease of
    /// the parent.
    ///
    /// # Errors
    /// Returns `DbError::internal` when:
    /// - `range_id` does not exist.
    /// - `split_key` is not strictly inside the parent range
    ///   (i.e. `parent.start_key < split_key < parent.end_key`).
    pub fn split(
        &self,
        range_id: RangeId,
        new_range_id: RangeId,
        split_key: Vec<u8>,
    ) -> DbResult<(RangeDescriptor, RangeDescriptor)> {
        let parent = self
            .get(range_id)
            .ok_or_else(|| DbError::internal(format!("range {range_id} not found")))?;
        if split_key <= parent.start_key {
            return Err(DbError::internal(format!(
                "split_key must be strictly greater than parent start_key for {range_id}"
            )));
        }
        if !parent.end_key.is_empty() && split_key >= parent.end_key {
            return Err(DbError::internal(format!(
                "split_key must be strictly less than parent end_key for {range_id}"
            )));
        }
        if self.get(new_range_id).is_some() {
            return Err(DbError::internal(format!(
                "new range id {new_range_id} already exists"
            )));
        }

        let left = RangeDescriptor {
            range_id,
            start_key: parent.start_key.clone(),
            end_key: split_key.clone(),
            replicas: parent.replicas.clone(),
            shard: parent.shard,
            lease: parent.lease,
            // upsert() will bump generation, so start one below the
            // parent's value to keep the post-upsert generation
            // strictly monotonic vs the parent's previous value.
            generation: parent.generation,
        };
        let right = RangeDescriptor {
            range_id: new_range_id,
            start_key: split_key,
            end_key: parent.end_key.clone(),
            replicas: parent.replicas.clone(),
            shard: parent.shard,
            lease: parent.lease,
            generation: 0,
        };

        // Remove the parent first so the upserts don't conflict.
        self.remove(range_id);
        self.upsert(left.clone())?;
        self.upsert(right.clone())?;

        // Re-load to capture the post-upsert generations.
        let left = self.get(range_id).expect("left re-loaded");
        let right = self.get(new_range_id).expect("right re-loaded");
        Ok((left, right))
    }

    /// Merge two adjacent ranges into one. `left` retains its id and
    /// absorbs `right`'s key span. `right` is removed.
    ///
    /// # Errors
    /// Returns `DbError::internal` when either range is missing or
    /// when `left.end_key != right.start_key`.
    pub fn merge(&self, left_id: RangeId, right_id: RangeId) -> DbResult<RangeDescriptor> {
        let left = self
            .get(left_id)
            .ok_or_else(|| DbError::internal(format!("range {left_id} not found")))?;
        let right = self
            .get(right_id)
            .ok_or_else(|| DbError::internal(format!("range {right_id} not found")))?;
        if left.end_key != right.start_key {
            return Err(DbError::internal(format!(
                "{left_id} and {right_id} are not adjacent: left.end_key != right.start_key"
            )));
        }
        let merged = RangeDescriptor {
            range_id: left_id,
            start_key: left.start_key.clone(),
            end_key: right.end_key.clone(),
            replicas: left.replicas.clone(),
            shard: left.shard,
            lease: left.lease,
            generation: left.generation.max(right.generation),
        };
        self.remove(left_id);
        self.remove(right_id);
        self.upsert(merged.clone())?;
        Ok(self.get(left_id).expect("merged re-loaded"))
    }

    /// Remove a descriptor by id. Returns the previous value.
    pub fn remove(&self, range_id: RangeId) -> Option<RangeDescriptor> {
        let mut guard = self.lock();
        let start_key = guard.by_id.remove(&range_id)?;
        guard.by_start_key.remove(&start_key)
    }

    /// Snapshot every descriptor, ordered by start_key.
    pub fn snapshot(&self) -> Vec<RangeDescriptor> {
        self.lock().by_start_key.values().cloned().collect()
    }

    /// Number of ranges currently tracked.
    pub fn len(&self) -> usize {
        self.lock().by_start_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().by_start_key.is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RegistryInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn validate_bounds(descriptor: &RangeDescriptor) -> DbResult<()> {
    if descriptor.end_key.is_empty() {
        return Ok(()); // empty end_key = infinity, always ok.
    }
    if descriptor.start_key >= descriptor.end_key {
        return Err(DbError::internal(format!(
            "range {} has start_key >= end_key",
            descriptor.range_id
        )));
    }
    Ok(())
}

fn overlaps(a_start: &[u8], a_end: &[u8], b_start: &[u8], b_end: &[u8]) -> bool {
    // Two half-open intervals overlap iff a_start < b_end AND b_start < a_end.
    // Treat empty `end` as +infinity.
    let a_start_lt_b_end = b_end.is_empty() || a_start < b_end;
    let b_start_lt_a_end = a_end.is_empty() || b_start < a_end;
    a_start_lt_b_end && b_start_lt_a_end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(range_id: u64, start: &[u8], end: &[u8]) -> RangeDescriptor {
        RangeDescriptor {
            range_id: RangeId::new(range_id),
            start_key: start.to_vec(),
            end_key: end.to_vec(),
            replicas: vec![ReplicaDescriptor {
                replica_id: ReplicaId::new(1),
                node_id: "node-a".to_owned(),
                is_learner: false,
            }],
            shard: ShardId::new(0),
            lease: None,
            generation: 0,
        }
    }

    #[test]
    fn lookup_returns_none_on_empty_registry() {
        let r = RangeDescriptorRegistry::new();
        assert!(r.lookup(b"foo").is_none());
        assert!(r.is_empty());
    }

    #[test]
    fn upsert_then_lookup_finds_range() {
        let r = RangeDescriptorRegistry::new();
        r.upsert(descriptor(1, b"a", b"m")).unwrap();
        let found = r.lookup(b"f").expect("range covers f");
        assert_eq!(found.range_id, RangeId::new(1));
        assert_eq!(found.generation, 1, "generation bumped on first upsert");
    }

    #[test]
    fn lookup_uses_half_open_semantics() {
        let r = RangeDescriptorRegistry::new();
        r.upsert(descriptor(1, b"a", b"m")).unwrap();
        assert!(r.lookup(b"a").is_some(), "start_key is inclusive");
        assert!(r.lookup(b"l").is_some());
        assert!(r.lookup(b"m").is_none(), "end_key is exclusive");
    }

    #[test]
    fn empty_end_key_is_infinity() {
        let r = RangeDescriptorRegistry::new();
        r.upsert(descriptor(1, b"a", b"")).unwrap();
        assert!(r.lookup(b"a").is_some());
        assert!(r.lookup(b"zzzzz").is_some());
        assert!(r.lookup(b"\xff").is_some());
    }

    #[test]
    fn overlap_is_rejected() {
        let r = RangeDescriptorRegistry::new();
        r.upsert(descriptor(1, b"a", b"m")).unwrap();
        let err = r.upsert(descriptor(2, b"h", b"z"));
        assert!(err.is_err(), "overlap must be rejected");
    }

    #[test]
    fn upsert_with_same_id_replaces_in_place() {
        let r = RangeDescriptorRegistry::new();
        r.upsert(descriptor(1, b"a", b"m")).unwrap();
        // Update with a wider span -- allowed because the old slot is
        // dropped before the overlap check runs.
        r.upsert(descriptor(1, b"a", b"z")).unwrap();
        let found = r.get(RangeId::new(1)).unwrap();
        assert_eq!(found.end_key, b"z".to_vec());
        assert!(found.generation >= 2);
    }

    #[test]
    fn split_produces_two_adjacent_ranges() {
        let r = RangeDescriptorRegistry::new();
        r.upsert(descriptor(1, b"a", b"z")).unwrap();
        let (left, right) = r
            .split(RangeId::new(1), RangeId::new(2), b"m".to_vec())
            .unwrap();
        assert_eq!(left.range_id, RangeId::new(1));
        assert_eq!(left.start_key, b"a".to_vec());
        assert_eq!(left.end_key, b"m".to_vec());
        assert_eq!(right.range_id, RangeId::new(2));
        assert_eq!(right.start_key, b"m".to_vec());
        assert_eq!(right.end_key, b"z".to_vec());
        // Lookups still cover the full key span.
        assert_eq!(r.lookup(b"c").unwrap().range_id, RangeId::new(1));
        assert_eq!(r.lookup(b"p").unwrap().range_id, RangeId::new(2));
    }

    #[test]
    fn split_rejects_out_of_range_split_key() {
        let r = RangeDescriptorRegistry::new();
        r.upsert(descriptor(1, b"a", b"m")).unwrap();
        assert!(r
            .split(RangeId::new(1), RangeId::new(2), b"z".to_vec())
            .is_err());
        assert!(r
            .split(RangeId::new(1), RangeId::new(2), b"a".to_vec())
            .is_err());
    }

    #[test]
    fn merge_combines_adjacent_ranges() {
        let r = RangeDescriptorRegistry::new();
        r.upsert(descriptor(1, b"a", b"m")).unwrap();
        r.upsert(descriptor(2, b"m", b"z")).unwrap();
        let merged = r.merge(RangeId::new(1), RangeId::new(2)).unwrap();
        assert_eq!(merged.range_id, RangeId::new(1));
        assert_eq!(merged.start_key, b"a".to_vec());
        assert_eq!(merged.end_key, b"z".to_vec());
        assert!(r.get(RangeId::new(2)).is_none(), "right is consumed");
        assert_eq!(r.lookup(b"p").unwrap().range_id, RangeId::new(1));
    }

    #[test]
    fn merge_rejects_non_adjacent_ranges() {
        let r = RangeDescriptorRegistry::new();
        r.upsert(descriptor(1, b"a", b"m")).unwrap();
        r.upsert(descriptor(2, b"n", b"z")).unwrap();
        assert!(r.merge(RangeId::new(1), RangeId::new(2)).is_err());
    }

    #[test]
    fn lookup_range_returns_all_overlapping() {
        let r = RangeDescriptorRegistry::new();
        r.upsert(descriptor(1, b"a", b"e")).unwrap();
        r.upsert(descriptor(2, b"e", b"j")).unwrap();
        r.upsert(descriptor(3, b"j", b"z")).unwrap();
        let touched = r.lookup_range(b"c", b"k");
        let ids: Vec<u64> = touched.iter().map(|d| d.range_id.get()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn lookup_range_with_infinity_end() {
        let r = RangeDescriptorRegistry::new();
        r.upsert(descriptor(1, b"a", b"m")).unwrap();
        r.upsert(descriptor(2, b"m", b"")).unwrap();
        let touched = r.lookup_range(b"q", b"");
        assert_eq!(touched.len(), 1);
        assert_eq!(touched[0].range_id, RangeId::new(2));
    }

    #[test]
    fn remove_drops_descriptor() {
        let r = RangeDescriptorRegistry::new();
        r.upsert(descriptor(1, b"a", b"z")).unwrap();
        assert!(r.remove(RangeId::new(1)).is_some());
        assert!(r.lookup(b"a").is_none());
        assert!(r.is_empty());
    }

    #[test]
    fn snapshot_is_sorted_by_start_key() {
        let r = RangeDescriptorRegistry::new();
        r.upsert(descriptor(3, b"j", b"z")).unwrap();
        r.upsert(descriptor(1, b"a", b"e")).unwrap();
        r.upsert(descriptor(2, b"e", b"j")).unwrap();
        let snap = r.snapshot();
        let starts: Vec<&[u8]> = snap.iter().map(|d| d.start_key.as_slice()).collect();
        assert_eq!(starts, vec![b"a" as &[u8], b"e", b"j"]);
    }
}
