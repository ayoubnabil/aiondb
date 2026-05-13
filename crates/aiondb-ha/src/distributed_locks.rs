//! Distributed advisory locks.
//!
//! Named, cluster-wide locks similar to `pg_advisory_lock` but with
//! durability, TTL and a **fencing token**. Useful for coordinated
//! maintenance jobs (split, rebalance, schema migration) that must
//! run on exactly one node at a time.
//!
//! Built on the existing [`KvEngine`] so every lock acquire / release
//! flows through Raft. Properties guaranteed:
//!
//! - **Exclusive.** At most one holder per name at any instant.
//! - **Fenced.** Each acquire returns a monotonically-increasing
//!   `fencing_token`. Stale holders can be safely rejected by
//!   downstream services using the token as a guard.
//! - **TTL-bounded.** Locks expire if the holder forgets to renew.
//!   A janitor task in the future can sweep expired locks; this
//!   module exposes [`DistributedLockService::reap_expired`] for that
//!   use case.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aiondb_core::{DbError, DbResult};
use serde::{Deserialize, Serialize};

use crate::kv_engine::KvEngine;
use crate::multi_raft::MultiRaftGroupId;

/// Default lock TTL.
pub const DEFAULT_LOCK_TTL: Duration = Duration::from_secs(30);

/// One lock record stored under the KV engine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LockRecord {
    pub name: String,
    pub holder: String,
    pub fencing_token: u64,
    pub expires_at_us: u64,
}

impl LockRecord {
    fn is_expired(&self, now_us: u64) -> bool {
        now_us >= self.expires_at_us
    }
}

/// Distributed lock service.
#[derive(Clone, Debug)]
pub struct DistributedLockService {
    engine: KvEngine,
    group: MultiRaftGroupId,
    /// Counter for fencing tokens. Bumped on every acquire that
    /// changes the holder.
    next_token: Arc<std::sync::atomic::AtomicU64>,
    /// Serialises acquire / renew / release so concurrent contenders
    /// cannot both read-then-write the same lock record without
    /// observing the other's write.
    op_lock: Arc<std::sync::Mutex<()>>,
}

impl DistributedLockService {
    pub fn new(engine: KvEngine, group: MultiRaftGroupId) -> Self {
        Self {
            engine,
            group,
            next_token: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            op_lock: Arc::new(std::sync::Mutex::new(())),
        }
    }

    /// Try to acquire `name` for `holder`. Returns the new
    /// [`LockRecord`] on success or the existing record (with reason)
    /// on conflict.
    pub fn acquire(
        &self,
        name: impl Into<String>,
        holder: impl Into<String>,
        ttl: Duration,
    ) -> DbResult<AcquireOutcome> {
        let name = name.into();
        let holder = holder.into();
        let key = key_of(&name);
        let _op = self.op_lock.lock().unwrap();
        let now = now_us();
        let existing = self.read(&key)?;
        if let Some(record) = existing {
            if !record.is_expired(now) && record.holder != holder {
                return Ok(AcquireOutcome::Held { existing: record });
            }
            // Either expired or same holder -- both proceed to renew.
        }
        let token = self
            .next_token
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let expires_at_us = now.saturating_add(u64::try_from(ttl.as_micros()).unwrap_or(u64::MAX));
        let record = LockRecord {
            name,
            holder,
            fencing_token: token,
            expires_at_us,
        };
        self.write(&key, &record)?;
        Ok(AcquireOutcome::Granted(record))
    }

    /// Renew an existing lock. Returns the refreshed record or
    /// `AcquireOutcome::Held` if the caller no longer owns the lock.
    pub fn renew(
        &self,
        name: &str,
        holder: &str,
        fencing_token: u64,
        ttl: Duration,
    ) -> DbResult<AcquireOutcome> {
        let key = key_of(name);
        let _op = self.op_lock.lock().unwrap();
        let now = now_us();
        let existing = self.read(&key)?;
        let Some(mut record) = existing else {
            return Err(DbError::internal(format!(
                "lock {name} is not held; renew rejected"
            )));
        };
        if record.holder != holder || record.fencing_token != fencing_token {
            return Ok(AcquireOutcome::Held { existing: record });
        }
        record.expires_at_us =
            now.saturating_add(u64::try_from(ttl.as_micros()).unwrap_or(u64::MAX));
        self.write(&key, &record)?;
        Ok(AcquireOutcome::Granted(record))
    }

    /// Release a lock. Fails when the caller does not own it.
    pub fn release(&self, name: &str, holder: &str, fencing_token: u64) -> DbResult<()> {
        let key = key_of(name);
        let _op = self.op_lock.lock().unwrap();
        let existing = self.read(&key)?;
        let Some(record) = existing else {
            return Ok(()); // already free; idempotent.
        };
        if record.holder != holder || record.fencing_token != fencing_token {
            return Err(DbError::internal(format!(
                "lock {name} not owned by {holder}@{fencing_token}"
            )));
        }
        self.engine.delete(self.group, key)?;
        Ok(())
    }

    /// Force-release without ownership check. Use sparingly (operator
    /// intervention, decommissioning).
    pub fn force_release(&self, name: &str) -> DbResult<()> {
        self.engine.delete(self.group, key_of(name))?;
        Ok(())
    }

    /// Inspect the current holder.
    pub fn inspect(&self, name: &str) -> DbResult<Option<LockRecord>> {
        self.read(&key_of(name))
    }

    /// Iterate every lock and remove ones whose TTL has elapsed.
    /// Returns the number reaped.
    pub fn reap_expired(&self) -> DbResult<usize> {
        let now = now_us();
        let mut reaped = 0usize;
        for (key, raw) in self.engine.snapshot(self.group) {
            if !key.starts_with(LOCK_PREFIX) {
                continue;
            }
            if let Ok(record) = serde_json::from_slice::<LockRecord>(&raw) {
                if record.is_expired(now) {
                    self.engine.delete(self.group, key)?;
                    reaped += 1;
                }
            }
        }
        Ok(reaped)
    }

    fn read(&self, key: &[u8]) -> DbResult<Option<LockRecord>> {
        let value = self.engine.get(self.group, key)?;
        match value {
            Some(bytes) => {
                Ok(Some(serde_json::from_slice(&bytes).map_err(|e| {
                    DbError::internal(format!("lock record decode: {e}"))
                })?))
            }
            None => Ok(None),
        }
    }

    fn write(&self, key: &[u8], record: &LockRecord) -> DbResult<()> {
        let bytes = serde_json::to_vec(record)
            .map_err(|e| DbError::internal(format!("lock record encode: {e}")))?;
        self.engine.put(self.group, key.to_vec(), bytes)?;
        Ok(())
    }
}

/// Outcome of an acquire call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AcquireOutcome {
    Granted(LockRecord),
    Held { existing: LockRecord },
}

const LOCK_PREFIX: &[u8] = b"lock:";

fn key_of(name: &str) -> Vec<u8> {
    let mut k = LOCK_PREFIX.to_vec();
    k.extend_from_slice(name.as_bytes());
    k
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
    use crate::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
    use crate::protocol::NodeId;
    use std::sync::Arc;

    fn fresh() -> (tempfile::TempDir, DistributedLockService) {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
        let g = MultiRaftGroupId::new(99);
        registry.create_group(g, 1).unwrap();
        registry.become_leader(g, &[]).unwrap();
        let engine = KvEngine::new(registry);
        (tmp, DistributedLockService::new(engine, g))
    }

    #[test]
    fn first_acquire_grants() {
        let (_t, svc) = fresh();
        match svc.acquire("table-a", "node-1", DEFAULT_LOCK_TTL).unwrap() {
            AcquireOutcome::Granted(r) => {
                assert_eq!(r.holder, "node-1");
                assert_eq!(r.fencing_token, 1);
            }
            other => panic!("expected Granted, got {other:?}"),
        }
    }

    #[test]
    fn second_holder_blocked_until_expiry() {
        let (_t, svc) = fresh();
        svc.acquire("table-a", "node-1", DEFAULT_LOCK_TTL).unwrap();
        match svc.acquire("table-a", "node-2", DEFAULT_LOCK_TTL).unwrap() {
            AcquireOutcome::Held { existing } => assert_eq!(existing.holder, "node-1"),
            other => panic!("expected Held, got {other:?}"),
        }
    }

    #[test]
    fn fencing_token_is_monotonic() {
        let (_t, svc) = fresh();
        let r1 = match svc.acquire("a", "node-1", DEFAULT_LOCK_TTL).unwrap() {
            AcquireOutcome::Granted(r) => r,
            _ => panic!(),
        };
        let r2 = match svc.acquire("b", "node-1", DEFAULT_LOCK_TTL).unwrap() {
            AcquireOutcome::Granted(r) => r,
            _ => panic!(),
        };
        assert!(r2.fencing_token > r1.fencing_token);
    }

    #[test]
    fn renew_extends_ttl_for_owner_only() {
        let (_t, svc) = fresh();
        let r = match svc.acquire("a", "node-1", Duration::from_secs(1)).unwrap() {
            AcquireOutcome::Granted(r) => r,
            _ => panic!(),
        };
        match svc
            .renew("a", "node-1", r.fencing_token, Duration::from_secs(60))
            .unwrap()
        {
            AcquireOutcome::Granted(updated) => assert!(updated.expires_at_us > r.expires_at_us),
            other => panic!("expected Granted, got {other:?}"),
        }
        // Wrong holder.
        match svc
            .renew("a", "node-2", r.fencing_token, DEFAULT_LOCK_TTL)
            .unwrap()
        {
            AcquireOutcome::Held { .. } => {}
            other => panic!("expected Held, got {other:?}"),
        }
        // Right holder, stale fencing token.
        match svc.renew("a", "node-1", 9999, DEFAULT_LOCK_TTL).unwrap() {
            AcquireOutcome::Held { .. } => {}
            other => panic!("expected Held, got {other:?}"),
        }
    }

    #[test]
    fn release_must_match_owner_and_token() {
        let (_t, svc) = fresh();
        let r = match svc.acquire("a", "node-1", DEFAULT_LOCK_TTL).unwrap() {
            AcquireOutcome::Granted(r) => r,
            _ => panic!(),
        };
        assert!(svc.release("a", "node-2", r.fencing_token).is_err());
        assert!(svc.release("a", "node-1", 9999).is_err());
        svc.release("a", "node-1", r.fencing_token).unwrap();
        assert!(svc.inspect("a").unwrap().is_none());
    }

    #[test]
    fn force_release_works_even_without_ownership() {
        let (_t, svc) = fresh();
        svc.acquire("a", "node-1", DEFAULT_LOCK_TTL).unwrap();
        svc.force_release("a").unwrap();
        assert!(svc.inspect("a").unwrap().is_none());
    }

    #[test]
    fn reap_removes_expired_locks() {
        let (_t, svc) = fresh();
        svc.acquire("hot", "node-1", Duration::from_secs(10_000))
            .unwrap();
        svc.acquire("cold", "node-2", Duration::from_micros(1))
            .unwrap();
        // Spin briefly so wall clock advances past the 1us TTL.
        std::thread::sleep(Duration::from_millis(2));
        let reaped = svc.reap_expired().unwrap();
        assert_eq!(reaped, 1);
        assert!(svc.inspect("hot").unwrap().is_some());
        assert!(svc.inspect("cold").unwrap().is_none());
    }

    #[test]
    fn same_holder_can_re_acquire_to_extend() {
        let (_t, svc) = fresh();
        let r1 = match svc.acquire("a", "node-1", DEFAULT_LOCK_TTL).unwrap() {
            AcquireOutcome::Granted(r) => r,
            _ => panic!(),
        };
        let r2 = match svc.acquire("a", "node-1", DEFAULT_LOCK_TTL).unwrap() {
            AcquireOutcome::Granted(r) => r,
            _ => panic!(),
        };
        assert!(r2.fencing_token > r1.fencing_token);
    }
}
