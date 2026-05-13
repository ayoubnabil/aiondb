//! Per-shard lease registry.
//!
//! In a Cockroach-style cluster the **leaseholder** of a shard is the
//! single replica allowed to serve linearizable reads and process writes
//! for that shard. The leaseholder concept is intentionally separated
//! from the Raft leader: a leaseholder can keep serving traffic across
//! short Raft re-elections (no extra consensus round-trip), and lease
//! transfers can be planned independently of log replication topology.
//!
//! A lease is identified by `(ShardId, LeaseEpoch)`. The epoch increases
//! every time ownership changes hands so stale leaseholders can detect
//! they have been preempted without consulting the leader: they compare
//! their cached epoch with the registry's current epoch on every
//! `acquire_for_serving` / `extend` call.
//!
//! ## Lifecycle
//!
//! 1. A replica calls [`LeaseRegistry::acquire`] to claim a fresh lease.
//!    The epoch advances and the holder is recorded with an expiration
//!    deadline.
//! 2. The holder periodically renews via [`LeaseRegistry::extend`]
//!    before the deadline expires.
//! 3. If the holder misses the deadline, another replica can call
//!    [`LeaseRegistry::acquire`] (which checks the deadline first) to
//!    take ownership.
//! 4. Explicit transfers go through [`LeaseRegistry::transfer`], which
//!    accepts a target node and bumps the epoch atomically.
//!
//! All operations are atomic against the registry's internal mutex. The
//! registry is `Clone` and cheap to share between async tasks.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ShardId;

/// Monotonic counter bumped every time a lease changes hands. Stale
/// holders compare their cached epoch with the registry to detect
/// preemption without consulting the Raft leader.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LeaseEpoch(u64);

impl LeaseEpoch {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
    fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl std::fmt::Display for LeaseEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "epoch-{}", self.0)
    }
}

/// Identifier of a replica node. Opaque -- the lease registry does not
/// care how nodes are addressed at the network layer, only that two
/// distinct nodes have distinct ids.
pub type LeaseHolderId = u64;

/// Active lease for one shard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Lease {
    pub shard: ShardId,
    pub holder: LeaseHolderId,
    pub epoch: LeaseEpoch,
    /// Wall-clock deadline at which the lease expires unless extended.
    pub expires_at: Instant,
}

impl Lease {
    /// True when `now` is past the lease deadline.
    pub fn is_expired(&self, now: Instant) -> bool {
        now >= self.expires_at
    }
}

/// Outcome of a `try_acquire` / `extend` call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseOutcome {
    /// The lease was granted (or extended) to the requesting holder.
    Granted(Lease),
    /// The shard is already held by another node and the current lease
    /// has not yet expired. The caller should back off.
    Held(Lease),
    /// The requesting holder presented a stale epoch and was preempted.
    Stale { current: Lease },
}

#[derive(Clone, Debug, Default)]
pub struct LeaseRegistry {
    inner: Arc<std::sync::Mutex<HashMap<ShardId, Lease>>>,
}

impl LeaseRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the current lease for a shard, if any.
    pub fn current(&self, shard: ShardId) -> Option<Lease> {
        self.lock().get(&shard).copied()
    }

    /// Try to acquire a fresh lease for `shard` on `holder`. Succeeds if
    /// no lease exists or the existing lease has expired according to
    /// `now`. The TTL is `now + ttl`. The epoch advances exactly once.
    pub fn acquire(
        &self,
        shard: ShardId,
        holder: LeaseHolderId,
        ttl: Duration,
        now: Instant,
    ) -> LeaseOutcome {
        let mut guard = self.lock();
        match guard.get(&shard).copied() {
            Some(existing) if !existing.is_expired(now) && existing.holder != holder => {
                LeaseOutcome::Held(existing)
            }
            Some(existing) => {
                let next_epoch = existing.epoch.next();
                let lease = Lease {
                    shard,
                    holder,
                    epoch: next_epoch,
                    expires_at: now + ttl,
                };
                guard.insert(shard, lease);
                LeaseOutcome::Granted(lease)
            }
            None => {
                let lease = Lease {
                    shard,
                    holder,
                    epoch: LeaseEpoch::new(1),
                    expires_at: now + ttl,
                };
                guard.insert(shard, lease);
                LeaseOutcome::Granted(lease)
            }
        }
    }

    /// Extend an existing lease the caller already holds. Validates the
    /// caller's epoch -- if it is stale, the call returns
    /// [`LeaseOutcome::Stale`] with the current lease so the caller can
    /// stop serving traffic.
    pub fn extend(
        &self,
        shard: ShardId,
        holder: LeaseHolderId,
        expected_epoch: LeaseEpoch,
        ttl: Duration,
        now: Instant,
    ) -> LeaseOutcome {
        let mut guard = self.lock();
        match guard.get(&shard).copied() {
            Some(existing) if existing.holder == holder && existing.epoch == expected_epoch => {
                let lease = Lease {
                    shard,
                    holder,
                    epoch: existing.epoch,
                    expires_at: now + ttl,
                };
                guard.insert(shard, lease);
                LeaseOutcome::Granted(lease)
            }
            Some(existing) => LeaseOutcome::Stale { current: existing },
            None => {
                // No lease at all -- treat as preemption rather than
                // silently granting, because the caller's `expected_epoch`
                // refers to a lease that has been forcibly revoked.
                let placeholder = Lease {
                    shard,
                    holder: 0,
                    epoch: LeaseEpoch::default(),
                    expires_at: now,
                };
                LeaseOutcome::Stale {
                    current: placeholder,
                }
            }
        }
    }

    /// Explicitly transfer ownership to `target`. Bumps the epoch
    /// regardless of expiration so the previous holder sees a `Stale`
    /// result on its next call. The new lease starts fresh with `ttl`.
    pub fn transfer(
        &self,
        shard: ShardId,
        target: LeaseHolderId,
        ttl: Duration,
        now: Instant,
    ) -> Lease {
        let mut guard = self.lock();
        let next_epoch = guard
            .get(&shard)
            .map(|lease| lease.epoch.next())
            .unwrap_or_else(|| LeaseEpoch::new(1));
        let lease = Lease {
            shard,
            holder: target,
            epoch: next_epoch,
            expires_at: now + ttl,
        };
        guard.insert(shard, lease);
        lease
    }

    /// Forcefully revoke any lease on this shard. Used during
    /// decommission flows: the operator drains a node, we revoke all of
    /// its leases so the cluster can pick up the slack.
    pub fn revoke(&self, shard: ShardId) -> Option<Lease> {
        self.lock().remove(&shard)
    }

    /// Snapshot every active lease. Used by `pg_stat_*` introspection
    /// and by the lease-balance planner.
    pub fn snapshot(&self) -> Vec<Lease> {
        let mut leases: Vec<_> = self.lock().values().copied().collect();
        leases.sort_by_key(|lease| lease.shard);
        leases
    }

    /// Number of currently-tracked leases.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ShardId, Lease>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard(n: u32) -> ShardId {
        ShardId::new(n)
    }

    #[test]
    fn first_acquire_grants_epoch_1() {
        let r = LeaseRegistry::new();
        let now = Instant::now();
        match r.acquire(shard(1), 42, Duration::from_secs(5), now) {
            LeaseOutcome::Granted(lease) => {
                assert_eq!(lease.shard, shard(1));
                assert_eq!(lease.holder, 42);
                assert_eq!(lease.epoch.get(), 1);
                assert_eq!(lease.expires_at, now + Duration::from_secs(5));
            }
            other => panic!("expected Granted, got {other:?}"),
        }
    }

    #[test]
    fn acquire_by_other_holder_returns_held_until_expired() {
        let r = LeaseRegistry::new();
        let t0 = Instant::now();
        let _ = r.acquire(shard(1), 1, Duration::from_secs(5), t0);
        match r.acquire(shard(1), 2, Duration::from_secs(5), t0) {
            LeaseOutcome::Held(lease) => assert_eq!(lease.holder, 1),
            other => panic!("expected Held while lease still live, got {other:?}"),
        }
        // After deadline, the other holder takes over with a bumped epoch.
        let t1 = t0 + Duration::from_secs(6);
        match r.acquire(shard(1), 2, Duration::from_secs(5), t1) {
            LeaseOutcome::Granted(lease) => {
                assert_eq!(lease.holder, 2);
                assert_eq!(lease.epoch.get(), 2);
            }
            other => panic!("expected Granted after expiry, got {other:?}"),
        }
    }

    #[test]
    fn extend_with_matching_epoch_renews_deadline() {
        let r = LeaseRegistry::new();
        let t0 = Instant::now();
        let LeaseOutcome::Granted(initial) = r.acquire(shard(1), 1, Duration::from_secs(5), t0)
        else {
            panic!("acquire should grant fresh lease");
        };
        let t1 = t0 + Duration::from_secs(2);
        match r.extend(shard(1), 1, initial.epoch, Duration::from_secs(5), t1) {
            LeaseOutcome::Granted(renewed) => {
                assert_eq!(renewed.epoch, initial.epoch);
                assert_eq!(renewed.holder, 1);
                assert_eq!(renewed.expires_at, t1 + Duration::from_secs(5));
            }
            other => panic!("expected renewal, got {other:?}"),
        }
    }

    #[test]
    fn extend_with_stale_epoch_is_rejected() {
        let r = LeaseRegistry::new();
        let t0 = Instant::now();
        let _ = r.acquire(shard(1), 1, Duration::from_secs(5), t0);
        let _ = r.transfer(shard(1), 2, Duration::from_secs(5), t0);
        // Original holder tries to renew with old epoch -- must fail.
        match r.extend(shard(1), 1, LeaseEpoch::new(1), Duration::from_secs(5), t0) {
            LeaseOutcome::Stale { current } => {
                assert_eq!(current.holder, 2);
                assert_eq!(current.epoch.get(), 2);
            }
            other => panic!("expected stale, got {other:?}"),
        }
    }

    #[test]
    fn transfer_bumps_epoch_and_replaces_holder() {
        let r = LeaseRegistry::new();
        let t0 = Instant::now();
        let _ = r.acquire(shard(1), 1, Duration::from_secs(5), t0);
        let lease = r.transfer(shard(1), 99, Duration::from_secs(5), t0);
        assert_eq!(lease.holder, 99);
        assert_eq!(lease.epoch.get(), 2);
        assert_eq!(r.current(shard(1)), Some(lease));
    }

    #[test]
    fn revoke_clears_the_lease() {
        let r = LeaseRegistry::new();
        let t0 = Instant::now();
        let _ = r.acquire(shard(1), 1, Duration::from_secs(5), t0);
        let revoked = r.revoke(shard(1)).expect("revoke returns previous lease");
        assert_eq!(revoked.holder, 1);
        assert!(r.current(shard(1)).is_none());
        assert!(r.is_empty());
    }

    #[test]
    fn snapshot_returns_sorted_leases() {
        let r = LeaseRegistry::new();
        let t0 = Instant::now();
        let _ = r.acquire(shard(3), 30, Duration::from_secs(5), t0);
        let _ = r.acquire(shard(1), 10, Duration::from_secs(5), t0);
        let _ = r.acquire(shard(2), 20, Duration::from_secs(5), t0);
        let snap = r.snapshot();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].shard, shard(1));
        assert_eq!(snap[1].shard, shard(2));
        assert_eq!(snap[2].shard, shard(3));
    }

    #[test]
    fn extend_without_existing_lease_reports_stale() {
        let r = LeaseRegistry::new();
        let now = Instant::now();
        match r.extend(shard(1), 7, LeaseEpoch::new(3), Duration::from_secs(5), now) {
            LeaseOutcome::Stale { current } => assert_eq!(current.holder, 0),
            other => panic!("expected stale, got {other:?}"),
        }
    }
}
