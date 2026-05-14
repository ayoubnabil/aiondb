//! Uncommitted write intent registry.
//!
//! Every write a distributed transaction performs lays down an
//! **intent**: a tentative key→value pair tagged with the owning
//! transaction id. A reader that encounters an intent must consult the
//! intent's owning `DistributedTxnRecord` to decide what to do:
//!
//! - Owning txn committed → resolve to the value, treat as durable.
//! - Owning txn aborted → garbage-collect, skip the intent.
//! - Owning txn pending → wait, push, or abort according to priority.
//!
//! This module provides the in-memory side of that bookkeeping: a
//! [`IntentRegistry`] mapping each `(range, key)` to its current
//! [`Intent`]. The durable side lives in storage; the registry exists
//! so reads and writes on the same node can resolve intents without a
//! round-trip to disk.
//!
//! # Invariants
//!
//! - At most one pending intent per `(range, key)` per writer at a
//!   time. A second write by the same txn replaces the value but keeps
//!   the same txn id and bumps `sequence`.
//! - An intent's owning txn id is immutable. Resolution (commit/abort)
//!   removes the intent or promotes it; it never mutates the owner.
//! - The registry never reads or writes value bytes -- it tracks the
//!   metadata only.
//!
//! # Resolution outcomes
//!
//! [`IntentRegistry::resolve_committed`] removes the intent and
//! returns the captured value bytes so the caller can promote them to
//! a permanent KV pair. [`IntentRegistry::resolve_aborted`] removes
//! the intent and discards the captured bytes.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::distributed_record::DistributedTxnId;
use crate::hlc::HlcTimestamp;

/// One in-flight write intent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Intent {
    pub txn_id: DistributedTxnId,
    pub key: Vec<u8>,
    /// Tentative value bytes. `None` = tombstone intent.
    pub value: Option<Vec<u8>>,
    /// Monotonic per-key counter inside the owning transaction.
    /// Increments every time the same txn rewrites the same key.
    pub sequence: u32,
    /// HLC at which the intent was laid down. Used by reads to decide
    /// whether the intent is in their read snapshot window.
    pub written_at: HlcTimestamp,
}

/// Outcome of an `add` call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AddOutcome {
    /// The intent was inserted with the supplied data.
    Inserted(Intent),
    /// The same txn already had an intent for this key; it has been
    /// updated in place with a higher `sequence`.
    Updated(Intent),
    /// Another transaction already owns an intent for this key. The
    /// caller must arbitrate via push / wait before retrying.
    Conflict { existing: Intent },
}

/// What kind of conflict a `check_conflict` found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IntentConflict {
    /// The same txn owns the intent -- the caller can proceed (it is
    /// rewriting its own value).
    SameTxn(Intent),
    /// A different txn owns the intent. Caller must resolve before
    /// writing.
    OtherTxn(Intent),
}

/// Range identifier used to scope intents. Opaque to the registry --
/// callers pick whatever range id makes sense (matches
/// `aiondb_shard::range_descriptor::RangeId` in practice).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IntentRangeId(pub u64);

impl std::fmt::Display for IntentRangeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "range-{}", self.0)
    }
}

/// In-memory intent registry. Cheap to clone.
#[derive(Clone, Debug, Default)]
pub struct IntentRegistry {
    inner: Arc<std::sync::Mutex<RegistryInner>>,
}

#[derive(Debug, Default)]
struct RegistryInner {
    /// Primary index: `(range, key) -> Intent`.
    by_key: BTreeMap<(IntentRangeId, Vec<u8>), Intent>,
    /// Reverse index: `txn_id -> [ (range, key) ]` so resolving a txn
    /// is O(intents-owned-by-txn) instead of O(total-intents).
    by_txn: HashMap<DistributedTxnId, Vec<(IntentRangeId, Vec<u8>)>>,
}

impl IntentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or update an intent. Returns [`AddOutcome::Conflict`]
    /// when another txn already holds the key; the caller must
    /// arbitrate via push/wait before retrying.
    pub fn add(
        &self,
        range: IntentRangeId,
        key: Vec<u8>,
        value: Option<Vec<u8>>,
        txn_id: DistributedTxnId,
        written_at: HlcTimestamp,
    ) -> AddOutcome {
        let mut guard = self.lock();
        let entry_key = (range, key.clone());
        if let Some(existing) = guard.by_key.get(&entry_key) {
            if existing.txn_id != txn_id {
                return AddOutcome::Conflict {
                    existing: existing.clone(),
                };
            }
            let mut updated = existing.clone();
            updated.value = value;
            updated.sequence = updated.sequence.saturating_add(1);
            updated.written_at = written_at.max(updated.written_at);
            guard.by_key.insert(entry_key.clone(), updated.clone());
            return AddOutcome::Updated(updated);
        }
        let intent = Intent {
            txn_id,
            key,
            value,
            sequence: 1,
            written_at,
        };
        guard.by_key.insert(entry_key.clone(), intent.clone());
        guard.by_txn.entry(txn_id).or_default().push(entry_key);
        AddOutcome::Inserted(intent)
    }

    /// Look up the intent at `(range, key)`. Returns `None` if no
    /// intent exists.
    pub fn get(&self, range: IntentRangeId, key: &[u8]) -> Option<Intent> {
        self.lock().by_key.get(&(range, key.to_vec())).cloned()
    }

    /// Inspect a potential conflict without acquiring the intent. Used
    /// by readers and would-be writers to decide whether to proceed.
    pub fn check_conflict(
        &self,
        range: IntentRangeId,
        key: &[u8],
        as_txn: DistributedTxnId,
    ) -> Option<IntentConflict> {
        let guard = self.lock();
        let intent = guard.by_key.get(&(range, key.to_vec()))?;
        if intent.txn_id == as_txn {
            Some(IntentConflict::SameTxn(intent.clone()))
        } else {
            Some(IntentConflict::OtherTxn(intent.clone()))
        }
    }

    /// Resolve every intent owned by `txn_id` as committed at
    /// `commit_ts`. Returns the resolved intents in `(range, key)`
    /// order so the storage layer can promote them deterministically.
    pub fn resolve_committed(
        &self,
        txn_id: DistributedTxnId,
        commit_ts: HlcTimestamp,
    ) -> Vec<ResolvedIntent> {
        let mut guard = self.lock();
        let keys = match guard.by_txn.remove(&txn_id) {
            Some(k) => k,
            None => return Vec::new(),
        };
        let mut resolved = Vec::with_capacity(keys.len());
        for entry_key in keys {
            if let Some(intent) = guard.by_key.remove(&entry_key) {
                resolved.push(ResolvedIntent {
                    range: entry_key.0,
                    key: entry_key.1,
                    value: intent.value,
                    sequence: intent.sequence,
                    commit_ts,
                });
            }
        }
        resolved.sort_by(|a, b| (a.range, &a.key).cmp(&(b.range, &b.key)));
        resolved
    }

    /// Resolve every intent owned by `txn_id` as aborted. Returns the
    /// keys whose intents were discarded.
    pub fn resolve_aborted(&self, txn_id: DistributedTxnId) -> Vec<(IntentRangeId, Vec<u8>)> {
        let mut guard = self.lock();
        let keys = match guard.by_txn.remove(&txn_id) {
            Some(k) => k,
            None => return Vec::new(),
        };
        let mut out = Vec::with_capacity(keys.len());
        for entry_key in keys {
            if guard.by_key.remove(&entry_key).is_some() {
                out.push(entry_key);
            }
        }
        out.sort();
        out
    }

    /// List every intent owned by `txn_id`. Sorted for determinism.
    pub fn intents_for_txn(&self, txn_id: DistributedTxnId) -> Vec<Intent> {
        let guard = self.lock();
        let keys = match guard.by_txn.get(&txn_id) {
            Some(k) => k,
            None => return Vec::new(),
        };
        let mut intents: Vec<Intent> = keys
            .iter()
            .filter_map(|k| guard.by_key.get(k).cloned())
            .collect();
        intents.sort_by(|a, b| (&a.key, a.sequence).cmp(&(&b.key, b.sequence)));
        intents
    }

    /// List every intent in `[start_key, end_key)` for `range`. Used
    /// by readers performing a scan to see which keys have pending
    /// intents that need resolving.
    pub fn intents_in_range(
        &self,
        range: IntentRangeId,
        start_key: &[u8],
        end_key: &[u8],
    ) -> Vec<Intent> {
        let guard = self.lock();
        let lower = (range, start_key.to_vec());
        let upper = (range, end_key.to_vec());
        if end_key.is_empty() {
            // Empty end_key = infinity. Take all entries for this range.
            return guard
                .by_key
                .range(lower..)
                .take_while(|((r, _), _)| *r == range)
                .map(|(_, intent)| intent.clone())
                .collect();
        }
        guard
            .by_key
            .range(lower..upper)
            .map(|(_, intent)| intent.clone())
            .collect()
    }

    /// Number of in-flight intents across all txns.
    pub fn len(&self) -> usize {
        self.lock().by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().by_key.is_empty()
    }

    /// Snapshot every intent. Ordered by `(range, key)`.
    pub fn snapshot(&self) -> Vec<Intent> {
        self.lock().by_key.values().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RegistryInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Single committed intent ready to be promoted by the storage layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedIntent {
    pub range: IntentRangeId,
    pub key: Vec<u8>,
    pub value: Option<Vec<u8>>,
    pub sequence: u32,
    pub commit_ts: HlcTimestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn txn(coord: u64) -> DistributedTxnId {
        DistributedTxnId {
            coordinator: coord,
            start_ts: HlcTimestamp::new(100, 0),
            seq: 0,
        }
    }

    fn ts(wall: u64) -> HlcTimestamp {
        HlcTimestamp::new(wall, 0)
    }

    fn range(n: u64) -> IntentRangeId {
        IntentRangeId(n)
    }

    #[test]
    fn add_then_get_returns_intent() {
        let r = IntentRegistry::new();
        match r.add(
            range(1),
            b"k1".to_vec(),
            Some(b"v1".to_vec()),
            txn(1),
            ts(100),
        ) {
            AddOutcome::Inserted(intent) => {
                assert_eq!(intent.key, b"k1");
                assert_eq!(intent.sequence, 1);
            }
            other => panic!("expected Inserted, got {other:?}"),
        }
        let got = r.get(range(1), b"k1").unwrap();
        assert_eq!(got.value, Some(b"v1".to_vec()));
    }

    #[test]
    fn add_same_txn_updates_in_place_and_bumps_sequence() {
        let r = IntentRegistry::new();
        r.add(
            range(1),
            b"k1".to_vec(),
            Some(b"v1".to_vec()),
            txn(1),
            ts(100),
        );
        match r.add(
            range(1),
            b"k1".to_vec(),
            Some(b"v2".to_vec()),
            txn(1),
            ts(200),
        ) {
            AddOutcome::Updated(intent) => {
                assert_eq!(intent.value, Some(b"v2".to_vec()));
                assert_eq!(intent.sequence, 2);
                assert_eq!(intent.written_at, ts(200));
            }
            other => panic!("expected Updated, got {other:?}"),
        }
    }

    #[test]
    fn add_other_txn_returns_conflict() {
        let r = IntentRegistry::new();
        r.add(
            range(1),
            b"k1".to_vec(),
            Some(b"v1".to_vec()),
            txn(1),
            ts(100),
        );
        match r.add(
            range(1),
            b"k1".to_vec(),
            Some(b"v2".to_vec()),
            txn(2),
            ts(150),
        ) {
            AddOutcome::Conflict { existing } => {
                assert_eq!(existing.txn_id, txn(1));
                assert_eq!(existing.value, Some(b"v1".to_vec()));
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn check_conflict_distinguishes_same_vs_other_txn() {
        let r = IntentRegistry::new();
        r.add(
            range(1),
            b"k1".to_vec(),
            Some(b"v1".to_vec()),
            txn(1),
            ts(100),
        );
        match r.check_conflict(range(1), b"k1", txn(1)).unwrap() {
            IntentConflict::SameTxn(_) => {}
            other => panic!("expected SameTxn, got {other:?}"),
        }
        match r.check_conflict(range(1), b"k1", txn(2)).unwrap() {
            IntentConflict::OtherTxn(_) => {}
            other => panic!("expected OtherTxn, got {other:?}"),
        }
        assert!(r.check_conflict(range(1), b"missing", txn(1)).is_none());
    }

    #[test]
    fn resolve_committed_removes_intents_and_carries_value() {
        let r = IntentRegistry::new();
        r.add(
            range(1),
            b"k1".to_vec(),
            Some(b"v1".to_vec()),
            txn(1),
            ts(100),
        );
        r.add(range(1), b"k2".to_vec(), None, txn(1), ts(110)); // tombstone
        r.add(
            range(2),
            b"k3".to_vec(),
            Some(b"v3".to_vec()),
            txn(1),
            ts(120),
        );
        let resolved = r.resolve_committed(txn(1), ts(500));
        assert_eq!(resolved.len(), 3);
        assert_eq!(resolved[0].range, range(1));
        assert_eq!(resolved[0].key, b"k1");
        assert_eq!(resolved[0].value, Some(b"v1".to_vec()));
        assert_eq!(resolved[1].range, range(1));
        assert_eq!(resolved[1].key, b"k2");
        assert_eq!(resolved[1].value, None);
        assert_eq!(resolved[2].range, range(2));
        assert_eq!(resolved[0].commit_ts, ts(500));
        assert!(r.is_empty());
    }

    #[test]
    fn resolve_aborted_removes_intents_and_discards_value() {
        let r = IntentRegistry::new();
        r.add(
            range(1),
            b"k1".to_vec(),
            Some(b"v1".to_vec()),
            txn(1),
            ts(100),
        );
        r.add(
            range(1),
            b"k2".to_vec(),
            Some(b"v2".to_vec()),
            txn(1),
            ts(110),
        );
        let removed = r.resolve_aborted(txn(1));
        assert_eq!(removed.len(), 2);
        assert!(r.is_empty());
    }

    #[test]
    fn resolve_unknown_txn_returns_empty() {
        let r = IntentRegistry::new();
        let resolved = r.resolve_committed(txn(99), ts(500));
        assert!(resolved.is_empty());
        let aborted = r.resolve_aborted(txn(99));
        assert!(aborted.is_empty());
    }

    #[test]
    fn intents_for_txn_lists_only_that_txn() {
        let r = IntentRegistry::new();
        r.add(
            range(1),
            b"k1".to_vec(),
            Some(b"v1".to_vec()),
            txn(1),
            ts(100),
        );
        r.add(
            range(1),
            b"k2".to_vec(),
            Some(b"v2".to_vec()),
            txn(2),
            ts(105),
        );
        r.add(
            range(1),
            b"k3".to_vec(),
            Some(b"v3".to_vec()),
            txn(1),
            ts(110),
        );
        let t1_intents = r.intents_for_txn(txn(1));
        assert_eq!(t1_intents.len(), 2);
        let keys: Vec<&[u8]> = t1_intents.iter().map(|i| i.key.as_slice()).collect();
        assert_eq!(keys, vec![b"k1" as &[u8], b"k3"]);
    }

    #[test]
    fn intents_in_range_uses_half_open_bounds() {
        let r = IntentRegistry::new();
        r.add(
            range(1),
            b"a".to_vec(),
            Some(b"x".to_vec()),
            txn(1),
            ts(100),
        );
        r.add(
            range(1),
            b"e".to_vec(),
            Some(b"x".to_vec()),
            txn(1),
            ts(100),
        );
        r.add(
            range(1),
            b"m".to_vec(),
            Some(b"x".to_vec()),
            txn(1),
            ts(100),
        );
        r.add(
            range(1),
            b"z".to_vec(),
            Some(b"x".to_vec()),
            txn(1),
            ts(100),
        );
        let in_span = r.intents_in_range(range(1), b"a", b"m");
        let keys: Vec<&[u8]> = in_span.iter().map(|i| i.key.as_slice()).collect();
        assert_eq!(keys, vec![b"a" as &[u8], b"e"]);
    }

    #[test]
    fn intents_in_range_with_empty_end_means_infinity() {
        let r = IntentRegistry::new();
        r.add(
            range(1),
            b"a".to_vec(),
            Some(b"x".to_vec()),
            txn(1),
            ts(100),
        );
        r.add(
            range(1),
            b"z".to_vec(),
            Some(b"x".to_vec()),
            txn(1),
            ts(100),
        );
        r.add(
            range(2),
            b"a".to_vec(),
            Some(b"x".to_vec()),
            txn(1),
            ts(100),
        );
        let in_span = r.intents_in_range(range(1), b"a", b"");
        assert_eq!(in_span.len(), 2);
        // Range 2 must not bleed into the scan.
        let in_span2 = r.intents_in_range(range(2), b"a", b"");
        assert_eq!(in_span2.len(), 1);
    }

    #[test]
    fn snapshot_is_sorted_by_range_then_key() {
        let r = IntentRegistry::new();
        r.add(
            range(2),
            b"a".to_vec(),
            Some(b"x".to_vec()),
            txn(1),
            ts(100),
        );
        r.add(
            range(1),
            b"z".to_vec(),
            Some(b"x".to_vec()),
            txn(1),
            ts(100),
        );
        r.add(
            range(1),
            b"a".to_vec(),
            Some(b"x".to_vec()),
            txn(1),
            ts(100),
        );
        let snap = r.snapshot();
        let pairs: Vec<(u64, &[u8])> = snap
            .iter()
            .map(|i| {
                // recover the range from the by_txn ordering -- snapshot only carries the intent
                (0, i.key.as_slice())
            })
            .collect();
        // The snapshot is sorted by (range, key) at the storage level
        // but `Intent` itself does not carry the range. Probe via the
        // primary getter instead to confirm ordering.
        let _ = pairs;
        assert_eq!(snap.len(), 3);
        // Range 1 / "a" first; range 1 / "z"; then range 2 / "a".
        assert_eq!(snap[0].key, b"a");
        assert_eq!(snap[1].key, b"z");
        assert_eq!(snap[2].key, b"a");
    }
}
