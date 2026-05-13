#![allow(
    clippy::collapsible_else_if,
    clippy::items_after_statements,
    clippy::too_many_lines,
    clippy::wildcard_imports
)]

use super::*;
use std::time::Instant;
use tracing::error;

fn log_tx_cleanup_result(action: &'static str, result: DbResult<()>) {
    if let Err(error) = result {
        warn!(action, error = %error, "transaction cleanup step failed");
    }
}

pub(super) enum SessionPolicyEnforcement {
    Allowed,
    Rejected(DbError),
    RollbackAndReject {
        error: DbError,
        txn: aiondb_tx::ActiveTransaction,
        include_catalog_participant: bool,
        include_storage_participant: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransactionEndAction {
    Commit,
    Rollback,
}

impl Engine {
    pub(super) fn ensure_active_transaction_participants(
        &self,
        session: &SessionHandle,
        require_catalog_participant: bool,
        require_storage_participant: bool,
    ) -> DbResult<()> {
        let Some((
            txn_id,
            isolation,
            implicit_txn_active,
            has_catalog_participant,
            has_storage_participant,
        )) = self.with_session(session, |record| {
            let Some(txn) = record.active_txn.as_ref() else {
                return Ok(None);
            };
            Ok(Some((
                txn.id,
                txn.isolation,
                record.implicit_txn_active,
                record.active_txn_includes_catalog_participant,
                record.active_txn_includes_storage_participant,
            )))
        })?
        else {
            return Ok(());
        };

        // Implicit transactions choose participants when they begin.
        if implicit_txn_active {
            return Ok(());
        }

        let start_catalog = require_catalog_participant && !has_catalog_participant;
        let start_storage = require_storage_participant && !has_storage_participant;
        if !start_catalog && !start_storage {
            return Ok(());
        }

        if start_catalog {
            self.catalog_txn.begin_txn(txn_id)?;
        }
        if start_storage {
            if let Err(error) = self.storage_txn.begin_txn(txn_id, isolation) {
                if start_catalog {
                    log_tx_cleanup_result(
                        "rollback catalog after storage participant enrollment failure",
                        self.catalog_txn.rollback_txn(txn_id),
                    );
                }
                return Err(error);
            }
        }

        if let Err(error) = self.with_session_mut(session, |record| {
            if record
                .active_txn
                .as_ref()
                .is_some_and(|txn| txn.id == txn_id && !record.implicit_txn_active)
            {
                if start_catalog {
                    record.active_txn_includes_catalog_participant = true;
                }
                if start_storage {
                    record.active_txn_includes_storage_participant = true;
                }
            }
            Ok(())
        }) {
            if start_storage {
                log_tx_cleanup_result(
                    "rollback storage after participant enrollment session state failure",
                    self.storage_txn.rollback_txn(txn_id),
                );
            }
            if start_catalog {
                log_tx_cleanup_result(
                    "rollback catalog after participant enrollment session state failure",
                    self.catalog_txn.rollback_txn(txn_id),
                );
            }
            return Err(error);
        }

        Ok(())
    }

    fn take_session_txn_with_absent_notice(
        &self,
        session: &SessionHandle,
        absent_notice: &str,
        end_action: TransactionEndAction,
    ) -> DbResult<Option<(aiondb_tx::ActiveTransaction, bool, bool)>> {
        let txn = self.with_session_mut(session, |record| {
            let Some(txn) = record.active_txn.take() else {
                record.transaction_failed = false;
                record.push_notice(absent_notice.to_owned());
                return Ok(None);
            };
            let includes_catalog_participant = record.active_txn_includes_catalog_participant;
            let includes_storage_participant = record.active_txn_includes_storage_participant;
            record.active_txn_includes_catalog_participant = false;
            record.active_txn_includes_storage_participant = false;
            let implicit_txn_active = record.implicit_txn_active;
            record.transaction_failed = false;
            record.txn_started_at = None;
            record.savepoints.clear();
            record.clear_transaction_local_state();
            aiondb_eval::plpgsql_clear_compat_cursors();
            if implicit_txn_active {
                record.clear_compat_cursor_portals();
            } else {
                if end_action == TransactionEndAction::Commit {
                    record.clear_transaction_scoped_portals_on_commit();
                } else {
                    record.clear_transaction_scoped_portals();
                }
            }
            Ok(Some((
                txn,
                includes_catalog_participant,
                includes_storage_participant,
            )))
        })?;
        if txn.is_some() {
            self.clear_compat_advisory_transaction_locks(session);
        }
        Ok(txn)
    }

    pub(super) fn begin_transaction_internal(
        &self,
        session: &SessionHandle,
        isolation: IsolationLevel,
    ) -> DbResult<()> {
        self.begin_transaction_internal_with_options(session, isolation, true, true, true)
    }

    pub(super) fn begin_transaction_internal_with_options(
        &self,
        session: &SessionHandle,
        isolation: IsolationLevel,
        include_catalog_participant: bool,
        include_storage_participant: bool,
        set_local_transaction_characteristics: bool,
    ) -> DbResult<()> {
        // PG treats BEGIN when already in a transaction as a warning, not an error.
        let already_active = self.with_session_mut(session, |record| {
            let already_active = record.active_txn.is_some();
            if already_active {
                record.push_notice("there is already a transaction in progress".to_owned());
            }
            Ok(already_active)
        })?;
        if already_active {
            return Ok(());
        }

        let mut txn = Some(self.tx_manager.begin(isolation)?);
        let txn_id = txn.as_ref().map(|active| active.id).unwrap_or_default();
        if include_catalog_participant {
            if let Err(error) = self.catalog_txn.begin_txn(txn_id) {
                let Some(active_txn) = txn.take() else {
                    return Err(DbError::internal(
                        "query engine lost transaction handle during catalog begin",
                    ));
                };
                log_tx_cleanup_result(
                    "rollback tx manager after catalog begin failure",
                    self.tx_manager.rollback(active_txn),
                );
                return Err(error);
            }
        }
        if include_storage_participant {
            if let Err(error) = self.storage_txn.begin_txn(txn_id, isolation) {
                if include_catalog_participant {
                    log_tx_cleanup_result(
                        "rollback catalog after storage begin failure",
                        self.catalog_txn.rollback_txn(txn_id),
                    );
                }
                let Some(active_txn) = txn.take() else {
                    return Err(DbError::internal(
                        "query engine lost transaction handle during storage begin",
                    ));
                };
                log_tx_cleanup_result(
                    "rollback tx manager after storage begin failure",
                    self.tx_manager.rollback(active_txn),
                );
                return Err(error);
            }
        }
        let store_result = self.with_session_mut(session, |record| {
            if record.active_txn.is_some() {
                return Err(DbError::protocol("transaction already active"));
            }
            record.active_txn = txn.take();
            record.active_txn_includes_catalog_participant = include_catalog_participant;
            record.active_txn_includes_storage_participant = include_storage_participant;
            record.transaction_failed = false;
            record.txn_started_at = Some(Instant::now());
            Ok(())
        });
        if let Err(error) = store_result {
            if include_catalog_participant {
                log_tx_cleanup_result(
                    "rollback catalog after failed session txn state store",
                    self.catalog_txn.rollback_txn(txn_id),
                );
            }
            if include_storage_participant {
                log_tx_cleanup_result(
                    "rollback storage after failed session txn state store",
                    self.storage_txn.rollback_txn(txn_id),
                );
            }
            if let Some(txn) = txn {
                log_tx_cleanup_result(
                    "rollback tx manager after failed session txn state store",
                    self.tx_manager.rollback(txn),
                );
            }
            return Err(error);
        }

        if set_local_transaction_characteristics {
            if let Err(error) = self.with_session_mut(session, |record| {
                super::session_vars::set_transaction_characteristics_in_record(
                    record,
                    Some(isolation),
                    Some(super::session_vars::default_transaction_read_only_for_record(record)),
                    Some(super::session_vars::default_transaction_deferrable_for_record(record)),
                    true,
                )
            }) {
                let _ = self.rollback_transaction_internal(session);
                return Err(error);
            }
        }
        Ok(())
    }

    /// Commit a transaction with retry-hardened catalog commit.
    ///
    /// The commit protocol is:
    /// 1. Validate: serializable coordinator, catalog, storage (can fail -> clean rollback)
    /// 2. Acquire commit timestamp from tx manager
    /// 3. Storage commit (durable WAL write -- point of no return)
    /// 4. Catalog commit with retry (up to 3 attempts after storage succeeds)
    /// 5. Serializable coordinator finalization
    /// 6. Lock release
    ///
    /// If catalog commit fails after all retries, the commit is marked as
    /// ambiguous. Recovery on restart will complete the catalog commit.
    pub(super) fn commit_transaction_internal(&self, session: &SessionHandle) -> DbResult<()> {
        let Some(txn) = self.take_session_txn_with_absent_notice(
            session,
            "there is no transaction in progress",
            TransactionEndAction::Commit,
        )?
        else {
            return Ok(());
        };
        let (txn, include_catalog_participant, include_storage_participant) = txn;

        #[derive(Clone)]
        enum CommitProgress {
            PreTxManagerCommit,
            TxManagerCommitted(aiondb_tx::CommitResult),
            StorageCommitted(aiondb_tx::CommitResult),
            CatalogCommitted,
        }

        let txn_id = txn.id;
        let txn_for_validation = txn.clone();
        let mut progress = CommitProgress::PreTxManagerCommit;
        let result = (|| {
            // Serialize commits for anything other than pure READ COMMITTED DML.
            // Snapshot-isolation / serializable transactions rely on a single
            // critical section spanning validate_commit → commit_ts allocation →
            // finish_commit. Dropping the lock between validate and finish
            // permits concurrent writers to both pass validation against the
            let needs_commit_coordination = txn_for_validation.isolation
                != aiondb_tx::IsolationLevel::ReadCommitted
                || (include_catalog_participant && self.catalog_txn.txn_writes_catalog(txn_id)?);
            let _commit_guard = if needs_commit_coordination {
                Some(self.commit_lock.lock().map_err(|e| {
                    DbError::internal(format!("commit coordination lock poisoned: {e}"))
                })?)
            } else {
                None
            };

            self.serializable_coordinator
                .validate_commit(&txn_for_validation)?;
            if include_catalog_participant {
                self.catalog_txn.validate_commit_txn(txn_id)?;
            }
            if include_storage_participant {
                self.storage_txn.validate_commit_txn(txn_id)?;
            }

            // Commit order: tx_manager (to obtain commit_ts) -> storage ->
            // catalog. Storage remains the more failure-prone participant, so
            // we still publish it first once validation says the catalog
            // commit is safe to apply.
            let commit = self.tx_manager.commit(txn)?;
            progress = CommitProgress::TxManagerCommitted(commit.clone());

            if include_storage_participant {
                self.storage_txn
                    .commit_txn(commit.txn_id, commit.commit_ts)
                    .map_err(|error| {
                        super::support::mark_commit_outcome_ambiguous(error, "storage commit")
                    })?;
                progress = CommitProgress::StorageCommitted(commit.clone());
            }

            // Catalog commit with retry: storage is already committed,
            // so we MUST complete the catalog commit to avoid an ambiguous
            // state. Retry up to 3 times before marking as ambiguous.
            let catalog_result = if include_catalog_participant {
                let mut last_err = None;
                let mut committed = false;
                for attempt in 0..3u32 {
                    match self.catalog_txn.commit_txn(txn_id) {
                        Ok(()) => {
                            committed = true;
                            break;
                        }
                        Err(e) => {
                            warn!(
                                txn_id = txn_id.get(),
                                attempt = attempt + 1,
                                error = %e,
                                "catalog commit failed after storage commit, retrying"
                            );
                            last_err = Some(e);
                        }
                    }
                }
                if committed {
                    Ok(())
                } else {
                    Err(last_err.unwrap_or_else(|| {
                        DbError::internal("catalog commit failed without error")
                    }))
                }
            } else {
                Ok(())
            };

            if include_storage_participant {
                if let Err(ref catalog_err) = catalog_result {
                    // Storage committed but catalog failed after retries.
                    // Log critical error: recovery will be needed on restart.
                    error!(
                        txn_id = txn_id.get(),
                        error = %catalog_err,
                        "CRITICAL: storage committed but catalog commit failed after 3 retries — \
                         transaction is in ambiguous state; automatic recovery will complete it on next restart"
                    );
                }
            }

            catalog_result
                .map_err(|e| super::support::mark_commit_outcome_ambiguous(e, "catalog commit"))?;
            progress = CommitProgress::CatalogCommitted;

            self.serializable_coordinator
                .finish_commit(txn_id, commit.commit_ts)
                .map_err(|error| {
                    super::support::mark_commit_outcome_ambiguous(
                        error,
                        "serializable commit finalization",
                    )
                })?;
            Ok(())
        })();

        if result.is_err() {
            match &progress {
                CommitProgress::PreTxManagerCommit => {
                    if include_catalog_participant {
                        log_tx_cleanup_result(
                            "rollback catalog after commit failure before tx-manager commit",
                            self.catalog_txn.rollback_txn(txn_id),
                        );
                    }
                    if include_storage_participant {
                        log_tx_cleanup_result(
                            "rollback storage after commit failure before tx-manager commit",
                            self.storage_txn.rollback_txn(txn_id),
                        );
                    }
                    log_tx_cleanup_result(
                        "rollback tx manager after commit failure before tx-manager commit",
                        self.tx_manager.rollback(txn_for_validation),
                    );
                    log_tx_cleanup_result(
                        "rollback serializable coordinator after commit failure before tx-manager commit",
                        self.serializable_coordinator.rollback_txn(txn_id),
                    );
                }
                CommitProgress::TxManagerCommitted(commit) => {
                    // Catalog has not committed yet, so clearing its pending
                    // state is still safe even though the overall commit
                    // outcome is now ambiguous.
                    if include_catalog_participant {
                        log_tx_cleanup_result(
                            "rollback catalog after ambiguous storage commit failure",
                            self.catalog_txn.rollback_txn(txn_id),
                        );
                    }
                    log_tx_cleanup_result(
                        "finish serializable coordinator after ambiguous storage commit failure",
                        self.serializable_coordinator
                            .finish_commit(txn_id, commit.commit_ts),
                    );
                }
                CommitProgress::StorageCommitted(commit) => {
                    log_tx_cleanup_result(
                        "finish serializable coordinator after ambiguous catalog commit failure",
                        self.serializable_coordinator
                            .finish_commit(txn_id, commit.commit_ts),
                    );
                }
                CommitProgress::CatalogCommitted => {}
            }
        }

        let release_result = self.lock_manager.release_txn(txn_id);
        match progress {
            CommitProgress::PreTxManagerCommit => {
                super::support::merge_with_lock_release_error(result, release_result, "commit")
            }
            _ => match (result, release_result) {
                (Ok(()), Ok(())) => Ok(()),
                (Ok(()), Err(release_error)) => Err(DbError::internal(format!(
                    "commit succeeded but lock release failed: {release_error}"
                ))
                .with_client_detail(
                    "transaction changes were committed, but lock cleanup reported an error",
                )),
                (Err(error), Ok(())) => Err(error),
                (Err(error), Err(release_error)) => {
                    Err(super::support::with_appended_internal_detail(
                        error,
                        format!("lock release after commit also failed: {release_error}"),
                    ))
                }
            },
        }
    }

    pub(super) fn rollback_transaction_internal(&self, session: &SessionHandle) -> DbResult<()> {
        let Some(txn) = self.take_session_txn_with_absent_notice(
            session,
            "there is no transaction in progress",
            TransactionEndAction::Rollback,
        )?
        else {
            return Ok(());
        };
        let (txn, include_catalog_participant, include_storage_participant) = txn;
        self.rollback_active_transaction(
            txn,
            include_catalog_participant,
            include_storage_participant,
        )
    }

    pub(super) fn statement_lock_owner(&self, txn_id: TxnId) -> (TxnId, bool) {
        if txn_id == TxnId::default() {
            (
                TxnId::new(self.statement_lock_owner.fetch_sub(1, Ordering::SeqCst)),
                true,
            )
        } else {
            (txn_id, false)
        }
    }

    pub(super) fn sessions(
        &self,
    ) -> DbResult<RwLockReadGuard<'_, HashMap<SessionHandle, Arc<Mutex<SessionRecord>>>>> {
        self.sessions
            .read()
            .map_err(|e| DbError::internal(format!("session registry poisoned: {e}")))
    }

    pub(super) fn sessions_mut(
        &self,
    ) -> DbResult<RwLockWriteGuard<'_, HashMap<SessionHandle, Arc<Mutex<SessionRecord>>>>> {
        self.sessions
            .write()
            .map_err(|e| DbError::internal(format!("session registry poisoned: {e}")))
    }

    pub(super) fn session_entry(
        &self,
        handle: &SessionHandle,
    ) -> DbResult<Arc<Mutex<SessionRecord>>> {
        self.sessions()?
            .get(handle)
            .cloned()
            .ok_or_else(|| DbError::invalid_authorization("unknown session handle"))
    }

    pub(super) fn lock_session(
        session: &Arc<Mutex<SessionRecord>>,
    ) -> DbResult<MutexGuard<'_, SessionRecord>> {
        session
            .lock()
            .map_err(|e| DbError::internal(format!("session state poisoned: {e}")))
    }

    fn session_policy_error(
        &self,
        record: &SessionRecord,
        now: Instant,
    ) -> SessionPolicyEnforcement {
        if let Some(max_lifetime) = self.config.security.max_session_lifetime {
            if now.duration_since(record.created_at) > max_lifetime {
                return SessionPolicyEnforcement::Rejected(DbError::from_report(
                    aiondb_core::ErrorReport::new(
                        aiondb_core::SqlState::AdminShutdown,
                        "session lifetime exceeded",
                    ),
                ));
            }
        }
        if let Some(max_idle) = self.config.security.max_session_idle_timeout {
            if now.duration_since(record.last_active) > max_idle {
                return SessionPolicyEnforcement::Rejected(DbError::from_report(
                    aiondb_core::ErrorReport::new(
                        aiondb_core::SqlState::IdleSessionTimeout,
                        "session idle timeout exceeded",
                    ),
                ));
            }
        }
        if let Some(max_txn_idle) = self.config.security.max_transaction_idle_timeout {
            if record.active_txn.is_some() && now.duration_since(record.last_active) > max_txn_idle
            {
                let error = DbError::transaction_error(
                    aiondb_core::SqlState::IdleInTransactionSessionTimeout,
                    "transaction idle timeout exceeded; transaction has been rolled back",
                );
                if let Some(txn) = record.active_txn.clone() {
                    return SessionPolicyEnforcement::RollbackAndReject {
                        error,
                        txn,
                        include_catalog_participant: record.active_txn_includes_catalog_participant,
                        include_storage_participant: record.active_txn_includes_storage_participant,
                    };
                }
                return SessionPolicyEnforcement::Rejected(error);
            }
        }
        SessionPolicyEnforcement::Allowed
    }

    pub(super) fn enforce_session_policies(
        &self,
        record: &mut SessionRecord,
    ) -> SessionPolicyEnforcement {
        match self.session_policy_error(record, Instant::now()) {
            SessionPolicyEnforcement::RollbackAndReject {
                error,
                txn,
                include_catalog_participant,
                include_storage_participant,
            } => {
                record.active_txn = None;
                record.active_txn_includes_catalog_participant = false;
                record.active_txn_includes_storage_participant = false;
                record.transaction_failed = false;
                record.txn_started_at = None;
                record.savepoints.clear();
                record.clear_transaction_local_state();
                record.clear_transaction_scoped_portals();
                SessionPolicyEnforcement::RollbackAndReject {
                    error,
                    txn,
                    include_catalog_participant,
                    include_storage_participant,
                }
            }
            other => other,
        }
    }

    /// Hardcoded upper bound for orphaned transaction cleanup.  If no
    /// `max_transaction_idle_timeout` is configured, sessions with
    /// transactions older than this are still purged as a safety net
    /// against resource leaks from abrupt disconnections.
    const ORPHAN_TXN_SAFETY_NET_TIMEOUT: std::time::Duration =
        std::time::Duration::from_secs(60 * 10);

    pub(super) fn purge_expired_sessions(&self) -> DbResult<()> {
        let now = Instant::now();
        let mut expired_transactions = Vec::new();
        let mut expired_handles = Vec::new();
        let txn_timeout = self
            .config
            .security
            .max_transaction_idle_timeout
            .unwrap_or(Self::ORPHAN_TXN_SAFETY_NET_TIMEOUT);

        {
            let sessions = self.sessions()?;
            for (handle, session) in sessions.iter() {
                let record = Self::lock_session(session)?;
                // Check standard session-level policies (lifetime, idle).
                if !matches!(
                    self.session_policy_error(&record, now),
                    SessionPolicyEnforcement::Allowed
                ) {
                    expired_handles.push(handle.clone());
                    continue;
                }
                // Safety net: detect sessions holding a transaction that has
                // been idle longer than `txn_timeout`. This catches orphaned
                // transactions from connections that died without completing
                // cleanup without treating long-but-active transactions as
                // stale.
                if record.active_txn.is_some() {
                    let idle_for = now.duration_since(record.last_active);
                    if idle_for > txn_timeout {
                        warn!(
                            txn_idle_secs = idle_for.as_secs(),
                            "purging session with stale transaction"
                        );
                        expired_handles.push(handle.clone());
                    }
                }
            }
        }

        {
            let mut sessions = self.sessions_mut()?;
            for handle in expired_handles {
                if let Some(session) = sessions.remove(&handle) {
                    let mut record = Self::lock_session(&session)?;
                    record.compat_advisory_locks.session_locks.clear();
                    record.compat_advisory_locks.xact_locks.clear();
                    record.txn_started_at = None;
                    let txn = record.active_txn.take();
                    let include_catalog_participant =
                        record.active_txn_includes_catalog_participant;
                    let include_storage_participant =
                        record.active_txn_includes_storage_participant;
                    record.active_txn_includes_catalog_participant = false;
                    record.active_txn_includes_storage_participant = false;
                    expired_transactions.push((
                        txn,
                        include_catalog_participant,
                        include_storage_participant,
                    ));
                }
            }
        }

        for (active_txn, include_catalog_participant, include_storage_participant) in
            expired_transactions
        {
            if let Some(active_txn) = active_txn {
                self.rollback_active_transaction(
                    active_txn,
                    include_catalog_participant,
                    include_storage_participant,
                )?;
            }
        }

        Ok(())
    }

    pub(super) fn rollback_active_transaction(
        &self,
        txn: aiondb_tx::ActiveTransaction,
        include_catalog_participant: bool,
        include_storage_participant: bool,
    ) -> DbResult<()> {
        // Rollback in reverse of commit order (tx_manager -> storage -> catalog):
        // catalog first, then storage, then tx_manager.
        let txn_id = txn.id;
        let mut first_error = None;

        if include_catalog_participant {
            if let Err(error) = self.catalog_txn.rollback_txn(txn_id) {
                first_error = Some(error);
            }
        }
        if include_storage_participant {
            if let Err(error) = self.storage_txn.rollback_txn(txn_id) {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = self.tx_manager.rollback(txn) {
            first_error.get_or_insert(error);
        }
        if let Err(error) = self.serializable_coordinator.rollback_txn(txn_id) {
            first_error.get_or_insert(error);
        }
        if let Err(error) = self.lock_manager.release_txn(txn_id) {
            first_error.get_or_insert(error);
        }

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
