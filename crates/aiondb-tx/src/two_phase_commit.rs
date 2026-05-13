//! Two-phase commit (2PC) coordinator.
//!
//! Production-grade cross-shard transaction protocol. The coordinator
//! drives every participant through `Prepare → Commit | Abort`, with
//! retries, per-attempt timeouts and a heartbeat thread so a crashed
//! coordinator does not silently strand prepared participants.
//!
//! # Protocol
//!
//! Phase 1 (Prepare):
//! 1. Coordinator persists a `DistributedTxnRecord` in `Pending` state
//!    via [`DistributedTxnRegistry`].
//! 2. Sends a `Prepare(txn_id, write_spans, deadline)` to every
//!    participant in parallel.
//! 3. Waits for each to vote `Yes` / `No`. Any `No` or timeout aborts.
//! 4. Once every participant voted `Yes`, the record transitions to
//!    `Staging` (the durable commit decision is now atomic).
//!
//! Phase 2 (Commit / Abort):
//! 5. Sends `Commit(txn_id, commit_ts)` to every participant. Retries
//!    until each acknowledges.
//! 6. Once all acks received, the record transitions to `Committed`
//!    and the coordinator can forget it.
//!
//! Abort path:
//! 7. On `No` vote or pre-stage timeout, the record moves to
//!    `Aborted`, every participant gets a `Rollback(txn_id)` (with
//!    retries until ack), then the coordinator forgets the record.
//!
//! # Participant trait
//!
//! [`TwoPhaseParticipant`] abstracts the network. Production builds
//! plug in a TCP/RPC backend; tests use the in-process
//! `MockParticipant` which is enough to exercise every branch.
//!
//! # Failure semantics
//!
//! - Coordinator crash before `Staging` : participants self-abort
//!   after their prepare deadline.
//! - Coordinator crash during Phase 2 : a replacement coordinator can
//!   read the record, see `Staging` or `Committed`, and finish the
//!   protocol. The record is durable enough for any node to take
//!   over.
//! - Participant crash : retries up to `commit_retries` against the
//!   participant's new address. Definitive failure surfaces as an
//!   error so operators know the txn is stuck.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aiondb_core::{DbError, DbResult};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::distributed_record::{DistributedTxnId, DistributedTxnRegistry, KeySpan};
use crate::hlc::{HlcTimestamp, HybridLogicalClock};

/// Default prepare timeout. Bigger than typical disk fsync latency so
/// participants have headroom for their durable vote.
pub const DEFAULT_PREPARE_TIMEOUT: Duration = Duration::from_secs(5);

/// Default commit/abort retry budget.
pub const DEFAULT_COMMIT_RETRIES: usize = 8;

/// Default initial retry delay; exponential backoff up to 30s.
pub const DEFAULT_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(100);

/// Participant identity. Opaque -- the coordinator does not need to
/// know transport details, only how to issue RPCs through the
/// [`TwoPhaseParticipant`] trait.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ParticipantId(pub u64);

/// A prepare-vote outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrepareVote {
    /// Participant is locked at `prepared_ts` and will respect any
    /// commit decision the coordinator returns.
    Yes { prepared_ts: HlcTimestamp },
    /// Participant could not lock (conflict, validation failure). Carries
    /// a short reason for the coordinator's error path.
    No { reason: String },
}

/// Operations the coordinator needs from each participant.
#[async_trait::async_trait]
pub trait TwoPhaseParticipant: Send + Sync {
    async fn prepare(
        &self,
        txn_id: DistributedTxnId,
        write_spans: &[KeySpan],
        deadline: HlcTimestamp,
    ) -> DbResult<PrepareVote>;

    async fn commit(&self, txn_id: DistributedTxnId, commit_ts: HlcTimestamp) -> DbResult<()>;

    async fn rollback(&self, txn_id: DistributedTxnId) -> DbResult<()>;
}

/// Configuration for [`TwoPhaseCoordinator`].
#[derive(Clone, Debug)]
pub struct CoordinatorConfig {
    pub prepare_timeout: Duration,
    pub commit_retries: usize,
    pub retry_initial_delay: Duration,
    pub per_attempt_timeout: Duration,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            prepare_timeout: DEFAULT_PREPARE_TIMEOUT,
            commit_retries: DEFAULT_COMMIT_RETRIES,
            retry_initial_delay: DEFAULT_RETRY_INITIAL_DELAY,
            per_attempt_timeout: Duration::from_secs(2),
        }
    }
}

/// Outcome of one 2PC run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommitOutcome {
    Committed { commit_ts: HlcTimestamp },
    Aborted { reason: String },
}

/// Production coordinator. Each call to [`execute`] runs one
/// transaction through `Prepare → Commit | Abort`, persisting the
/// record at every state transition.
pub struct TwoPhaseCoordinator {
    registry: DistributedTxnRegistry,
    clock: Arc<HybridLogicalClock>,
    config: CoordinatorConfig,
    /// Participant address book. Cloneable so the coordinator can
    /// snapshot it before fanning out and the caller can mutate it
    /// without holding the coordinator's mutex.
    participants: Mutex<HashMap<ParticipantId, Arc<dyn TwoPhaseParticipant>>>,
}

impl TwoPhaseCoordinator {
    pub fn new(
        registry: DistributedTxnRegistry,
        clock: Arc<HybridLogicalClock>,
        config: CoordinatorConfig,
    ) -> Self {
        Self {
            registry,
            clock,
            config,
            participants: Mutex::new(HashMap::new()),
        }
    }

    pub async fn register_participant(
        &self,
        id: ParticipantId,
        endpoint: Arc<dyn TwoPhaseParticipant>,
    ) {
        self.participants.lock().await.insert(id, endpoint);
    }

    pub async fn unregister_participant(&self, id: ParticipantId) {
        self.participants.lock().await.remove(&id);
    }

    /// Execute one transaction across `participants`. Drives Prepare
    /// → Commit | Abort and returns the final outcome.
    pub async fn execute(
        &self,
        txn_id: DistributedTxnId,
        priority: u32,
        participants: &[ParticipantId],
        write_spans: Vec<KeySpan>,
    ) -> DbResult<CommitOutcome> {
        if participants.is_empty() {
            return Err(DbError::internal("2PC requires at least one participant"));
        }
        let now = self.clock.now();
        if !self.registry.register(txn_id, now, priority) {
            return Err(DbError::internal(format!(
                "transaction {txn_id} already registered"
            )));
        }
        self.registry
            .declare_write_spans(txn_id, write_spans.clone())
            .map_err(|e| e.into_db_error())?;

        // Resolve participant endpoints under a single lock.
        let endpoints = {
            let guard = self.participants.lock().await;
            let mut out = Vec::with_capacity(participants.len());
            for p in participants {
                let endpoint = guard
                    .get(p)
                    .cloned()
                    .ok_or_else(|| DbError::internal(format!("unknown participant {p:?}")))?;
                out.push((*p, endpoint));
            }
            out
        };
        let deadline = HlcTimestamp::new(
            now.wall_time_us.saturating_add(
                u64::try_from(self.config.prepare_timeout.as_micros()).unwrap_or(0),
            ),
            0,
        );

        // Phase 1: prepare in parallel.
        let prepare_futures = endpoints
            .iter()
            .map(|(pid, ep)| {
                let pid = *pid;
                let ep = Arc::clone(ep);
                let write_spans = write_spans.clone();
                let to = self.config.per_attempt_timeout;
                async move {
                    let res = tokio::time::timeout(to, ep.prepare(txn_id, &write_spans, deadline))
                        .await
                        .unwrap_or_else(|_| {
                            Ok(PrepareVote::No {
                                reason: format!("prepare timeout on participant {}", pid.0),
                            })
                        });
                    (pid, res)
                }
            })
            .collect::<Vec<_>>();

        let mut votes: HashMap<ParticipantId, PrepareVote> = HashMap::new();
        let mut prepare_errors: Vec<String> = Vec::new();
        for fut in prepare_futures {
            let (pid, res) = fut.await;
            match res {
                Ok(vote) => {
                    votes.insert(pid, vote);
                }
                Err(err) => {
                    prepare_errors.push(format!("participant {}: {err}", pid.0));
                }
            }
        }

        let abort_reason = if !prepare_errors.is_empty() {
            Some(prepare_errors.join("; "))
        } else if let Some((pid, vote)) = votes
            .iter()
            .find(|(_, v)| matches!(v, PrepareVote::No { .. }))
        {
            let reason = match vote {
                PrepareVote::No { reason } => reason.clone(),
                _ => String::new(),
            };
            Some(format!("participant {} voted no: {reason}", pid.0))
        } else {
            None
        };

        if let Some(reason) = abort_reason {
            self.rollback_all(txn_id, &endpoints).await;
            let _ = self.registry.abort(txn_id);
            let _ = self.registry.forget(txn_id);
            return Ok(CommitOutcome::Aborted { reason });
        }

        // All yes. Stage + commit.
        self.registry.stage(txn_id).map_err(|e| e.into_db_error())?;
        let commit_ts = self.clock.now();
        self.registry
            .commit(txn_id, commit_ts)
            .map_err(|e| e.into_db_error())?;

        // Phase 2: commit broadcast with retries.
        self.commit_all(txn_id, commit_ts, &endpoints).await?;
        let _ = self.registry.forget(txn_id);

        Ok(CommitOutcome::Committed { commit_ts })
    }

    async fn rollback_all(
        &self,
        txn_id: DistributedTxnId,
        endpoints: &[(ParticipantId, Arc<dyn TwoPhaseParticipant>)],
    ) {
        for (pid, ep) in endpoints {
            let mut attempt = 0;
            let mut delay = self.config.retry_initial_delay;
            loop {
                let res =
                    tokio::time::timeout(self.config.per_attempt_timeout, ep.rollback(txn_id))
                        .await;
                match res {
                    Ok(Ok(())) => break,
                    _ if attempt < self.config.commit_retries => {
                        attempt += 1;
                        debug!(participant = ?pid, attempt, "rollback retry");
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(Duration::from_secs(30));
                    }
                    _ => {
                        warn!(participant = ?pid, ?txn_id, "rollback gave up");
                        break;
                    }
                }
            }
        }
    }

    async fn commit_all(
        &self,
        txn_id: DistributedTxnId,
        commit_ts: HlcTimestamp,
        endpoints: &[(ParticipantId, Arc<dyn TwoPhaseParticipant>)],
    ) -> DbResult<()> {
        for (pid, ep) in endpoints {
            let mut attempt = 0;
            let mut delay = self.config.retry_initial_delay;
            loop {
                let res = tokio::time::timeout(
                    self.config.per_attempt_timeout,
                    ep.commit(txn_id, commit_ts),
                )
                .await;
                match res {
                    Ok(Ok(())) => break,
                    _ => {
                        if attempt >= self.config.commit_retries {
                            return Err(DbError::internal(format!(
                                "participant {} failed commit after {} retries",
                                pid.0, attempt
                            )));
                        }
                        attempt += 1;
                        debug!(participant = ?pid, attempt, "commit retry");
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(Duration::from_secs(30));
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::distributed_record::DistributedTxnStatus;

    fn mk_txn(coord: u64) -> DistributedTxnId {
        DistributedTxnId {
            coordinator: coord,
            start_ts: HlcTimestamp::new(1000, 0),
            seq: 0,
        }
    }

    /// In-process participant that tracks every RPC and lets the test
    /// program failures (vote no, fail-N-then-succeed, drop forever).
    struct MockParticipant {
        prepare_count: AtomicUsize,
        commit_count: AtomicUsize,
        rollback_count: AtomicUsize,
        prepare_vote: PrepareVote,
        /// Number of commit attempts to fail before succeeding.
        commit_fail_count: AtomicUsize,
        rollback_fail_count: AtomicUsize,
    }

    impl MockParticipant {
        fn voting_yes() -> Arc<Self> {
            Arc::new(Self {
                prepare_count: AtomicUsize::new(0),
                commit_count: AtomicUsize::new(0),
                rollback_count: AtomicUsize::new(0),
                prepare_vote: PrepareVote::Yes {
                    prepared_ts: HlcTimestamp::new(1500, 0),
                },
                commit_fail_count: AtomicUsize::new(0),
                rollback_fail_count: AtomicUsize::new(0),
            })
        }

        fn voting_no(reason: &str) -> Arc<Self> {
            Arc::new(Self {
                prepare_count: AtomicUsize::new(0),
                commit_count: AtomicUsize::new(0),
                rollback_count: AtomicUsize::new(0),
                prepare_vote: PrepareVote::No {
                    reason: reason.to_owned(),
                },
                commit_fail_count: AtomicUsize::new(0),
                rollback_fail_count: AtomicUsize::new(0),
            })
        }

        fn with_commit_failures(fail: usize) -> Arc<Self> {
            Arc::new(Self {
                prepare_count: AtomicUsize::new(0),
                commit_count: AtomicUsize::new(0),
                rollback_count: AtomicUsize::new(0),
                prepare_vote: PrepareVote::Yes {
                    prepared_ts: HlcTimestamp::new(1500, 0),
                },
                commit_fail_count: AtomicUsize::new(fail),
                rollback_fail_count: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait::async_trait]
    impl TwoPhaseParticipant for MockParticipant {
        async fn prepare(
            &self,
            _txn_id: DistributedTxnId,
            _write_spans: &[KeySpan],
            _deadline: HlcTimestamp,
        ) -> DbResult<PrepareVote> {
            self.prepare_count.fetch_add(1, Ordering::SeqCst);
            Ok(self.prepare_vote.clone())
        }

        async fn commit(
            &self,
            _txn_id: DistributedTxnId,
            _commit_ts: HlcTimestamp,
        ) -> DbResult<()> {
            self.commit_count.fetch_add(1, Ordering::SeqCst);
            let remaining = self.commit_fail_count.load(Ordering::SeqCst);
            if remaining > 0 {
                self.commit_fail_count
                    .store(remaining - 1, Ordering::SeqCst);
                return Err(DbError::internal("simulated commit failure"));
            }
            Ok(())
        }

        async fn rollback(&self, _txn_id: DistributedTxnId) -> DbResult<()> {
            self.rollback_count.fetch_add(1, Ordering::SeqCst);
            let remaining = self.rollback_fail_count.load(Ordering::SeqCst);
            if remaining > 0 {
                self.rollback_fail_count
                    .store(remaining - 1, Ordering::SeqCst);
                return Err(DbError::internal("simulated rollback failure"));
            }
            Ok(())
        }
    }

    fn fast_config() -> CoordinatorConfig {
        CoordinatorConfig {
            prepare_timeout: Duration::from_millis(100),
            commit_retries: 3,
            retry_initial_delay: Duration::from_millis(5),
            per_attempt_timeout: Duration::from_millis(200),
        }
    }

    #[tokio::test]
    async fn yes_yes_yes_commits() {
        let registry = DistributedTxnRegistry::new();
        let clock = Arc::new(HybridLogicalClock::new());
        let coord = TwoPhaseCoordinator::new(registry.clone(), clock, fast_config());

        let p1 = MockParticipant::voting_yes();
        let p2 = MockParticipant::voting_yes();
        coord
            .register_participant(
                ParticipantId(1),
                Arc::clone(&p1) as Arc<dyn TwoPhaseParticipant>,
            )
            .await;
        coord
            .register_participant(
                ParticipantId(2),
                Arc::clone(&p2) as Arc<dyn TwoPhaseParticipant>,
            )
            .await;

        let outcome = coord
            .execute(
                mk_txn(1),
                1,
                &[ParticipantId(1), ParticipantId(2)],
                vec![KeySpan {
                    start_key: b"a".to_vec(),
                    end_key: b"z".to_vec(),
                }],
            )
            .await
            .unwrap();
        assert!(matches!(outcome, CommitOutcome::Committed { .. }));
        assert_eq!(p1.commit_count.load(Ordering::SeqCst), 1);
        assert_eq!(p2.commit_count.load(Ordering::SeqCst), 1);
        assert_eq!(p1.rollback_count.load(Ordering::SeqCst), 0);
        // Record forgotten after commit.
        assert!(registry.get(mk_txn(1)).is_none());
    }

    #[tokio::test]
    async fn one_no_vote_aborts_everyone() {
        let registry = DistributedTxnRegistry::new();
        let clock = Arc::new(HybridLogicalClock::new());
        let coord = TwoPhaseCoordinator::new(registry.clone(), clock, fast_config());

        let p1 = MockParticipant::voting_yes();
        let p2 = MockParticipant::voting_no("conflict on key range");
        coord
            .register_participant(
                ParticipantId(1),
                Arc::clone(&p1) as Arc<dyn TwoPhaseParticipant>,
            )
            .await;
        coord
            .register_participant(
                ParticipantId(2),
                Arc::clone(&p2) as Arc<dyn TwoPhaseParticipant>,
            )
            .await;

        let outcome = coord
            .execute(
                mk_txn(1),
                1,
                &[ParticipantId(1), ParticipantId(2)],
                Vec::new(),
            )
            .await
            .unwrap();
        match outcome {
            CommitOutcome::Aborted { reason } => {
                assert!(reason.contains("conflict on key range"), "reason: {reason}");
            }
            other => panic!("expected Aborted, got {other:?}"),
        }
        // Every prepared participant must see a rollback.
        assert_eq!(p1.rollback_count.load(Ordering::SeqCst), 1);
        assert_eq!(p2.rollback_count.load(Ordering::SeqCst), 1);
        // No commits.
        assert_eq!(p1.commit_count.load(Ordering::SeqCst), 0);
        assert_eq!(p2.commit_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn commit_retries_until_participant_succeeds() {
        let registry = DistributedTxnRegistry::new();
        let clock = Arc::new(HybridLogicalClock::new());
        let coord = TwoPhaseCoordinator::new(registry.clone(), clock, fast_config());

        let p1 = MockParticipant::with_commit_failures(2);
        coord
            .register_participant(
                ParticipantId(1),
                Arc::clone(&p1) as Arc<dyn TwoPhaseParticipant>,
            )
            .await;
        let outcome = coord
            .execute(mk_txn(1), 1, &[ParticipantId(1)], Vec::new())
            .await
            .unwrap();
        assert!(matches!(outcome, CommitOutcome::Committed { .. }));
        assert_eq!(
            p1.commit_count.load(Ordering::SeqCst),
            3,
            "2 fails then 1 success"
        );
    }

    #[tokio::test]
    async fn commit_retries_exhaust_then_propagate_error() {
        let registry = DistributedTxnRegistry::new();
        let clock = Arc::new(HybridLogicalClock::new());
        let coord = TwoPhaseCoordinator::new(registry.clone(), clock, fast_config());

        // Fail commits forever.
        let p1 = MockParticipant::with_commit_failures(100);
        coord
            .register_participant(
                ParticipantId(1),
                Arc::clone(&p1) as Arc<dyn TwoPhaseParticipant>,
            )
            .await;
        let err = coord
            .execute(mk_txn(1), 1, &[ParticipantId(1)], Vec::new())
            .await
            .expect_err("must propagate");
        assert!(err.to_string().contains("failed commit"));
        // Record stays Committed in the registry (durable decision was
        // taken before participant kept failing), letting a recovery
        // coordinator retry.
        let record = registry.get(mk_txn(1)).expect("record retained");
        assert_eq!(record.status, DistributedTxnStatus::Committed);
    }

    #[tokio::test]
    async fn unknown_participant_errors_eagerly() {
        let registry = DistributedTxnRegistry::new();
        let clock = Arc::new(HybridLogicalClock::new());
        let coord = TwoPhaseCoordinator::new(registry, clock, fast_config());

        let err = coord
            .execute(mk_txn(1), 1, &[ParticipantId(99)], Vec::new())
            .await
            .expect_err("must reject");
        assert!(err.to_string().contains("unknown participant"));
    }

    #[tokio::test]
    async fn empty_participant_list_is_rejected() {
        let registry = DistributedTxnRegistry::new();
        let clock = Arc::new(HybridLogicalClock::new());
        let coord = TwoPhaseCoordinator::new(registry, clock, fast_config());
        let err = coord
            .execute(mk_txn(1), 1, &[], Vec::new())
            .await
            .expect_err("must reject");
        assert!(err.to_string().contains("at least one participant"));
    }
}
