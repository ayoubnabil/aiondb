//! Distributed pessimistic lock manager.
//!
//! Tracks per-(shard, key) locks held by distributed transactions.
//! Used during 2PC's prepare phase : each participant takes a
//! pessimistic lock on every key it intends to write so concurrent
//! writers from other transactions block instead of racing.
//!
//! Locks are released either on commit / abort or after a
//! lease-style TTL expires (defends against coordinator crashes).
//! Cycle detection delegated to [`crate::distributed_deadlock`].

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aiondb_core::DbResult;

use crate::distributed_record::DistributedTxnId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockMode {
    Shared,
    Exclusive,
}

#[derive(Clone, Debug)]
pub struct LockEntry {
    pub mode: LockMode,
    pub owners: HashSet<DistributedTxnId>,
    pub waiters: Vec<DistributedTxnId>,
    pub expires_at_us: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockOutcome {
    Granted,
    Wait { holders: Vec<DistributedTxnId> },
    Conflict { holder: DistributedTxnId },
}

pub const DEFAULT_LOCK_TTL_US: u64 = 30 * 1_000_000; // 30 seconds.

#[derive(Clone, Debug, Default)]
pub struct PessimisticLockManager {
    inner: Arc<std::sync::Mutex<BTreeMap<(u64, Vec<u8>), LockEntry>>>,
}

impl PessimisticLockManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn try_lock(
        &self,
        shard: u64,
        key: Vec<u8>,
        txn: DistributedTxnId,
        mode: LockMode,
        ttl: Duration,
    ) -> DbResult<LockOutcome> {
        let now = now_us();
        let mut guard = self.inner.lock().unwrap();
        let expires_at = now.saturating_add(u64::try_from(ttl.as_micros()).unwrap_or(u64::MAX));
        let entry = guard.entry((shard, key)).or_insert(LockEntry {
            mode,
            owners: HashSet::new(),
            waiters: Vec::new(),
            expires_at_us: expires_at,
        });
        // Clear expired ownership.
        if !entry.owners.is_empty() && entry.expires_at_us <= now {
            entry.owners.clear();
            entry.waiters.clear();
        }
        if entry.owners.is_empty() {
            entry.mode = mode;
            entry.owners.insert(txn);
            entry.expires_at_us = expires_at;
            return Ok(LockOutcome::Granted);
        }
        // Owner already present.
        if entry.owners.contains(&txn) {
            // Re-entrant grant; upgrade mode if needed.
            if matches!(mode, LockMode::Exclusive) {
                entry.mode = LockMode::Exclusive;
            }
            entry.expires_at_us = entry.expires_at_us.max(expires_at);
            return Ok(LockOutcome::Granted);
        }
        match (entry.mode, mode) {
            (LockMode::Shared, LockMode::Shared) => {
                entry.owners.insert(txn);
                entry.expires_at_us = entry.expires_at_us.max(expires_at);
                Ok(LockOutcome::Granted)
            }
            _ => {
                if !entry.waiters.contains(&txn) {
                    entry.waiters.push(txn);
                }
                let holders: Vec<DistributedTxnId> = entry.owners.iter().copied().collect();
                Ok(LockOutcome::Wait { holders })
            }
        }
    }

    pub fn release(&self, shard: u64, key: &[u8], txn: DistributedTxnId) -> DbResult<()> {
        let mut guard = self.inner.lock().unwrap();
        let Some(entry) = guard.get_mut(&(shard, key.to_vec())) else {
            return Ok(());
        };
        entry.owners.remove(&txn);
        if entry.owners.is_empty() {
            // Promote next waiter, if any.
            if let Some(next) = entry.waiters.pop() {
                entry.owners.insert(next);
            } else {
                guard.remove(&(shard, key.to_vec()));
            }
        }
        Ok(())
    }

    pub fn release_all(&self, txn: DistributedTxnId) -> DbResult<usize> {
        let mut guard = self.inner.lock().unwrap();
        let mut released = 0usize;
        let mut empty_keys: Vec<(u64, Vec<u8>)> = Vec::new();
        for (key, entry) in guard.iter_mut() {
            if entry.owners.remove(&txn) {
                released += 1;
                if entry.owners.is_empty() {
                    if let Some(next) = entry.waiters.pop() {
                        entry.owners.insert(next);
                    } else {
                        empty_keys.push(key.clone());
                    }
                }
            }
            entry.waiters.retain(|w| *w != txn);
        }
        for k in empty_keys {
            guard.remove(&k);
        }
        Ok(released)
    }

    pub fn owners_of(&self, shard: u64, key: &[u8]) -> Vec<DistributedTxnId> {
        let guard = self.inner.lock().unwrap();
        guard
            .get(&(shard, key.to_vec()))
            .map(|e| e.owners.iter().copied().collect())
            .unwrap_or_default()
    }

    pub fn entry_count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_micros()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distributed_record::DistributedTxnId;
    use crate::hlc::HlcTimestamp;

    fn txn(seq: u32) -> DistributedTxnId {
        DistributedTxnId {
            coordinator: 1,
            start_ts: HlcTimestamp::new(100, 0),
            seq,
        }
    }

    #[test]
    fn exclusive_lock_blocks_other_txns() {
        let lm = PessimisticLockManager::new();
        assert_eq!(
            lm.try_lock(
                1,
                b"k".to_vec(),
                txn(1),
                LockMode::Exclusive,
                Duration::from_secs(60)
            )
            .unwrap(),
            LockOutcome::Granted
        );
        match lm
            .try_lock(
                1,
                b"k".to_vec(),
                txn(2),
                LockMode::Exclusive,
                Duration::from_secs(60),
            )
            .unwrap()
        {
            LockOutcome::Wait { holders } => assert!(holders.contains(&txn(1))),
            other => panic!("expected Wait, got {other:?}"),
        }
    }

    #[test]
    fn shared_locks_can_overlap() {
        let lm = PessimisticLockManager::new();
        assert_eq!(
            lm.try_lock(
                1,
                b"k".to_vec(),
                txn(1),
                LockMode::Shared,
                Duration::from_secs(60)
            )
            .unwrap(),
            LockOutcome::Granted
        );
        assert_eq!(
            lm.try_lock(
                1,
                b"k".to_vec(),
                txn(2),
                LockMode::Shared,
                Duration::from_secs(60)
            )
            .unwrap(),
            LockOutcome::Granted
        );
    }

    #[test]
    fn reentrant_grant_for_same_owner() {
        let lm = PessimisticLockManager::new();
        lm.try_lock(
            1,
            b"k".to_vec(),
            txn(1),
            LockMode::Shared,
            Duration::from_secs(60),
        )
        .unwrap();
        let out = lm
            .try_lock(
                1,
                b"k".to_vec(),
                txn(1),
                LockMode::Exclusive,
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(out, LockOutcome::Granted);
        assert!(lm.owners_of(1, b"k").contains(&txn(1)));
    }

    #[test]
    fn release_drops_entry_when_no_owners_remain() {
        let lm = PessimisticLockManager::new();
        lm.try_lock(
            1,
            b"k".to_vec(),
            txn(1),
            LockMode::Exclusive,
            Duration::from_secs(60),
        )
        .unwrap();
        lm.release(1, b"k", txn(1)).unwrap();
        assert_eq!(lm.entry_count(), 0);
    }

    #[test]
    fn release_all_clears_every_lock_for_txn() {
        let lm = PessimisticLockManager::new();
        lm.try_lock(
            1,
            b"a".to_vec(),
            txn(1),
            LockMode::Exclusive,
            Duration::from_secs(60),
        )
        .unwrap();
        lm.try_lock(
            1,
            b"b".to_vec(),
            txn(1),
            LockMode::Exclusive,
            Duration::from_secs(60),
        )
        .unwrap();
        lm.try_lock(
            2,
            b"c".to_vec(),
            txn(1),
            LockMode::Exclusive,
            Duration::from_secs(60),
        )
        .unwrap();
        let released = lm.release_all(txn(1)).unwrap();
        assert_eq!(released, 3);
        assert_eq!(lm.entry_count(), 0);
    }

    #[test]
    fn expired_lock_is_reclaimable() {
        let lm = PessimisticLockManager::new();
        lm.try_lock(
            1,
            b"k".to_vec(),
            txn(1),
            LockMode::Exclusive,
            Duration::from_micros(1),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let out = lm
            .try_lock(
                1,
                b"k".to_vec(),
                txn(2),
                LockMode::Exclusive,
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(out, LockOutcome::Granted);
    }

    #[test]
    fn waiter_promoted_after_owner_releases() {
        let lm = PessimisticLockManager::new();
        lm.try_lock(
            1,
            b"k".to_vec(),
            txn(1),
            LockMode::Exclusive,
            Duration::from_secs(60),
        )
        .unwrap();
        let _ = lm
            .try_lock(
                1,
                b"k".to_vec(),
                txn(2),
                LockMode::Exclusive,
                Duration::from_secs(60),
            )
            .unwrap();
        lm.release(1, b"k", txn(1)).unwrap();
        assert!(lm.owners_of(1, b"k").contains(&txn(2)));
    }
}
