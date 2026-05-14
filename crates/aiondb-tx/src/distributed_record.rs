//! Distributed transaction record.
//!
//! CockroachDB tracks every multi-range write transaction in a single
//! durable **transaction record**: a row that lives at a well-known
//! key under the coordinator's first range. Every write the txn
//! performs leaves an *intent* (uncommitted KV pair) whose visibility
//! is dictated entirely by the status of that txn record. Resolving
//! an intent therefore requires looking up its record:
//!
//! - **Committed** : the intent is promoted to a visible value.
//! - **Aborted** : the intent is garbage-collected.
//! - **Pending** : the reader must either wait or push the writer.
//!
//! This module implements the in-memory side of that bookkeeping: a
//! [`DistributedTxnRegistry`] mapping each [`DistributedTxnId`] to its
//! current [`DistributedTxnRecord`]. The durable side belongs to the
//! storage / replication layer; the registry exists so all nodes can
//! agree on the live state without consulting the disk on every
//! check.
//!
//! # State machine
//!
//! ```text
//!                   start
//!                     │
//!                     ▼
//!                 ┌────────┐
//!         abort   │ Pending│  heartbeat
//!         ┌───────│        │◀───────────┐
//!         │       └────┬───┘            │
//!         │            │ stage          │
//!         │            ▼                │
//!         │       ┌────────┐            │
//!         │       │Staging │────────────┘
//!         │       └────┬───┘
//!         │            │ finalize-commit
//!         ▼            ▼
//!     ┌────────┐  ┌──────────┐
//!     │Aborted │  │Committed │
//!     └────────┘  └──────────┘
//! ```
//!
//! - `Pending → Staging` : every write has been buffered, coordinator
//!   is starting the implicit commit (Cockroach "parallel commits").
//! - `Staging → Committed` : intents resolved, durable record marked
//!   final.
//! - `Pending → Aborted` or `Staging → Aborted` : explicit rollback or
//!   forced abort by an interloping pusher.
//! - Heartbeat extends the deadline by `heartbeat_period` so dead
//!   coordinators eventually expire and let pushers commit-or-abort
//!   the orphaned record.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aiondb_core::{DbError, DbResult};

use crate::hlc::HlcTimestamp;

/// Default coordinator heartbeat period. Coordinators MUST refresh the
/// record at least once per period or pushers may declare them dead.
pub const DEFAULT_HEARTBEAT_PERIOD: Duration = Duration::from_secs(1);

/// Default expiration interval. The record is treated as orphaned
/// when `now - last_heartbeat > DEFAULT_EXPIRATION`.
pub const DEFAULT_EXPIRATION: Duration = Duration::from_secs(5);

/// Distributed transaction identifier. Globally unique. The `seq`
/// field disambiguates same-coordinator transactions that started at
/// the same HLC microsecond.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DistributedTxnId {
    /// Identifier of the coordinator node that began the transaction.
    pub coordinator: u64,
    /// HLC timestamp when the transaction started.
    pub start_ts: HlcTimestamp,
    /// Disambiguation counter within the same `(coordinator, start_ts)`.
    pub seq: u32,
}

impl std::fmt::Display for DistributedTxnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "txn-{}-{}.{}-{}",
            self.coordinator, self.start_ts.wall_time_us, self.start_ts.logical, self.seq
        )
    }
}

/// Possible states of a distributed transaction record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DistributedTxnStatus {
    /// The coordinator is still writing intents.
    Pending,
    /// Every intent has been laid down; the coordinator is staging an
    /// implicit commit. A reader that sees a Staging record must
    /// recover-or-roll it forward (Cockroach "parallel commits"
    /// recovery).
    Staging,
    /// Final: every intent must be promoted to a visible value.
    Committed,
    /// Final: every intent must be garbage-collected.
    Aborted,
}

impl DistributedTxnStatus {
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Committed | Self::Aborted)
    }
}

/// A pair of keys describing a key span the transaction has written
/// to. Used during recovery to locate intents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeySpan {
    pub start_key: Vec<u8>,
    pub end_key: Vec<u8>,
}

/// Distributed transaction record. Cloneable so callers can inspect a
/// snapshot without holding the registry mutex.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DistributedTxnRecord {
    pub id: DistributedTxnId,
    pub status: DistributedTxnStatus,
    /// HLC of the most recent heartbeat. The record is considered
    /// orphaned when `now - last_heartbeat > expiration`.
    pub last_heartbeat: HlcTimestamp,
    /// Commit timestamp once `status = Committed`. Zero otherwise.
    pub commit_ts: HlcTimestamp,
    /// Spans the transaction will write to. Reported up-front so the
    /// recovery flow knows where to look for intents.
    pub write_spans: Vec<KeySpan>,
    /// Spans the transaction read. Used by serialisable refresh.
    pub read_spans: Vec<KeySpan>,
    /// Priority class. Higher values preempt lower values when two
    /// transactions contend.
    pub priority: u32,
}

impl DistributedTxnRecord {
    fn new(id: DistributedTxnId, now: HlcTimestamp, priority: u32) -> Self {
        Self {
            id,
            status: DistributedTxnStatus::Pending,
            last_heartbeat: now,
            commit_ts: HlcTimestamp::ZERO,
            write_spans: Vec::new(),
            read_spans: Vec::new(),
            priority,
        }
    }
}

/// Reasons a state transition was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxnTransitionError {
    /// The record does not exist (never started, or already cleaned up).
    Unknown,
    /// The transition is illegal from the record's current state.
    InvalidTransition {
        from: DistributedTxnStatus,
        to: DistributedTxnStatus,
    },
}

impl std::fmt::Display for TxnTransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => write!(f, "unknown transaction"),
            Self::InvalidTransition { from, to } => {
                write!(f, "invalid transition {:?} -> {:?}", from, to)
            }
        }
    }
}

impl std::error::Error for TxnTransitionError {}

impl TxnTransitionError {
    /// Convert into a generic `DbError` for callers that need a uniform
    /// error type. Kept as an explicit method rather than a `From` impl
    /// to avoid widening the set of types convertible into `DbError`,
    /// which can break `Result<_, _>` type inference at downstream
    /// call sites.
    pub fn into_db_error(self) -> DbError {
        DbError::internal(self.to_string())
    }
}

/// In-memory registry of distributed transaction records.
#[derive(Clone, Debug, Default)]
pub struct DistributedTxnRegistry {
    inner: Arc<std::sync::Mutex<HashMap<DistributedTxnId, DistributedTxnRecord>>>,
    expiration_us: u64,
}

impl DistributedTxnRegistry {
    pub fn new() -> Self {
        Self::with_expiration(DEFAULT_EXPIRATION)
    }

    pub fn with_expiration(expiration: Duration) -> Self {
        Self {
            inner: Arc::default(),
            expiration_us: u64::try_from(expiration.as_micros()).unwrap_or(u64::MAX),
        }
    }

    /// Register a fresh transaction. Returns `false` (no-op) when the
    /// id is already present, which only happens on coordinator crash
    /// + restart with the same id.
    pub fn register(&self, id: DistributedTxnId, now: HlcTimestamp, priority: u32) -> bool {
        let mut guard = self.lock();
        if guard.contains_key(&id) {
            return false;
        }
        guard.insert(id, DistributedTxnRecord::new(id, now, priority));
        true
    }

    /// Fetch a record snapshot.
    pub fn get(&self, id: DistributedTxnId) -> Option<DistributedTxnRecord> {
        self.lock().get(&id).cloned()
    }

    /// Refresh the heartbeat timestamp. Returns `Err(Unknown)` when the
    /// record was already cleaned up. Once the record is `Committed` /
    /// `Aborted` the heartbeat is ignored but still returns success
    /// (idempotent so coordinator's last heartbeat can race with its
    /// own commit).
    pub fn heartbeat(
        &self,
        id: DistributedTxnId,
        now: HlcTimestamp,
    ) -> Result<DistributedTxnRecord, TxnTransitionError> {
        let mut guard = self.lock();
        let record = guard.get_mut(&id).ok_or(TxnTransitionError::Unknown)?;
        if !record.status.is_final() && now > record.last_heartbeat {
            record.last_heartbeat = now;
        }
        Ok(record.clone())
    }

    /// Add or replace the write spans for a record.
    pub fn declare_write_spans(
        &self,
        id: DistributedTxnId,
        spans: Vec<KeySpan>,
    ) -> Result<(), TxnTransitionError> {
        let mut guard = self.lock();
        let record = guard.get_mut(&id).ok_or(TxnTransitionError::Unknown)?;
        if record.status.is_final() {
            return Err(TxnTransitionError::InvalidTransition {
                from: record.status,
                to: record.status,
            });
        }
        record.write_spans = spans;
        Ok(())
    }

    /// Add or replace the read spans for a record.
    pub fn declare_read_spans(
        &self,
        id: DistributedTxnId,
        spans: Vec<KeySpan>,
    ) -> Result<(), TxnTransitionError> {
        let mut guard = self.lock();
        let record = guard.get_mut(&id).ok_or(TxnTransitionError::Unknown)?;
        if record.status.is_final() {
            return Err(TxnTransitionError::InvalidTransition {
                from: record.status,
                to: record.status,
            });
        }
        record.read_spans = spans;
        Ok(())
    }

    /// Transition Pending → Staging. Idempotent.
    pub fn stage(&self, id: DistributedTxnId) -> Result<DistributedTxnRecord, TxnTransitionError> {
        let mut guard = self.lock();
        let record = guard.get_mut(&id).ok_or(TxnTransitionError::Unknown)?;
        match record.status {
            DistributedTxnStatus::Pending | DistributedTxnStatus::Staging => {
                record.status = DistributedTxnStatus::Staging;
                Ok(record.clone())
            }
            DistributedTxnStatus::Committed | DistributedTxnStatus::Aborted => {
                Err(TxnTransitionError::InvalidTransition {
                    from: record.status,
                    to: DistributedTxnStatus::Staging,
                })
            }
        }
    }

    /// Transition (Pending|Staging) → Committed with `commit_ts`.
    pub fn commit(
        &self,
        id: DistributedTxnId,
        commit_ts: HlcTimestamp,
    ) -> Result<DistributedTxnRecord, TxnTransitionError> {
        let mut guard = self.lock();
        let record = guard.get_mut(&id).ok_or(TxnTransitionError::Unknown)?;
        match record.status {
            DistributedTxnStatus::Pending | DistributedTxnStatus::Staging => {
                record.status = DistributedTxnStatus::Committed;
                record.commit_ts = commit_ts;
                Ok(record.clone())
            }
            DistributedTxnStatus::Committed => {
                if record.commit_ts != commit_ts {
                    return Err(TxnTransitionError::InvalidTransition {
                        from: record.status,
                        to: DistributedTxnStatus::Committed,
                    });
                }
                Ok(record.clone())
            }
            DistributedTxnStatus::Aborted => Err(TxnTransitionError::InvalidTransition {
                from: record.status,
                to: DistributedTxnStatus::Committed,
            }),
        }
    }

    /// Transition (Pending|Staging) → Aborted. Idempotent on already-
    /// aborted records. Forbidden from Committed.
    pub fn abort(&self, id: DistributedTxnId) -> Result<DistributedTxnRecord, TxnTransitionError> {
        let mut guard = self.lock();
        let record = guard.get_mut(&id).ok_or(TxnTransitionError::Unknown)?;
        match record.status {
            DistributedTxnStatus::Pending | DistributedTxnStatus::Staging => {
                record.status = DistributedTxnStatus::Aborted;
                Ok(record.clone())
            }
            DistributedTxnStatus::Aborted => Ok(record.clone()),
            DistributedTxnStatus::Committed => Err(TxnTransitionError::InvalidTransition {
                from: record.status,
                to: DistributedTxnStatus::Aborted,
            }),
        }
    }

    /// Remove a final record from memory. Returns the removed record.
    /// Refuses to clean up non-final records to avoid losing in-flight
    /// state through API misuse.
    pub fn forget(&self, id: DistributedTxnId) -> Result<DistributedTxnRecord, TxnTransitionError> {
        let mut guard = self.lock();
        let record = guard.get(&id).ok_or(TxnTransitionError::Unknown)?;
        if !record.status.is_final() {
            return Err(TxnTransitionError::InvalidTransition {
                from: record.status,
                to: record.status,
            });
        }
        Ok(guard.remove(&id).expect("present"))
    }

    /// Return every non-final record whose heartbeat is older than
    /// `expiration`. These records are candidates for push: an
    /// interloper reader can attempt to abort them.
    pub fn expired(&self, now: HlcTimestamp) -> Vec<DistributedTxnRecord> {
        let guard = self.lock();
        let mut out: Vec<_> = guard
            .values()
            .filter(|r| !r.status.is_final())
            .filter(|r| {
                let elapsed = now
                    .wall_time_us
                    .saturating_sub(r.last_heartbeat.wall_time_us);
                elapsed > self.expiration_us
            })
            .cloned()
            .collect();
        out.sort_by_key(|r| r.id);
        out
    }

    /// Push a contender: attempt to abort a transaction whose
    /// priority is lower than `pusher_priority`. Returns the resulting
    /// record (Aborted on success, unchanged otherwise).
    pub fn push(
        &self,
        target: DistributedTxnId,
        pusher_priority: u32,
        now: HlcTimestamp,
    ) -> Result<DistributedTxnRecord, TxnTransitionError> {
        let mut guard = self.lock();
        let record = guard.get_mut(&target).ok_or(TxnTransitionError::Unknown)?;
        if record.status.is_final() {
            return Ok(record.clone());
        }
        let heartbeat_age = now
            .wall_time_us
            .saturating_sub(record.last_heartbeat.wall_time_us);
        let expired = heartbeat_age > self.expiration_us;
        if pusher_priority > record.priority || expired {
            record.status = DistributedTxnStatus::Aborted;
        }
        Ok(record.clone())
    }

    /// Snapshot every record in the registry.
    pub fn snapshot(&self) -> Vec<DistributedTxnRecord> {
        let mut out: Vec<_> = self.lock().values().cloned().collect();
        out.sort_by_key(|r| r.id);
        out
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    pub fn expiration(&self) -> Duration {
        Duration::from_micros(self.expiration_us)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<DistributedTxnId, DistributedTxnRecord>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Hand-rolled `DbResult` adaptor for callers that prefer a single error
/// type instead of `TxnTransitionError`.
impl DistributedTxnRegistry {
    pub fn try_register(
        &self,
        id: DistributedTxnId,
        now: HlcTimestamp,
        priority: u32,
    ) -> DbResult<()> {
        if self.register(id, now, priority) {
            Ok(())
        } else {
            Err(DbError::internal(format!(
                "transaction {id} is already registered"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(coord: u64, wall: u64, seq: u32) -> DistributedTxnId {
        DistributedTxnId {
            coordinator: coord,
            start_ts: HlcTimestamp::new(wall, 0),
            seq,
        }
    }

    fn now(wall: u64) -> HlcTimestamp {
        HlcTimestamp::new(wall, 0)
    }

    #[test]
    fn register_then_get_returns_pending() {
        let r = DistributedTxnRegistry::new();
        let t = id(1, 100, 0);
        assert!(r.register(t, now(100), 1));
        let rec = r.get(t).expect("registered");
        assert_eq!(rec.status, DistributedTxnStatus::Pending);
        assert_eq!(rec.priority, 1);
    }

    #[test]
    fn register_twice_is_rejected() {
        let r = DistributedTxnRegistry::new();
        let t = id(1, 100, 0);
        assert!(r.register(t, now(100), 1));
        assert!(!r.register(t, now(200), 1));
    }

    #[test]
    fn heartbeat_only_advances() {
        let r = DistributedTxnRegistry::new();
        let t = id(1, 100, 0);
        r.register(t, now(100), 1);
        r.heartbeat(t, now(200)).unwrap();
        // Older heartbeats are ignored.
        r.heartbeat(t, now(150)).unwrap();
        let rec = r.get(t).unwrap();
        assert_eq!(rec.last_heartbeat, now(200));
    }

    #[test]
    fn pending_to_staging_to_committed_path() {
        let r = DistributedTxnRegistry::new();
        let t = id(1, 100, 0);
        r.register(t, now(100), 1);
        r.stage(t).unwrap();
        let rec = r.commit(t, now(500)).unwrap();
        assert_eq!(rec.status, DistributedTxnStatus::Committed);
        assert_eq!(rec.commit_ts, now(500));
    }

    #[test]
    fn pending_to_aborted_skipping_staging() {
        let r = DistributedTxnRegistry::new();
        let t = id(1, 100, 0);
        r.register(t, now(100), 1);
        let rec = r.abort(t).unwrap();
        assert_eq!(rec.status, DistributedTxnStatus::Aborted);
    }

    #[test]
    fn cannot_abort_committed() {
        let r = DistributedTxnRegistry::new();
        let t = id(1, 100, 0);
        r.register(t, now(100), 1);
        r.commit(t, now(500)).unwrap();
        let err = r.abort(t).expect_err("must reject");
        assert!(matches!(
            err,
            TxnTransitionError::InvalidTransition {
                from: DistributedTxnStatus::Committed,
                to: DistributedTxnStatus::Aborted
            }
        ));
    }

    #[test]
    fn cannot_commit_aborted() {
        let r = DistributedTxnRegistry::new();
        let t = id(1, 100, 0);
        r.register(t, now(100), 1);
        r.abort(t).unwrap();
        assert!(matches!(
            r.commit(t, now(500)),
            Err(TxnTransitionError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn commit_idempotent_with_same_ts_and_rejects_different_ts() {
        let r = DistributedTxnRegistry::new();
        let t = id(1, 100, 0);
        r.register(t, now(100), 1);
        r.commit(t, now(500)).unwrap();
        r.commit(t, now(500)).expect("idempotent");
        assert!(matches!(
            r.commit(t, now(600)),
            Err(TxnTransitionError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn declare_spans_records_them() {
        let r = DistributedTxnRegistry::new();
        let t = id(1, 100, 0);
        r.register(t, now(100), 1);
        let spans = vec![
            KeySpan {
                start_key: b"a".to_vec(),
                end_key: b"m".to_vec(),
            },
            KeySpan {
                start_key: b"m".to_vec(),
                end_key: b"z".to_vec(),
            },
        ];
        r.declare_write_spans(t, spans.clone()).unwrap();
        assert_eq!(r.get(t).unwrap().write_spans, spans);
    }

    #[test]
    fn expired_returns_only_stale_non_final_records() {
        let r = DistributedTxnRegistry::with_expiration(Duration::from_micros(100));
        let live = id(1, 100, 0);
        let dead = id(2, 100, 0);
        let done = id(3, 100, 0);
        r.register(live, now(1000), 1);
        r.register(dead, now(100), 1);
        r.register(done, now(100), 1);
        r.commit(done, now(2000)).unwrap();
        let stale = r.expired(now(500));
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].id, dead);
    }

    #[test]
    fn push_aborts_lower_priority_target() {
        let r = DistributedTxnRegistry::new();
        let target = id(1, 100, 0);
        r.register(target, now(100), 1);
        // Higher-priority pusher aborts.
        let rec = r.push(target, 10, now(100)).unwrap();
        assert_eq!(rec.status, DistributedTxnStatus::Aborted);
    }

    #[test]
    fn push_leaves_higher_priority_target_intact() {
        let r = DistributedTxnRegistry::new();
        let target = id(1, 100, 0);
        r.register(target, now(100), 100);
        let rec = r.push(target, 5, now(100)).unwrap();
        assert_eq!(rec.status, DistributedTxnStatus::Pending);
    }

    #[test]
    fn push_aborts_expired_target_regardless_of_priority() {
        let r = DistributedTxnRegistry::with_expiration(Duration::from_micros(100));
        let target = id(1, 100, 0);
        r.register(target, now(100), 999);
        // Expired -> abort even with lower-priority pusher.
        let rec = r.push(target, 1, now(1000)).unwrap();
        assert_eq!(rec.status, DistributedTxnStatus::Aborted);
    }

    #[test]
    fn forget_only_removes_final_records() {
        let r = DistributedTxnRegistry::new();
        let t = id(1, 100, 0);
        r.register(t, now(100), 1);
        assert!(matches!(
            r.forget(t),
            Err(TxnTransitionError::InvalidTransition { .. })
        ));
        r.commit(t, now(500)).unwrap();
        let rec = r.forget(t).unwrap();
        assert_eq!(rec.status, DistributedTxnStatus::Committed);
        assert!(r.get(t).is_none());
    }

    #[test]
    fn snapshot_is_sorted_by_id() {
        let r = DistributedTxnRegistry::new();
        r.register(id(3, 100, 0), now(100), 1);
        r.register(id(1, 100, 0), now(100), 1);
        r.register(id(2, 100, 0), now(100), 1);
        let snap = r.snapshot();
        let coords: Vec<u64> = snap.iter().map(|r| r.id.coordinator).collect();
        assert_eq!(coords, vec![1, 2, 3]);
    }
}
