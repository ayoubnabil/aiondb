//! Lock manager implementations.
//!
//! The production manager is [`WaitGraphLockManager`]: a sharded
//! wait-for-graph lock table that supports table-level + tuple-level
//! grants, lock upgrades, and deadlock detection by reachability.
//!
//! # Production invariants
//!
//! * **Sharding.** Lock state is partitioned into 16 shards keyed by
//!   relation id. Per-relation locks always live in the same shard so
//!   wait-graph cycles within a relation remain detectable; unrelated
//!   relations make progress independently.
//! * **Upgrade safety.** A txn that already owns a lock on `(table,
//!   tuple)` may upgrade (e.g. `IS → IX → X`); the manager detects
//!   incompatible existing grants on the same key from other txns and
//!   rejects with the right SQLSTATE before parking.
//! * **Deadlock detection.** Before parking, the requester adds a wait-for
//!   edge `requester → holder` and runs a reachability check. If
//!   `holder → requester` is reachable, a cycle exists and the requester
//!   raises SQLSTATE 40P01 instead of parking.
//! * **Timeout.** Default timeout is 1 second per acquire. Sessions can
//!   override via `SET lock_timeout`. A timeout returns SQLSTATE 55P03
//!   without leaving a stale wait edge.
//! * **Wakeups.** [`WaitGraphLockManager::release_txn`] only notifies
//!   shards that recorded a waiter for that txn; idle shards are skipped
//!   to avoid hot-path wakeup storms.
//!
//! # Test-only
//!
//! tests that need to drive concurrency primitives without lock-manager

#![allow(clippy::unnested_or_patterns)]

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::{Duration, Instant},
};

use parking_lot::{Condvar, Mutex, MutexGuard};

use aiondb_core::{DbError, DbResult, RelationId, SqlState, TupleId, TxnId};

use crate::LockMode;

#[allow(clippy::missing_errors_doc)]
pub trait LockManager: Send + Sync {
    fn acquire_table_lock(&self, txn: TxnId, table_id: RelationId, mode: LockMode) -> DbResult<()>;
    fn acquire_tuple_lock(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: LockMode,
    ) -> DbResult<()>;

    fn release_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }

    /// Set a per-transaction lock timeout override. Implementations that
    /// support configurable timeouts use this to honour session-level
    fn set_txn_lock_timeout(&self, _txn: TxnId, _timeout: Duration) {}

    /// Clear the per-transaction lock timeout override. Called on transaction
    fn clear_txn_lock_timeout(&self, _txn: TxnId) {}

    /// Return transaction ids currently holding write-intent table locks on
    /// `table_id` (RowExclusive/Update/AccessExclusive).
    fn table_write_lock_holders(&self, _table_id: RelationId) -> DbResult<Vec<TxnId>> {
        Ok(Vec::new())
    }

    /// Return whether `txn` currently holds any write-intent lock (table or tuple).
    fn txn_holds_write_locks(&self, _txn: TxnId) -> DbResult<bool> {
        Ok(false)
    }

    /// Return whether `txn` currently holds exactly `mode` on `table_id`.
    fn txn_holds_table_lock(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
        _mode: LockMode,
    ) -> DbResult<bool> {
        Ok(false)
    }

    /// Return whether `txn` currently holds exactly `mode` on a tuple.
    fn txn_holds_tuple_lock(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
        _tuple_id: TupleId,
        _mode: LockMode,
    ) -> DbResult<bool> {
        Ok(false)
    }

    /// Atomic equivalent of `txn_holds_tuple_lock(...)` immediately followed
    /// by `acquire_tuple_lock(...)`. Returns whether the lock was already
    /// held in the requested mode before this call. Implementations that
    /// shard internally should override this to hold the relevant shard
    /// lock for both the check and the grant in one round-trip; the
    /// default falls back to two sequential calls.
    ///
    /// Used by the bulk UPDATE row loop to avoid the per-tuple double
    /// shard-mutex acquisition that the historical "check then acquire"
    /// pattern paid.
    fn acquire_tuple_lock_returning_was_held(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: LockMode,
    ) -> DbResult<bool> {
        let was_held = self.txn_holds_tuple_lock(txn, table_id, tuple_id, mode)?;
        self.acquire_tuple_lock(txn, table_id, tuple_id, mode)?;
        Ok(was_held)
    }

    /// Acquire a table lock without waiting.
    ///
    /// Implementations should override this with true NOWAIT behavior.
    /// The default is intentionally fail-closed so custom lock managers do
    /// not accidentally block on a code path that the caller expects to be
    /// non-blocking.
    fn try_acquire_table_lock_nowait(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
        _mode: LockMode,
    ) -> DbResult<()> {
        Err(DbError::transaction_error(
            SqlState::FeatureNotSupported,
            "NOWAIT table locks are not supported by this lock manager",
        ))
    }

    fn try_acquire_tuple_lock_nowait(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
        _tuple_id: TupleId,
        _mode: LockMode,
    ) -> DbResult<()> {
        Err(DbError::transaction_error(
            SqlState::FeatureNotSupported,
            "NOWAIT tuple locks are not supported by this lock manager",
        ))
    }
}

#[derive(Debug, Default)]
pub struct NoopLockManager;

impl LockManager for NoopLockManager {
    fn acquire_table_lock(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
        _mode: LockMode,
    ) -> DbResult<()> {
        Ok(())
    }

    fn acquire_tuple_lock(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
        _tuple_id: TupleId,
        _mode: LockMode,
    ) -> DbResult<()> {
        Ok(())
    }

    fn try_acquire_table_lock_nowait(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
        _mode: LockMode,
    ) -> DbResult<()> {
        Ok(())
    }

    fn try_acquire_tuple_lock_nowait(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
        _tuple_id: TupleId,
        _mode: LockMode,
    ) -> DbResult<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum LockTarget {
    Table(RelationId),
    Tuple {
        table_id: RelationId,
        tuple_id: TupleId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LockGrant {
    txn: TxnId,
    mode: LockMode,
}

#[derive(Debug, Default)]
struct LockState {
    grants: HashMap<LockTarget, Vec<LockGrant>>,
    held_by_txn: HashMap<TxnId, HashMap<LockTarget, LockMode>>,
    wait_for: HashMap<TxnId, HashSet<TxnId>>,
    /// Number of waiters parked on each target. Lets `release_txn` skip
    /// `notify_all` when no thread is waiting (eliminates wakeup storms on
    /// the hot uncontended path).
    waiters_per_target: HashMap<LockTarget, u32>,
}

impl LockState {
    fn held_mode(&self, txn: TxnId, target: LockTarget) -> Option<LockMode> {
        self.held_by_txn
            .get(&txn)
            .and_then(|targets| targets.get(&target).copied())
    }

    fn blockers(&self, txn: TxnId, target: LockTarget, mode: LockMode) -> HashSet<TxnId> {
        self.grants
            .get(&target)
            .into_iter()
            .flatten()
            .filter(|grant| grant.txn != txn && !lock_modes_compatible(grant.mode, mode))
            .map(|grant| grant.txn)
            .collect()
    }

    fn grant(&mut self, txn: TxnId, target: LockTarget, mode: LockMode) {
        let grants = self.grants.entry(target).or_default();
        if let Some(existing) = grants.iter_mut().find(|grant| grant.txn == txn) {
            existing.mode = mode;
        } else {
            grants.push(LockGrant { txn, mode });
        }

        self.held_by_txn
            .entry(txn)
            .or_default()
            .insert(target, mode);
        self.wait_for.remove(&txn);
    }

    fn release_txn(&mut self, txn: TxnId) {
        if let Some(held) = self.held_by_txn.remove(&txn) {
            for target in held.into_keys() {
                if let Some(grants) = self.grants.get_mut(&target) {
                    grants.retain(|grant| grant.txn != txn);
                    if grants.is_empty() {
                        self.grants.remove(&target);
                    }
                }
            }
        }

        self.wait_for.remove(&txn);
        for blockers in self.wait_for.values_mut() {
            blockers.remove(&txn);
        }
    }

    fn would_deadlock(&self, waiting_txn: TxnId, blockers: &HashSet<TxnId>) -> bool {
        blockers
            .iter()
            .copied()
            .any(|blocker| self.reachable(blocker, waiting_txn))
    }

    fn reachable(&self, start: TxnId, target: TxnId) -> bool {
        let mut visited = HashSet::new();
        let mut stack = vec![start];

        while let Some(current) = stack.pop() {
            if current == target {
                return true;
            }
            if !visited.insert(current) {
                continue;
            }
            if let Some(next) = self.wait_for.get(&current) {
                stack.extend(next.iter().copied());
            }
        }

        false
    }
}

/// Default lock wait timeout (1 second). Prevents indefinite blocking when a
/// lock holder becomes stuck (e.g. disconnected client without TCP keepalive).
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_LOCK_WAIT_CHUNK: Duration = Duration::from_secs(60);

/// Lock-state shard count. Must be a power of two so that
/// `target_shard_idx` can use a bitmask. Sharding by table id keeps every
/// operation on a given relation (table + all its tuples) inside one shard,
/// preserving exact wait-graph deadlock detection per relation while letting
/// disjoint-relation traffic proceed in parallel.
const SHARDS: usize = 16;
const SHARD_MASK: usize = SHARDS - 1;

#[inline]
fn target_shard_idx(target: LockTarget) -> usize {
    let table = match target {
        LockTarget::Table(r) => r.get(),
        LockTarget::Tuple { table_id, .. } => table_id.get(),
    };
    let mixed = table.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let shard = usize::try_from(mixed).unwrap_or_else(|_| {
        let folded = (mixed ^ (mixed >> 32)) & u64::from(u32::MAX);
        usize::try_from(folded).unwrap_or(usize::MAX)
    });
    shard & SHARD_MASK
}

#[derive(Default)]
struct LockShard {
    state: Mutex<LockState>,
    wake: Condvar,
}

pub struct WaitGraphLockManager {
    shards: [LockShard; SHARDS],
    lock_timeout: Duration,
    /// Per-transaction timeout overrides set by session-level `SET lock_timeout`.
    txn_timeout_overrides: Mutex<BTreeMap<TxnId, Duration>>,
}

impl Default for WaitGraphLockManager {
    fn default() -> Self {
        Self {
            shards: std::array::from_fn(|_| LockShard::default()),
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
            txn_timeout_overrides: Mutex::new(BTreeMap::new()),
        }
    }
}

impl std::fmt::Debug for WaitGraphLockManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaitGraphLockManager").finish()
    }
}

impl WaitGraphLockManager {
    fn timeout_overrides_guard(&self) -> MutexGuard<'_, BTreeMap<TxnId, Duration>> {
        self.txn_timeout_overrides.lock()
    }

    /// Resolve the effective lock timeout for a transaction: per-txn override
    /// if set, otherwise the instance default.
    fn effective_lock_timeout(&self, txn: TxnId) -> Duration {
        self.timeout_overrides_guard()
            .get(&txn)
            .copied()
            .unwrap_or(self.lock_timeout)
    }

    fn shard_for(&self, target: LockTarget) -> &LockShard {
        &self.shards[target_shard_idx(target)]
    }

    fn shard_state(&self, target: LockTarget) -> MutexGuard<'_, LockState> {
        self.shard_for(target).state.lock()
    }

    fn acquire(&self, txn: TxnId, target: LockTarget, requested_mode: LockMode) -> DbResult<()> {
        // Compute timeout BEFORE acquiring `state` to avoid nested lock
        // (state → txn_timeout_overrides), which would risk deadlock if any
        // code path ever acquired the locks in the opposite order.
        let deadline = lock_wait_deadline(self.effective_lock_timeout(txn));
        let shard = self.shard_for(target);
        let mut state = shard.state.lock();
        let mut parked = false;

        loop {
            let held_mode = state.held_mode(txn, target);
            let effective_mode = held_mode.map_or(requested_mode, |held| {
                strongest_lock_mode(held, requested_mode)
            });

            if held_mode == Some(effective_mode) {
                state.wait_for.remove(&txn);
                if parked {
                    decrement_waiter(&mut state, target);
                }
                return Ok(());
            }

            // `state` stays locked across blocker discovery and grant, so an
            // upgrade decision is made against one consistent snapshot.
            let blockers = state.blockers(txn, target, effective_mode);
            if blockers.is_empty() {
                state.grant(txn, target, effective_mode);
                if parked {
                    decrement_waiter(&mut state, target);
                }
                return Ok(());
            }

            state.wait_for.insert(txn, blockers.clone());

            if state.would_deadlock(txn, &blockers) {
                state.wait_for.remove(&txn);
                if parked {
                    decrement_waiter(&mut state, target);
                }
                return Err(DbError::transaction_error(
                    SqlState::DeadlockDetected,
                    format!("deadlock detected while waiting on {target:?}"),
                ));
            }

            if lock_wait_deadline_expired(deadline) {
                state.wait_for.remove(&txn);
                if parked {
                    decrement_waiter(&mut state, target);
                }
                return Err(DbError::transaction_error(
                    SqlState::LockNotAvailable,
                    format!("lock wait timeout exceeded on {target:?}"),
                ));
            }

            if !parked {
                *state.waiters_per_target.entry(target).or_insert(0) += 1;
                parked = true;
            }
            let remaining = lock_wait_remaining(deadline);
            shard.wake.wait_for(&mut state, remaining);
        }
    }

    fn acquire_nowait(
        &self,
        txn: TxnId,
        target: LockTarget,
        requested_mode: LockMode,
    ) -> DbResult<()> {
        let mut state = self.shard_state(target);
        let held_mode = state.held_mode(txn, target);
        let effective_mode = held_mode.map_or(requested_mode, |held| {
            strongest_lock_mode(held, requested_mode)
        });

        if held_mode == Some(effective_mode) {
            state.wait_for.remove(&txn);
            return Ok(());
        }

        let blockers = state.blockers(txn, target, effective_mode);
        if blockers.is_empty() {
            state.grant(txn, target, effective_mode);
            return Ok(());
        }

        state.wait_for.remove(&txn);
        Err(DbError::transaction_error(
            SqlState::LockNotAvailable,
            format!("could not obtain lock on {target:?}"),
        ))
    }
}

fn decrement_waiter(state: &mut LockState, target: LockTarget) {
    if let Some(count) = state.waiters_per_target.get_mut(&target) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            state.waiters_per_target.remove(&target);
        }
    }
}

fn lock_wait_deadline(timeout: Duration) -> Option<Instant> {
    Instant::now().checked_add(timeout)
}

fn lock_wait_remaining(deadline: Option<Instant>) -> Duration {
    match deadline {
        Some(deadline) => deadline
            .saturating_duration_since(Instant::now())
            .min(MAX_LOCK_WAIT_CHUNK),
        None => MAX_LOCK_WAIT_CHUNK,
    }
}

fn lock_wait_deadline_expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| deadline.saturating_duration_since(Instant::now()).is_zero())
}

impl LockManager for WaitGraphLockManager {
    fn acquire_table_lock(&self, txn: TxnId, table_id: RelationId, mode: LockMode) -> DbResult<()> {
        self.acquire(txn, LockTarget::Table(table_id), mode)
    }

    fn acquire_tuple_lock(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: LockMode,
    ) -> DbResult<()> {
        self.acquire(txn, LockTarget::Tuple { table_id, tuple_id }, mode)
    }

    fn acquire_tuple_lock_returning_was_held(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: LockMode,
    ) -> DbResult<bool> {
        // Take the shard lock once and inspect the held mode before
        // delegating to the slow acquire path. The grant itself happens
        // through the regular `acquire` call below so that the wait /
        // deadlock path is reused unchanged.
        let target = LockTarget::Tuple { table_id, tuple_id };
        let was_held = {
            let state = self.shard_state(target);
            state
                .held_by_txn
                .get(&txn)
                .and_then(|targets| targets.get(&target).copied())
                == Some(mode)
        };
        self.acquire(txn, target, mode)?;
        Ok(was_held)
    }

    fn try_acquire_tuple_lock_nowait(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: LockMode,
    ) -> DbResult<()> {
        self.acquire_nowait(txn, LockTarget::Tuple { table_id, tuple_id }, mode)
    }

    fn release_txn(&self, txn: TxnId) -> DbResult<()> {
        // Walk every shard: a txn may have acquired locks across multiple
        // tables, each routed to its own shard. Each shard is locked
        // independently - there is no global state lock - so disjoint shards
        // proceed in parallel. The timeout-override map is touched after all
        // shards drop, mirroring the pre-shard ordering (state → overrides)
        // to avoid any nested-lock deadlock potential.
        for shard in &self.shards {
            let mut state = shard.state.lock();
            let had_waiters = !state.waiters_per_target.is_empty();
            state.release_txn(txn);
            drop(state);
            if had_waiters {
                shard.wake.notify_all();
            }
        }
        self.timeout_overrides_guard().remove(&txn);
        Ok(())
    }

    fn set_txn_lock_timeout(&self, txn: TxnId, timeout: Duration) {
        self.timeout_overrides_guard().insert(txn, timeout);
    }

    fn clear_txn_lock_timeout(&self, txn: TxnId) {
        self.timeout_overrides_guard().remove(&txn);
    }

    fn table_write_lock_holders(&self, table_id: RelationId) -> DbResult<Vec<TxnId>> {
        let target = LockTarget::Table(table_id);
        let state = self.shard_state(target);
        let mut holders = HashSet::new();
        if let Some(grants) = state.grants.get(&target) {
            for grant in grants {
                if is_write_table_lock_mode(grant.mode) {
                    holders.insert(grant.txn);
                }
            }
        }
        let mut holders = holders.into_iter().collect::<Vec<_>>();
        holders.sort_unstable();
        Ok(holders)
    }

    fn txn_holds_write_locks(&self, txn: TxnId) -> DbResult<bool> {
        for shard in &self.shards {
            let state = shard.state.lock();
            if let Some(held) = state.held_by_txn.get(&txn) {
                if held.iter().any(|(target, mode)| {
                    // Tuple grain: `Update` is the canonical write lock,
                    // but `AccessExclusive` (taken by the tuple-delete
                    // hot path) also implies a write - without this case
                    // a txn that ONLY held AccessExclusive tuple locks
                    // would falsely report "no write locks" and could
                    // skip required pre-commit conflict tracking.
                    let tuple_write = matches!(target, LockTarget::Tuple { .. })
                        && matches!(mode, LockMode::Update | LockMode::AccessExclusive);
                    let table_write =
                        matches!(target, LockTarget::Table(_)) && is_write_table_lock_mode(*mode);
                    tuple_write || table_write
                }) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn txn_holds_table_lock(
        &self,
        txn: TxnId,
        table_id: RelationId,
        mode: LockMode,
    ) -> DbResult<bool> {
        let target = LockTarget::Table(table_id);
        let state = self.shard_state(target);
        let held = state
            .held_by_txn
            .get(&txn)
            .and_then(|targets| targets.get(&target).copied());
        Ok(held == Some(mode))
    }

    fn txn_holds_tuple_lock(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        mode: LockMode,
    ) -> DbResult<bool> {
        let target = LockTarget::Tuple { table_id, tuple_id };
        let state = self.shard_state(target);
        let held = state
            .held_by_txn
            .get(&txn)
            .and_then(|targets| targets.get(&target).copied());
        Ok(held == Some(mode))
    }

    fn try_acquire_table_lock_nowait(
        &self,
        txn: TxnId,
        table_id: RelationId,
        mode: LockMode,
    ) -> DbResult<()> {
        let target = LockTarget::Table(table_id);
        let mut state = self.shard_state(target);
        let held_mode = state.held_mode(txn, target);
        let effective_mode = held_mode.map_or(mode, |held| strongest_lock_mode(held, mode));

        if held_mode == Some(effective_mode) {
            state.wait_for.remove(&txn);
            return Ok(());
        }

        let blockers = state.blockers(txn, target, effective_mode);
        if blockers.is_empty() {
            state.grant(txn, target, effective_mode);
            return Ok(());
        }

        state.wait_for.remove(&txn);
        Err(DbError::transaction_error(
            SqlState::LockNotAvailable,
            format!("lock wait timeout exceeded on {target:?}"),
        ))
    }
}

fn strongest_lock_mode(current: LockMode, requested: LockMode) -> LockMode {
    if lock_mode_rank(current) >= lock_mode_rank(requested) {
        current
    } else {
        requested
    }
}

fn lock_mode_rank(mode: LockMode) -> u8 {
    match mode {
        LockMode::AccessShare => 0,
        LockMode::PredicateRead => 1,
        LockMode::KeyShare => 2,
        LockMode::RowExclusive => 3,
        LockMode::Update => 4,
        LockMode::AccessExclusive => 5,
    }
}

fn lock_modes_compatible(left: LockMode, right: LockMode) -> bool {
    !matches!(
        (left, right),
        (LockMode::AccessExclusive, _)
            | (_, LockMode::AccessExclusive)
            | (LockMode::Update, LockMode::Update)
            | (LockMode::Update, LockMode::KeyShare)
            | (LockMode::KeyShare, LockMode::Update)
            | (LockMode::PredicateRead, LockMode::RowExclusive)
            | (LockMode::PredicateRead, LockMode::Update)
            | (LockMode::RowExclusive, LockMode::PredicateRead)
            | (LockMode::Update, LockMode::PredicateRead)
    )
}

fn is_write_table_lock_mode(mode: LockMode) -> bool {
    matches!(
        mode,
        LockMode::RowExclusive | LockMode::Update | LockMode::AccessExclusive
    )
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{mpsc, Arc, Barrier},
        thread,
        time::Duration,
    };

    use super::*;

    fn noop() -> NoopLockManager {
        NoopLockManager
    }

    struct AcquireOnlyLockManager;

    impl LockManager for AcquireOnlyLockManager {
        fn acquire_table_lock(
            &self,
            _txn: TxnId,
            _table_id: RelationId,
            _mode: LockMode,
        ) -> DbResult<()> {
            Ok(())
        }

        fn acquire_tuple_lock(
            &self,
            _txn: TxnId,
            _table_id: RelationId,
            _tuple_id: TupleId,
            _mode: LockMode,
        ) -> DbResult<()> {
            Ok(())
        }
    }

    // --- acquire_table_lock always returns Ok ---
    #[test]
    fn acquire_table_lock_always_ok() {
        let lm = noop();
        let result =
            lm.acquire_table_lock(TxnId::new(1), RelationId::new(10), LockMode::AccessShare);
        assert!(result.is_ok());
    }

    // --- acquire_tuple_lock always returns Ok ---
    #[test]
    fn acquire_tuple_lock_always_ok() {
        let lm = noop();
        let result = lm.acquire_tuple_lock(
            TxnId::new(1),
            RelationId::new(10),
            TupleId::new(42),
            LockMode::RowExclusive,
        );
        assert!(result.is_ok());
    }

    // --- LockManager trait is object-safe ---
    #[test]
    fn lock_manager_trait_is_object_safe() {
        let lm = noop();
        let dyn_ref: &dyn LockManager = &lm;
        assert!(dyn_ref
            .acquire_table_lock(TxnId::new(1), RelationId::new(1), LockMode::AccessShare)
            .is_ok());
        assert!(dyn_ref
            .acquire_tuple_lock(
                TxnId::new(1),
                RelationId::new(1),
                TupleId::new(1),
                LockMode::Update,
            )
            .is_ok());
        assert!(dyn_ref.release_txn(TxnId::new(1)).is_ok());
    }

    #[test]
    fn default_nowait_table_lock_fails_closed() {
        let lm = AcquireOnlyLockManager;
        let error = lm
            .try_acquire_table_lock_nowait(TxnId::new(1), RelationId::new(1), LockMode::AccessShare)
            .expect_err("default NOWAIT must not silently block");
        assert_eq!(error.sqlstate(), SqlState::FeatureNotSupported);
    }

    #[test]
    fn default_nowait_tuple_lock_fails_closed() {
        let lm = AcquireOnlyLockManager;
        let error = lm
            .try_acquire_tuple_lock_nowait(
                TxnId::new(1),
                RelationId::new(1),
                TupleId::new(1),
                LockMode::AccessShare,
            )
            .expect_err("default NOWAIT must not silently block");
        assert_eq!(error.sqlstate(), SqlState::FeatureNotSupported);
    }

    #[test]
    fn noop_nowait_lock_paths_succeed() {
        let lm = noop();
        assert!(
            lm.try_acquire_table_lock_nowait(
                TxnId::new(1),
                RelationId::new(1),
                LockMode::AccessShare,
            )
            .is_ok()
        );
        assert!(lm
            .try_acquire_tuple_lock_nowait(
                TxnId::new(1),
                RelationId::new(1),
                TupleId::new(1),
                LockMode::Update,
            )
            .is_ok());
    }

    // --- Same lock acquired twice on same table succeeds ---
    #[test]
    fn duplicate_table_lock_succeeds() {
        let lm = WaitGraphLockManager::default();
        let txn = TxnId::new(1);
        let table = RelationId::new(5);
        assert!(lm
            .acquire_table_lock(txn, table, LockMode::AccessShare)
            .is_ok());
        assert!(lm
            .acquire_table_lock(txn, table, LockMode::AccessExclusive)
            .is_ok());
    }

    // --- Compatible table locks can coexist ---
    #[test]
    fn compatible_table_locks_succeed() {
        let lm = WaitGraphLockManager::default();
        let table = RelationId::new(5);
        assert!(lm
            .acquire_table_lock(TxnId::new(1), table, LockMode::AccessShare)
            .is_ok());
        assert!(lm
            .acquire_table_lock(TxnId::new(2), table, LockMode::RowExclusive)
            .is_ok());
    }

    // --- Conflicting lock blocks until release ---
    #[test]
    fn conflicting_table_lock_waits_until_release() {
        let lm = Arc::new(WaitGraphLockManager::default());
        let table = RelationId::new(7);
        lm.acquire_table_lock(TxnId::new(1), table, LockMode::AccessExclusive)
            .unwrap();

        let (sender, receiver) = mpsc::channel();
        let worker_lm = lm.clone();
        let handle = thread::spawn(move || {
            worker_lm
                .acquire_table_lock(TxnId::new(2), table, LockMode::AccessShare)
                .unwrap();
            sender.send(()).unwrap();
            worker_lm.release_txn(TxnId::new(2)).unwrap();
        });

        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
        lm.release_txn(TxnId::new(1)).unwrap();
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn lock_wait_deadline_handles_unrepresentable_timeout() {
        let deadline = lock_wait_deadline(Duration::MAX);
        assert!(deadline.is_none());
        assert_eq!(lock_wait_remaining(deadline), MAX_LOCK_WAIT_CHUNK);
        assert!(!lock_wait_deadline_expired(deadline));
    }

    #[test]
    fn conflicting_table_lock_with_unrepresentable_timeout_waits_until_release() {
        let lm = Arc::new(WaitGraphLockManager::default());
        let table = RelationId::new(17);
        let waiter = TxnId::new(2);
        lm.acquire_table_lock(TxnId::new(1), table, LockMode::AccessExclusive)
            .unwrap();
        lm.set_txn_lock_timeout(waiter, Duration::MAX);

        let (sender, receiver) = mpsc::channel();
        let worker_lm = Arc::clone(&lm);
        let handle = thread::spawn(move || {
            let result = worker_lm.acquire_table_lock(waiter, table, LockMode::AccessShare);
            sender.send(result).unwrap();
            worker_lm.release_txn(waiter).unwrap();
        });

        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
        lm.release_txn(TxnId::new(1)).unwrap();
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn predicate_read_blocks_row_exclusive_until_release() {
        let lm = Arc::new(WaitGraphLockManager::default());
        let table = RelationId::new(77);
        lm.acquire_table_lock(TxnId::new(1), table, LockMode::PredicateRead)
            .unwrap();

        let (sender, receiver) = mpsc::channel();
        let worker_lm = lm.clone();
        let handle = thread::spawn(move || {
            worker_lm
                .acquire_table_lock(TxnId::new(2), table, LockMode::RowExclusive)
                .unwrap();
            sender.send(()).unwrap();
            worker_lm.release_txn(TxnId::new(2)).unwrap();
        });

        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
        lm.release_txn(TxnId::new(1)).unwrap();
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        handle.join().unwrap();
    }

    // --- Conflicting tuple updates detect deadlock ---
    #[test]
    fn detects_deadlock_between_tuple_updates() {
        let lm = Arc::new(WaitGraphLockManager::default());
        let barrier = Arc::new(Barrier::new(2));
        let table = RelationId::new(9);
        let first_tuple = TupleId::new(1);
        let second_tuple = TupleId::new(2);
        let (sender, receiver) = mpsc::channel();

        let first_lm = lm.clone();
        let first_barrier = barrier.clone();
        let first_sender = sender.clone();
        let first = thread::spawn(move || {
            let txn = TxnId::new(1);
            first_lm
                .acquire_tuple_lock(txn, table, first_tuple, LockMode::Update)
                .unwrap();
            first_barrier.wait();
            let result = first_lm.acquire_tuple_lock(txn, table, second_tuple, LockMode::Update);
            first_sender.send(result.map(|()| txn)).unwrap();
            first_lm.release_txn(txn).unwrap();
        });

        let second_lm = lm.clone();
        let second_barrier = barrier.clone();
        let second = thread::spawn(move || {
            let txn = TxnId::new(2);
            second_lm
                .acquire_tuple_lock(txn, table, second_tuple, LockMode::Update)
                .unwrap();
            second_barrier.wait();
            thread::sleep(Duration::from_millis(50));
            let result = second_lm.acquire_tuple_lock(txn, table, first_tuple, LockMode::Update);
            sender.send(result.map(|()| txn)).unwrap();
            second_lm.release_txn(txn).unwrap();
        });

        let first_result = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        let second_result = receiver.recv_timeout(Duration::from_secs(1)).unwrap();

        let deadlock_detected = [first_result, second_result]
            .into_iter()
            .filter_map(Result::err)
            .any(|error| error.sqlstate() == SqlState::DeadlockDetected);
        assert!(deadlock_detected);

        first.join().unwrap();
        second.join().unwrap();
    }

    // --- release_txn clears held locks and wait edges ---
    #[test]
    fn release_txn_is_idempotent() {
        let lm = WaitGraphLockManager::default();
        let txn = TxnId::new(1);
        let table = RelationId::new(1);
        lm.acquire_table_lock(txn, table, LockMode::AccessShare)
            .unwrap();
        lm.release_txn(txn).unwrap();
        lm.release_txn(txn).unwrap();
        lm.acquire_table_lock(TxnId::new(2), table, LockMode::AccessExclusive)
            .unwrap();
    }

    #[test]
    fn release_txn_clears_timeout_override() {
        let lm = WaitGraphLockManager::default();
        let txn = TxnId::new(42);
        let custom = Duration::from_millis(250);

        lm.set_txn_lock_timeout(txn, custom);
        assert_eq!(lm.effective_lock_timeout(txn), custom);

        lm.release_txn(txn).unwrap();
        assert_eq!(lm.effective_lock_timeout(txn), DEFAULT_LOCK_TIMEOUT);
    }

    #[test]
    fn txn_holds_tuple_lock_reports_update_grant() {
        let lm = WaitGraphLockManager::default();
        let txn = TxnId::new(42);
        let table = RelationId::new(7);
        let tuple = TupleId::new(3);

        assert!(!lm
            .txn_holds_tuple_lock(txn, table, tuple, LockMode::Update)
            .unwrap());
        lm.acquire_tuple_lock(txn, table, tuple, LockMode::Update)
            .unwrap();
        assert!(lm
            .txn_holds_tuple_lock(txn, table, tuple, LockMode::Update)
            .unwrap());

        lm.release_txn(txn).unwrap();
        assert!(!lm
            .txn_holds_tuple_lock(txn, table, tuple, LockMode::Update)
            .unwrap());
    }

    // parking_lot::Mutex has no poisoning, so the previous panic-recovery
    // tests no longer apply. Keep coverage of the timeout-override lifecycle
    // through the regular paths (`release_txn_clears_timeout_override`).

    // --- WaitGraphLockManager implements Debug ---
    #[test]
    fn wait_graph_lock_manager_implements_debug() {
        let lm = WaitGraphLockManager::default();
        let dbg = format!("{lm:?}");
        assert!(dbg.contains("WaitGraphLockManager"));
    }
}
