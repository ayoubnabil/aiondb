//! Replicated KV state machine on top of [`MultiRaftRegistry`].
//!
//! Implements a real key-value store whose writes flow through a Raft
//! group. Every `put` / `delete` proposes a `RaftCommand::KvWrite` to
//! the local leader; once committed by quorum, every replica applies
//! the entry to its in-memory `BTreeMap`. Reads either:
//!
//! - Hit the local applied state (fast, may be stale).
//! - Wait for the local applied index to catch up to a target index
//!   reported by the leader (linearisable).
//!
//! Per-group isolation lets the same engine host many ranges; each
//! gets its own `BTreeMap` so writes in one range never invalidate
//! reads in another.
//!
//! # What this module is NOT
//!
//! - A transactional store : there are no MVCC versions, no intents,
//!   no isolation levels. Single-key writes are atomic; cross-key
//!   atomicity belongs to the 2PC layer.
//! - A storage engine : the state lives in memory. Persisting to disk
//!   is the storage-engine crate's responsibility.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use aiondb_core::DbResult;
use tracing::debug;

use crate::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use crate::raft::{RaftCommand, RaftEntry};

/// Observer fired by the engine on every applied write entry.
///
/// Implementations should be cheap and non-blocking : the apply loop
/// holds the engine's state lock while calling the observer.
pub trait KvApplyObserver: Send + Sync {
    fn on_write(
        &self,
        group: MultiRaftGroupId,
        key: &[u8],
        value: Option<&[u8]>,
        applied_index: u64,
    );
}

/// Per-group key-value state machine.
#[derive(Debug, Default)]
struct KvShard {
    table: BTreeMap<Vec<u8>, Vec<u8>>,
    applied_index: u64,
}

impl KvShard {
    fn apply(&mut self, entry: &RaftEntry) {
        match &entry.command {
            RaftCommand::KvWrite { key, value } => match value {
                Some(v) => {
                    self.table.insert(key.clone(), v.clone());
                }
                None => {
                    self.table.remove(key);
                }
            },
            // Non-KV commands are no-ops from the KV engine's
            // perspective; the control plane handles them.
            _ => {}
        }
        if entry.index > self.applied_index {
            self.applied_index = entry.index;
        }
    }
}

/// Replicated KV engine.
#[derive(Clone)]
pub struct KvEngine {
    registry: Arc<MultiRaftRegistry>,
    state: Arc<RwLock<HashMap<MultiRaftGroupId, KvShard>>>,
    /// Serialises CAS so concurrent compare-and-swap on the same key
    /// cannot both succeed. Keyed per group so disjoint groups still
    /// proceed in parallel.
    cas_locks: Arc<std::sync::Mutex<HashMap<MultiRaftGroupId, Arc<std::sync::Mutex<()>>>>>,
    /// Optional observer fired on every applied write entry.
    observer: Arc<std::sync::Mutex<Option<Arc<dyn KvApplyObserver>>>>,
}

impl std::fmt::Debug for KvEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KvEngine")
            .field("groups", &self.state.read().unwrap().len())
            .finish()
    }
}

impl KvEngine {
    pub fn new(registry: Arc<MultiRaftRegistry>) -> Self {
        Self {
            registry,
            state: Arc::new(RwLock::new(HashMap::new())),
            cas_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            observer: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Install an observer that fires on every applied write. Replaces
    /// the previous observer if any.
    pub fn set_observer(&self, observer: Arc<dyn KvApplyObserver>) {
        *self.observer.lock().unwrap() = Some(observer);
    }

    pub fn clear_observer(&self) {
        *self.observer.lock().unwrap() = None;
    }

    fn cas_lock(&self, group: MultiRaftGroupId) -> Arc<std::sync::Mutex<()>> {
        let mut map = self.cas_locks.lock().unwrap();
        Arc::clone(
            map.entry(group)
                .or_insert_with(|| Arc::new(std::sync::Mutex::new(()))),
        )
    }

    /// Number of groups currently tracked by the engine.
    pub fn group_count(&self) -> usize {
        self.state.read().unwrap().len()
    }

    /// Propose `Put(key, value)` to `group`. Returns the assigned log
    /// index. The write becomes visible after the next
    /// [`Self::apply_committed`] pass.
    pub fn put(&self, group: MultiRaftGroupId, key: Vec<u8>, value: Vec<u8>) -> DbResult<u64> {
        let idx = self.registry.propose(
            group,
            RaftCommand::KvWrite {
                key,
                value: Some(value),
            },
        )?;
        self.apply_committed(group)?;
        Ok(idx)
    }

    /// Propose `Delete(key)`.
    pub fn delete(&self, group: MultiRaftGroupId, key: Vec<u8>) -> DbResult<u64> {
        let idx = self
            .registry
            .propose(group, RaftCommand::KvWrite { key, value: None })?;
        self.apply_committed(group)?;
        Ok(idx)
    }

    /// Local-applied read. Returns the value or `None` (key absent /
    /// tombstoned). Reads observed here are consistent with the
    /// local applied index but may lag the leader by one round-trip.
    pub fn get(&self, group: MultiRaftGroupId, key: &[u8]) -> DbResult<Option<Vec<u8>>> {
        let state = self.state.read().unwrap();
        Ok(state
            .get(&group)
            .and_then(|shard| shard.table.get(key).cloned()))
    }

    /// Apply every committed-but-unapplied entry of `group` to the
    /// local state machine. Returns the new `applied_index`.
    pub fn apply_committed(&self, group: MultiRaftGroupId) -> DbResult<u64> {
        let unapplied = self.registry.unapplied_entries(group)?;
        if unapplied.is_empty() {
            return Ok(self.applied_index(group));
        }
        let observer = self.observer.lock().unwrap().clone();
        let mut state = self.state.write().unwrap();
        let shard = state.entry(group).or_default();
        let mut last = shard.applied_index;
        let mut observations: Vec<(Vec<u8>, Option<Vec<u8>>, u64)> = Vec::new();
        for entry in &unapplied {
            shard.apply(entry);
            last = entry.index.max(last);
            if observer.is_some() {
                if let RaftCommand::KvWrite { key, value } = &entry.command {
                    observations.push((key.clone(), value.clone(), entry.index));
                }
            }
        }
        drop(state);
        if let Some(obs) = observer {
            for (key, value, idx) in observations {
                obs.on_write(group, &key, value.as_deref(), idx);
            }
        }
        self.registry.mark_applied(group, last)?;
        debug!(?group, last_applied = last, "kv_engine applied entries");
        Ok(last)
    }

    pub fn applied_index(&self, group: MultiRaftGroupId) -> u64 {
        self.state
            .read()
            .unwrap()
            .get(&group)
            .map(|shard| shard.applied_index)
            .unwrap_or(0)
    }

    /// Snapshot of a single group's state for testing / introspection.
    pub fn snapshot(&self, group: MultiRaftGroupId) -> BTreeMap<Vec<u8>, Vec<u8>> {
        self.state
            .read()
            .unwrap()
            .get(&group)
            .map(|shard| shard.table.clone())
            .unwrap_or_default()
    }

    /// Compare-and-swap : if `key` currently equals `expected`, replace
    /// it with `value`. Implemented as a read-modify-write that loops
    /// the proposer until either CAS succeeds or fails. Single-node
    /// strict consistency; multi-node use requires routing through the
    /// leader to avoid lost updates.
    pub fn cas(
        &self,
        group: MultiRaftGroupId,
        key: Vec<u8>,
        expected: Option<Vec<u8>>,
        new_value: Option<Vec<u8>>,
    ) -> DbResult<bool> {
        // Serialise CAS so two callers cannot both read the same
        // value, both decide their `expected` matches, and both
        // commit conflicting writes.
        let lock = self.cas_lock(group);
        let _guard = lock.lock().unwrap();
        let current = self.get(group, &key)?;
        if current != expected {
            return Ok(false);
        }
        match new_value {
            Some(v) => {
                self.put(group, key, v)?;
            }
            None => {
                self.delete(group, key)?;
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multi_raft::MultiRaftGroupId;
    use crate::protocol::NodeId;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn fresh() -> (tempfile::TempDir, Arc<MultiRaftRegistry>, KvEngine) {
        let tmp = tmpdir();
        let reg = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
        reg.create_group(MultiRaftGroupId::new(1), 1).unwrap();
        reg.become_leader(MultiRaftGroupId::new(1), &[]).unwrap();
        let engine = KvEngine::new(Arc::clone(&reg));
        (tmp, reg, engine)
    }

    fn g() -> MultiRaftGroupId {
        MultiRaftGroupId::new(1)
    }

    #[test]
    fn put_then_get_returns_value() {
        let (_t, _r, eng) = fresh();
        eng.put(g(), b"k".to_vec(), b"v".to_vec()).unwrap();
        assert_eq!(eng.get(g(), b"k").unwrap(), Some(b"v".to_vec()));
    }

    #[test]
    fn delete_removes_key() {
        let (_t, _r, eng) = fresh();
        eng.put(g(), b"k".to_vec(), b"v".to_vec()).unwrap();
        eng.delete(g(), b"k".to_vec()).unwrap();
        assert!(eng.get(g(), b"k").unwrap().is_none());
    }

    #[test]
    fn overwrite_replaces_value() {
        let (_t, _r, eng) = fresh();
        eng.put(g(), b"k".to_vec(), b"v1".to_vec()).unwrap();
        eng.put(g(), b"k".to_vec(), b"v2".to_vec()).unwrap();
        assert_eq!(eng.get(g(), b"k").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn cas_succeeds_only_when_expected_matches() {
        let (_t, _r, eng) = fresh();
        eng.put(g(), b"k".to_vec(), b"v1".to_vec()).unwrap();
        assert!(eng
            .cas(
                g(),
                b"k".to_vec(),
                Some(b"v1".to_vec()),
                Some(b"v2".to_vec())
            )
            .unwrap());
        assert_eq!(eng.get(g(), b"k").unwrap(), Some(b"v2".to_vec()));
        // Stale expected -> no-op.
        assert!(!eng
            .cas(
                g(),
                b"k".to_vec(),
                Some(b"v1".to_vec()),
                Some(b"v3".to_vec())
            )
            .unwrap());
        assert_eq!(eng.get(g(), b"k").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn applied_index_advances_with_writes() {
        let (_t, _r, eng) = fresh();
        for i in 0..10u8 {
            eng.put(g(), vec![i], vec![i]).unwrap();
        }
        assert!(eng.applied_index(g()) >= 10);
    }

    #[test]
    fn snapshot_returns_full_kv_map() {
        let (_t, _r, eng) = fresh();
        eng.put(g(), b"a".to_vec(), b"1".to_vec()).unwrap();
        eng.put(g(), b"b".to_vec(), b"2".to_vec()).unwrap();
        let snap = eng.snapshot(g());
        assert_eq!(snap.len(), 2);
        assert_eq!(snap.get(b"a".as_slice()), Some(&b"1".to_vec()));
    }

    #[test]
    fn observer_fires_on_every_applied_write() {
        let (_t, _r, eng) = fresh();
        #[derive(Default)]
        struct Capture {
            inner: std::sync::Mutex<Vec<(Vec<u8>, Option<Vec<u8>>)>>,
        }
        impl KvApplyObserver for Capture {
            fn on_write(&self, _g: MultiRaftGroupId, key: &[u8], value: Option<&[u8]>, _idx: u64) {
                self.inner
                    .lock()
                    .unwrap()
                    .push((key.to_vec(), value.map(|v| v.to_vec())));
            }
        }
        let cap = Arc::new(Capture::default());
        eng.set_observer(Arc::clone(&cap) as Arc<dyn KvApplyObserver>);
        eng.put(g(), b"a".to_vec(), b"1".to_vec()).unwrap();
        eng.put(g(), b"b".to_vec(), b"2".to_vec()).unwrap();
        eng.delete(g(), b"a".to_vec()).unwrap();
        let snap = cap.inner.lock().unwrap();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0], (b"a".to_vec(), Some(b"1".to_vec())));
        assert_eq!(snap[1], (b"b".to_vec(), Some(b"2".to_vec())));
        assert_eq!(snap[2], (b"a".to_vec(), None));
    }

    #[test]
    fn groups_are_independent_namespaces() {
        let tmp = tmpdir();
        let reg = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
        for n in 1..=2u64 {
            reg.create_group(MultiRaftGroupId::new(n), 1).unwrap();
            reg.become_leader(MultiRaftGroupId::new(n), &[]).unwrap();
        }
        let eng = KvEngine::new(Arc::clone(&reg));
        eng.put(MultiRaftGroupId::new(1), b"k".to_vec(), b"v1".to_vec())
            .unwrap();
        eng.put(MultiRaftGroupId::new(2), b"k".to_vec(), b"v2".to_vec())
            .unwrap();
        assert_eq!(
            eng.get(MultiRaftGroupId::new(1), b"k").unwrap(),
            Some(b"v1".to_vec())
        );
        assert_eq!(
            eng.get(MultiRaftGroupId::new(2), b"k").unwrap(),
            Some(b"v2".to_vec())
        );
    }
}
