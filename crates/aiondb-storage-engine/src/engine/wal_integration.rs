use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[cfg(test)]
use std::cell::RefCell;

use aiondb_core::{DbError, DbResult, RelationId, TxnId};
use aiondb_wal::codec::{self, PreparedWalRecord};
use aiondb_wal::replication::{ReplicaRegistry, WalNotifier};
use aiondb_wal::writer::AppendBatchResult;
use aiondb_wal::{Lsn, WalCompression, WalConfig, WalRecord, WalWriter};
#[cfg(not(test))]
use rayon::prelude::*;

use super::WalCommitPolicy;

#[cfg(not(test))]
const PARALLEL_WAL_PREPARE_MIN_RECORDS: usize = 256;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InjectedWalFailure {
    Append,
    Flush,
}

impl fmt::Debug for WalIntegration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WalIntegration")
            .field("auto_txn_counter", &self.auto_txn_counter)
            .field("commit_policy", &self.commit_policy)
            .finish_non_exhaustive()
    }
}

/// Manages WAL logging for the storage engine.
///
/// All WAL operations go through this struct. The WAL writer is behind a
/// `Mutex` so that concurrent transactions can append records safely.
/// Autocommit operations use synthetic transaction IDs from a high range
/// to avoid collision with real explicit transaction IDs.
pub(super) struct WalIntegration {
    // Debug is manually implemented below since Mutex<WalWriter> doesn't derive it.
    writer: Mutex<WalWriter>,
    auto_txn_counter: AtomicU64,
    commit_policy: WalCommitPolicy,
    wal_compression: WalCompression,
    group_commit_delay: Duration,
    pending_commit_flushes: AtomicU32,
    group_commit_state: Mutex<GroupCommitState>,
    group_commit_cv: Condvar,
    durable_sync_lock: Mutex<()>,
    group_commit_queue_depth_peak: AtomicU32,
    wal_notifier: Mutex<Option<Arc<WalNotifier>>>,
    /// WAL directory path, used for snapshot persistence next to WAL segments.
    wal_dir: PathBuf,
    sync_commit_registry: Mutex<Option<Arc<ReplicaRegistry>>>,
    /// Write concern level encoded as u8:
    /// 0 = Local, 1 = Majority, 2 = All, 3+ = Factor(n-3).
    write_concern_level: AtomicU32,
    /// Timeout for write concern waits.
    sync_commit_timeout: Mutex<Duration>,
    written_bytes_total: AtomicU64,
    durable_bytes_total: AtomicU64,
    durable_flush_total: AtomicU64,
    durable_flush_micros_total: AtomicU64,
    durable_flush_micros_max: AtomicU64,
    #[cfg(test)]
    durable_flush_count: AtomicU64,
    #[cfg(test)]
    fail_next_operation: Mutex<Option<InjectedWalFailure>>,
}

/// Starting range for synthetic autocommit transaction IDs.
/// Real txn IDs start from small values; these start from a high range.
const AUTO_TXN_BASE: u64 = 1 << 48;

/// Default timeout for synchronous commit replica flush confirmation.
const SYNC_COMMIT_TIMEOUT: Duration = Duration::from_secs(30);

// Write concern level encoding (stored in AtomicU32).
const CONCERN_LOCAL: u32 = 0;
const CONCERN_MAJORITY: u32 = 1;
const CONCERN_ALL: u32 = 2;
/// Factor(n) is encoded as 3 + n.
const CONCERN_FACTOR_BASE: u32 = 3;

#[cfg(test)]
#[derive(Debug)]
struct PrepareRecordHook {
    started_tx: std::sync::mpsc::Sender<()>,
    release_rx: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
thread_local! {
    static PREPARE_RECORD_HOOK: RefCell<Option<PrepareRecordHook>> = const { RefCell::new(None) };
}

/// Compute the number of replica acks needed for a given concern level
/// and the current connected replica count.
fn compute_required_acks(concern_level: u32, connected_replicas: usize) -> usize {
    match concern_level {
        CONCERN_LOCAL => 0,
        CONCERN_MAJORITY => {
            let total = 1 + connected_replicas; // primary + replicas
            let majority = total / 2 + 1;
            majority.saturating_sub(1) // primary already acked
        }
        CONCERN_ALL => connected_replicas,
        n => {
            // Factor(n) where n = concern_level - CONCERN_FACTOR_BASE
            (n - CONCERN_FACTOR_BASE) as usize
        }
    }
}

#[derive(Clone, Debug)]
struct GroupCommitState {
    leader_in_progress: bool,
    pending_requests: u32,
    last_durable_lsn: Lsn,
    last_flush_error: Option<DbError>,
}

enum CommitFlushAction {
    None,
    DurableSync(Option<(std::fs::File, Lsn)>, Instant),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WalRuntimeMetricsSnapshot {
    pub written_bytes_total: u64,
    pub durable_bytes_total: u64,
    pub durable_flush_total: u64,
    pub durable_flush_micros_total: u64,
    pub durable_flush_micros_max: u64,
    pub group_commit_pending_requests: u32,
    pub group_commit_queue_depth_peak: u32,
}

impl WalIntegration {
    /// Open or create the WAL from a config. Scans existing segments to
    /// resume from the correct LSN.
    #[cfg(test)]
    pub fn open(config: WalConfig) -> DbResult<Self> {
        Self::open_with_commit_policy(config, WalCommitPolicy::Always)
    }

    /// Open or create the WAL from a config with an explicit commit policy.
    pub fn open_with_commit_policy(
        config: WalConfig,
        commit_policy: WalCommitPolicy,
    ) -> DbResult<Self> {
        validate_commit_policy(commit_policy)?;
        let group_commit_delay = Duration::from_micros(config.group_commit_delay_micros);
        let wal_dir = config.dir.clone();
        let wal_compression = config.wal_compression;
        let writer = WalWriter::open(config)?;
        let last_durable_lsn = writer.last_lsn().unwrap_or(Lsn::ZERO);
        Ok(Self {
            writer: Mutex::new(writer),
            auto_txn_counter: AtomicU64::new(AUTO_TXN_BASE),
            commit_policy,
            wal_compression,
            group_commit_delay,
            pending_commit_flushes: AtomicU32::new(0),
            group_commit_state: Mutex::new(GroupCommitState {
                leader_in_progress: false,
                pending_requests: 0,
                last_durable_lsn,
                last_flush_error: None,
            }),
            group_commit_cv: Condvar::new(),
            durable_sync_lock: Mutex::new(()),
            group_commit_queue_depth_peak: AtomicU32::new(0),
            wal_notifier: Mutex::new(None),
            wal_dir,
            sync_commit_registry: Mutex::new(None),
            write_concern_level: AtomicU32::new(CONCERN_LOCAL),
            sync_commit_timeout: Mutex::new(SYNC_COMMIT_TIMEOUT),
            written_bytes_total: AtomicU64::new(0),
            durable_bytes_total: AtomicU64::new(0),
            durable_flush_total: AtomicU64::new(0),
            durable_flush_micros_total: AtomicU64::new(0),
            durable_flush_micros_max: AtomicU64::new(0),
            #[cfg(test)]
            durable_flush_count: AtomicU64::new(0),
            #[cfg(test)]
            fail_next_operation: Mutex::new(None),
        })
    }

    /// Returns the WAL directory path, used for snapshot file placement.
    pub fn wal_dir(&self) -> &Path {
        &self.wal_dir
    }

    /// Generate a unique transaction ID for autocommit operations.
    pub fn next_auto_txn_id(&self) -> TxnId {
        let id = self.auto_txn_counter.fetch_add(1, Ordering::Relaxed);
        TxnId::new(id)
    }

    /// Append a WAL record without flushing.
    pub fn log(&self, record: &WalRecord) -> DbResult<Lsn> {
        #[cfg(test)]
        self.consume_injected_failure(InjectedWalFailure::Append)?;
        let prepared = self.prepare_record(record)?;
        let mut writer = self.lock_writer()?;
        let lsn = writer.append_prepared(&prepared)?;
        self.account_append_bytes(&writer);
        Ok(lsn)
    }

    /// Append multiple WAL records without flushing.
    pub fn log_batch(&self, records: &[WalRecord]) -> DbResult<Option<Lsn>> {
        if records.is_empty() {
            return Ok(None);
        }
        #[cfg(test)]
        self.consume_injected_failure(InjectedWalFailure::Append)?;
        let prepared = self.prepare_records(records)?;
        let mut writer = self.lock_writer()?;
        let batch = writer.append_prepared_batch(&prepared)?;
        let Some(batch) = batch else {
            return Err(DbError::internal(
                "prepared WAL batch was empty for non-empty records",
            ));
        };
        self.account_append_batch_bytes(batch);
        Ok(Some(batch.last_lsn))
    }

    /// Flush the WAL using the configured background policy.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn flush(&self) -> DbResult<()> {
        let mut writer = self.lock_writer()?;
        writer.flush()
    }

    /// Flush the WAL and force it to stable storage.
    pub fn flush_durable(&self) -> DbResult<()> {
        let flush_start = Instant::now();
        let prepared_sync = self.prepare_durable_sync_locked()?;
        self.complete_prepared_durable_sync(prepared_sync, flush_start)
            .map(|_| ())
    }

    /// Append a WAL record and force it durable immediately.
    pub fn log_and_flush(&self, record: &WalRecord) -> DbResult<Lsn> {
        #[cfg(test)]
        self.consume_injected_failure(InjectedWalFailure::Append)?;
        let prepared = self.prepare_record(record)?;
        let flush_start = Instant::now();
        let (lsn, prepared_sync) = {
            let mut writer = self.lock_writer()?;
            let lsn = writer.append_prepared(&prepared)?;
            self.account_append_bytes(&writer);
            let prepared_sync = writer.prepare_durable_sync()?;
            (lsn, prepared_sync)
        };
        self.complete_prepared_durable_sync(prepared_sync, flush_start)?;
        Ok(lsn)
    }

    /// Append a WAL record and flush it using the configured commit policy.
    pub fn log_and_commit(&self, record: &WalRecord) -> DbResult<Lsn> {
        if self.commit_policy == WalCommitPolicy::Always {
            // Always use group commit - even with delay=0, this allows
            // concurrent appends while the leader thread is flushing.
            #[cfg(test)]
            self.consume_injected_failure(InjectedWalFailure::Append)?;
            let prepared = self.prepare_record(record)?;
            let lsn = {
                let mut writer = self.lock_writer()?;
                let lsn = writer.append_prepared(&prepared)?;
                self.account_append_bytes(&writer);
                lsn
            };
            self.flush_durable_grouped(lsn)?;
            self.wait_for_sync_commit(lsn)?;
            return Ok(lsn);
        }

        let prepared = self.prepare_record(record)?;
        let (lsn, flush_action) = {
            let mut writer = self.lock_writer()?;
            #[cfg(test)]
            self.consume_injected_failure(InjectedWalFailure::Append)?;
            let lsn = writer.append_prepared(&prepared)?;
            self.account_append_bytes(&writer);
            let flush_action = self.flush_commit(&mut writer)?;
            (lsn, flush_action)
        };
        self.complete_commit_flush_action(flush_action)?;
        self.wait_for_sync_commit(lsn)?;
        Ok(lsn)
    }

    /// Append a full-page-image WAL record and flush it using the configured
    /// commit policy.
    #[allow(dead_code)]
    pub fn log_full_page_image(
        &self,
        relation_id: RelationId,
        page_number: u64,
        page_data: &[u8],
    ) -> DbResult<Lsn> {
        self.log_and_commit(&WalRecord::FullPageImage {
            relation_id,
            page_number,
            page_data: page_data.to_vec(),
        })
    }

    /// Append multiple records atomically (single lock acquisition), then make
    /// them durable before returning.
    #[cfg(test)]
    pub fn log_batch_and_flush(&self, records: &[WalRecord]) -> DbResult<Lsn> {
        if records.is_empty() {
            return Ok(Lsn::ZERO);
        }
        #[cfg(test)]
        if !records.is_empty() {
            self.consume_injected_failure(InjectedWalFailure::Append)?;
        }
        let prepared = self.prepare_records(records)?;
        let flush_start = Instant::now();
        let (last_lsn, prepared_sync) = {
            let mut writer = self.lock_writer()?;
            let Some(batch) = writer.append_prepared_batch(&prepared)? else {
                return Err(DbError::internal(
                    "prepared WAL batch was empty for non-empty records",
                ));
            };
            self.account_append_batch_bytes(batch);
            let prepared_sync = writer.prepare_durable_sync()?;
            (batch.last_lsn, prepared_sync)
        };
        self.complete_prepared_durable_sync(prepared_sync, flush_start)?;
        Ok(last_lsn)
    }

    /// Append multiple records atomically (single lock acquisition), then
    /// flush them using the configured commit policy before returning.
    pub fn log_batch_and_commit(&self, records: &[WalRecord]) -> DbResult<Lsn> {
        if records.is_empty() {
            return Ok(Lsn::ZERO);
        }
        if self.commit_policy == WalCommitPolicy::Always {
            // Always use group commit - even with delay=0, this allows
            // concurrent appends while the leader thread is flushing.
            #[cfg(test)]
            if !records.is_empty() {
                self.consume_injected_failure(InjectedWalFailure::Append)?;
            }
            let prepared = self.prepare_records(records)?;

            let (required_lsn, returned_lsn) = {
                let mut writer = self.lock_writer()?;
                if records.is_empty() {
                    (writer.last_lsn().unwrap_or(Lsn::ZERO), Lsn::ZERO)
                } else {
                    let Some(batch) = writer.append_prepared_batch(&prepared)? else {
                        return Err(DbError::internal(
                            "prepared WAL batch was empty for non-empty records",
                        ));
                    };
                    self.account_append_batch_bytes(batch);
                    (batch.last_lsn, batch.last_lsn)
                }
            };

            self.flush_durable_grouped(required_lsn)?;
            self.wait_for_sync_commit(required_lsn)?;
            return Ok(returned_lsn);
        }

        let prepared = self.prepare_records(records)?;
        let (last_lsn, flush_action) = {
            let mut writer = self.lock_writer()?;
            #[cfg(test)]
            if !records.is_empty() {
                self.consume_injected_failure(InjectedWalFailure::Append)?;
            }
            let Some(batch) = writer.append_prepared_batch(&prepared)? else {
                return Err(DbError::internal(
                    "prepared WAL batch was empty for non-empty records",
                ));
            };
            self.account_append_batch_bytes(batch);
            let flush_action = self.flush_commit(&mut writer)?;
            (batch.last_lsn, flush_action)
        };
        self.complete_commit_flush_action(flush_action)?;
        self.wait_for_sync_commit(last_lsn)?;
        Ok(last_lsn)
    }

    /// Returns the next LSN that will be assigned.
    pub fn next_lsn(&self) -> DbResult<Lsn> {
        let writer = self.lock_writer()?;
        Ok(writer.next_lsn())
    }

    /// Returns the last assigned WAL LSN, or zero when WAL is empty.
    pub fn last_lsn(&self) -> DbResult<Lsn> {
        let writer = self.lock_writer()?;
        Ok(writer.last_lsn().unwrap_or(Lsn::ZERO))
    }

    /// Remove WAL segments whose entries are all before `lsn`.
    ///
    /// Delegates to [`WalWriter::remove_segments_before`] under the writer lock.
    /// Returns the number of segments removed.
    #[cfg(test)]
    pub fn cleanup_before(&self, lsn: Lsn) -> DbResult<u64> {
        self.cleanup_before_with_min_segments(lsn, 0)
    }

    /// Remove WAL segments whose entries are all before `lsn`, while
    /// retaining at least `min_segments_to_keep` newest segments.
    pub fn cleanup_before_with_min_segments(
        &self,
        lsn: Lsn,
        min_segments_to_keep: u32,
    ) -> DbResult<u64> {
        let mut writer = self.lock_writer()?;
        writer.remove_segments_before_with_min_segments(lsn, min_segments_to_keep)
    }

    pub fn set_wal_notifier(&self, notifier: Arc<WalNotifier>) -> DbResult<()> {
        let mut guard = self
            .wal_notifier
            .lock()
            .map_err(|e| DbError::internal(format!("WAL notifier lock poisoned: {e}")))?;
        *guard = Some(notifier);
        Ok(())
    }

    /// Configure the write concern for commit operations.
    ///
    /// `concern_level` is the encoded write concern level (see
    /// `CONCERN_*` constants). The required replica ack count is computed
    /// dynamically at commit time based on the number of connected replicas.
    pub fn set_write_concern(
        &self,
        concern_level: u32,
        timeout: Duration,
        registry: Option<Arc<ReplicaRegistry>>,
    ) -> DbResult<()> {
        self.write_concern_level
            .store(concern_level, Ordering::Release);
        {
            let mut t = self.sync_commit_timeout.lock().map_err(|e| {
                DbError::internal(format!("sync commit timeout lock poisoned: {e}"))
            })?;
            *t = timeout;
        }
        let mut guard = self
            .sync_commit_registry
            .lock()
            .map_err(|e| DbError::internal(format!("sync commit registry lock poisoned: {e}")))?;
        *guard = registry;
        Ok(())
    }

    pub fn runtime_metrics_snapshot(&self) -> DbResult<WalRuntimeMetricsSnapshot> {
        let group_commit_pending_requests = {
            let state = self.lock_group_commit_state()?;
            state.pending_requests
        };
        Ok(WalRuntimeMetricsSnapshot {
            written_bytes_total: self.written_bytes_total.load(Ordering::Relaxed),
            durable_bytes_total: self.durable_bytes_total.load(Ordering::Relaxed),
            durable_flush_total: self.durable_flush_total.load(Ordering::Relaxed),
            durable_flush_micros_total: self.durable_flush_micros_total.load(Ordering::Relaxed),
            durable_flush_micros_max: self.durable_flush_micros_max.load(Ordering::Relaxed),
            group_commit_pending_requests,
            group_commit_queue_depth_peak: self
                .group_commit_queue_depth_peak
                .load(Ordering::Relaxed),
        })
    }

    fn wait_for_sync_commit(&self, commit_lsn: Lsn) -> DbResult<()> {
        let concern_level = self.write_concern_level.load(Ordering::Acquire);
        if concern_level == CONCERN_LOCAL {
            return Ok(());
        }
        let registry = self
            .sync_commit_registry
            .lock()
            .map_err(|e| DbError::internal(format!("sync commit registry lock poisoned: {e}")))?
            .clone();
        let Some(registry) = registry else {
            return Ok(());
        };
        let connected = registry.count();
        let required = compute_required_acks(concern_level, connected);
        if required == 0 {
            // Concern requires acks but there are too few replicas to satisfy it.
            // For Majority with only 1 node (the primary), majority=1 which is
            // already satisfied by the primary itself.
            return Ok(());
        }
        let timeout = self
            .sync_commit_timeout
            .lock()
            .map_err(|e| DbError::internal(format!("sync commit timeout lock poisoned: {e}")))?;
        let timeout = *timeout;
        if !registry.wait_for_write_concern(commit_lsn, required, timeout) {
            return Err(DbError::internal(format!(
                "write concern timed out after {}s waiting for {required} replica(s) to flush LSN {}",
                timeout.as_secs(),
                commit_lsn.get()
            )));
        }
        Ok(())
    }

    fn lock_writer(&self) -> DbResult<MutexGuard<'_, WalWriter>> {
        self.writer
            .lock()
            .map_err(|e| DbError::internal(format!("WAL writer lock poisoned: {e}")))
    }

    fn lock_group_commit_state(&self) -> DbResult<MutexGuard<'_, GroupCommitState>> {
        self.group_commit_state
            .lock()
            .map_err(|e| DbError::internal(format!("WAL group commit lock poisoned: {e}")))
    }

    fn prepare_record(&self, record: &WalRecord) -> DbResult<PreparedWalRecord> {
        #[cfg(test)]
        PREPARE_RECORD_HOOK.with(|hook| {
            if let Some(PrepareRecordHook {
                started_tx,
                release_rx,
            }) = hook.borrow_mut().take()
            {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
            }
        });
        codec::prepare_record_with_compression(record, self.wal_compression)
    }

    fn prepare_records(&self, records: &[WalRecord]) -> DbResult<Vec<PreparedWalRecord>> {
        #[cfg(not(test))]
        if records.len() >= PARALLEL_WAL_PREPARE_MIN_RECORDS {
            let compression = self.wal_compression;
            return records
                .par_iter()
                .map(|record| codec::prepare_record_with_compression(record, compression))
                .collect();
        }

        records
            .iter()
            .map(|record| self.prepare_record(record))
            .collect()
    }

    fn flush_durable_grouped(&self, required_lsn: Lsn) -> DbResult<()> {
        let mut request_registered = false;

        loop {
            let mut state = self.lock_group_commit_state()?;
            if !request_registered {
                state.pending_requests = state.pending_requests.saturating_add(1);
                request_registered = true;
                update_max_u32(&self.group_commit_queue_depth_peak, state.pending_requests);
                if state.leader_in_progress {
                    self.group_commit_cv.notify_all();
                }
            }

            if required_lsn <= state.last_durable_lsn {
                state.pending_requests = state.pending_requests.saturating_sub(1);
                return Ok(());
            }

            if !state.leader_in_progress {
                state.leader_in_progress = true;
                state.last_flush_error = None;

                if self.group_commit_delay.is_zero() {
                    // Zero delay: yield once to let concurrent appenders finish
                    // without adding measurable latency.
                    drop(state);
                    std::thread::yield_now();
                } else {
                    // Non-zero delay: hold the batching window only until
                    // another committer joins. The first follower gives us a
                    // real group to fsync, so waiting out the full window just
                    // adds latency under concurrent write load.
                    let (_state, _timeout) = self
                        .group_commit_cv
                        .wait_timeout_while(state, self.group_commit_delay, |state| {
                            state.pending_requests <= 1
                        })
                        .map_err(|e| {
                            DbError::internal(format!("WAL group commit lock poisoned: {e}"))
                        })?;
                }

                let result = self.flush_durable_grouped_leader();

                let mut state = self.lock_group_commit_state()?;
                state.leader_in_progress = false;
                state.pending_requests = state.pending_requests.saturating_sub(1);

                match result {
                    Ok(flushed_lsn) => {
                        state.last_durable_lsn = flushed_lsn;
                        state.last_flush_error = None;
                        self.group_commit_cv.notify_all();
                        return Ok(());
                    }
                    Err(error) => {
                        state.last_flush_error = Some(error.clone());
                        self.group_commit_cv.notify_all();
                        return Err(error);
                    }
                }
            }

            while required_lsn > state.last_durable_lsn && state.leader_in_progress {
                state = self.group_commit_cv.wait(state).map_err(|e| {
                    DbError::internal(format!("WAL group commit lock poisoned: {e}"))
                })?;
            }

            if required_lsn <= state.last_durable_lsn {
                state.pending_requests = state.pending_requests.saturating_sub(1);
                return Ok(());
            }

            if let Some(error) = state.last_flush_error.clone() {
                state.pending_requests = state.pending_requests.saturating_sub(1);
                return Err(error);
            }
        }
    }

    fn flush_commit(&self, writer: &mut WalWriter) -> DbResult<CommitFlushAction> {
        match self.commit_policy {
            WalCommitPolicy::Always => {
                let flush_start = Instant::now();
                let prepared_sync = self.prepare_durable_sync_with_writer(writer)?;
                Ok(CommitFlushAction::DurableSync(prepared_sync, flush_start))
            }
            WalCommitPolicy::Never => {
                self.flush_non_durable(writer)?;
                Ok(CommitFlushAction::None)
            }
            WalCommitPolicy::Every(interval) => {
                let next_pending = self
                    .pending_commit_flushes
                    .load(Ordering::Relaxed)
                    .saturating_add(1);
                if next_pending >= interval {
                    let flush_start = Instant::now();
                    let prepared_sync = self.prepare_durable_sync_with_writer(writer)?;
                    return Ok(CommitFlushAction::DurableSync(prepared_sync, flush_start));
                } else {
                    self.flush_non_durable(writer)?;
                    self.pending_commit_flushes
                        .store(next_pending, Ordering::Relaxed);
                }
                Ok(CommitFlushAction::None)
            }
        }
    }

    fn flush_durable_grouped_leader(&self) -> DbResult<Lsn> {
        #[cfg(test)]
        self.consume_injected_failure(InjectedWalFailure::Flush)?;
        let flush_start = Instant::now();
        let prepared_sync = self.prepare_durable_sync_locked()?;
        self.complete_prepared_durable_sync(prepared_sync, flush_start)
    }

    fn prepare_durable_sync_with_writer(
        &self,
        writer: &mut WalWriter,
    ) -> DbResult<Option<(std::fs::File, Lsn)>> {
        #[cfg(test)]
        self.consume_injected_failure(InjectedWalFailure::Flush)?;
        writer.prepare_durable_sync()
    }

    fn prepare_durable_sync_locked(&self) -> DbResult<Option<(std::fs::File, Lsn)>> {
        let mut writer = self.lock_writer()?;
        writer.prepare_durable_sync()
    }

    fn complete_prepared_durable_sync(
        &self,
        prepared_sync: Option<(std::fs::File, Lsn)>,
        flush_start: Instant,
    ) -> DbResult<Lsn> {
        let durable_lsn = prepared_sync
            .as_ref()
            .map(|(_, durable_lsn)| *durable_lsn)
            .unwrap_or(Lsn::ZERO);
        let _sync_guard = self
            .durable_sync_lock
            .lock()
            .map_err(|e| DbError::internal(format!("WAL durable sync lock poisoned: {e}")))?;
        if durable_lsn != Lsn::ZERO && durable_lsn <= self.current_durable_lsn()? {
            return Ok(durable_lsn);
        }
        if let Some((file, _)) = prepared_sync {
            WalWriter::sync_file_durable(&file)?;
        }
        self.finish_durable_flush(durable_lsn, flush_start)
    }

    fn complete_commit_flush_action(&self, action: CommitFlushAction) -> DbResult<()> {
        match action {
            CommitFlushAction::None => Ok(()),
            CommitFlushAction::DurableSync(prepared_sync, flush_start) => self
                .complete_prepared_durable_sync(prepared_sync, flush_start)
                .map(|_| ()),
        }
    }

    fn finish_durable_flush(&self, durable_lsn: Lsn, flush_start: Instant) -> DbResult<Lsn> {
        let flush_micros = u64::try_from(flush_start.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.durable_flush_total.fetch_add(1, Ordering::Relaxed);
        self.durable_flush_micros_total
            .fetch_add(flush_micros, Ordering::Relaxed);
        update_max_u64(&self.durable_flush_micros_max, flush_micros);
        let written_bytes_total = self.written_bytes_total.load(Ordering::Relaxed);
        self.durable_bytes_total
            .fetch_max(written_bytes_total, Ordering::Relaxed);
        self.pending_commit_flushes.store(0, Ordering::Relaxed);
        self.notify_wal_advance(durable_lsn);
        {
            let mut state = self.lock_group_commit_state()?;
            if durable_lsn > state.last_durable_lsn {
                state.last_durable_lsn = durable_lsn;
            }
            state.last_flush_error = None;
        }
        self.group_commit_cv.notify_all();
        #[cfg(test)]
        self.durable_flush_count.fetch_add(1, Ordering::Relaxed);
        Ok(durable_lsn)
    }

    fn current_durable_lsn(&self) -> DbResult<Lsn> {
        Ok(self.lock_group_commit_state()?.last_durable_lsn)
    }

    fn flush_non_durable(&self, writer: &mut WalWriter) -> DbResult<()> {
        #[cfg(test)]
        self.consume_injected_failure(InjectedWalFailure::Flush)?;
        writer.flush()?;
        self.notify_wal_advance(writer.last_lsn().unwrap_or(Lsn::ZERO));
        Ok(())
    }

    fn account_append_bytes(&self, writer: &WalWriter) {
        if let Some(entry_bytes) = writer.last_entry_bytes() {
            self.written_bytes_total
                .fetch_add(entry_bytes, Ordering::Relaxed);
        }
    }

    fn account_append_batch_bytes(&self, batch: AppendBatchResult) {
        self.written_bytes_total
            .fetch_add(batch.total_bytes, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(super) fn inject_failure(&self, failure: InjectedWalFailure) -> DbResult<()> {
        let mut guard = self
            .fail_next_operation
            .lock()
            .map_err(|e| DbError::internal(format!("WAL failure injection lock poisoned: {e}")))?;
        *guard = Some(failure);
        Ok(())
    }

    #[cfg(test)]
    fn consume_injected_failure(&self, expected: InjectedWalFailure) -> DbResult<()> {
        let mut guard = self
            .fail_next_operation
            .lock()
            .map_err(|e| DbError::internal(format!("WAL failure injection lock poisoned: {e}")))?;
        if guard.as_ref().copied() == Some(expected) {
            *guard = None;
            return Err(DbError::internal(format!(
                "injected WAL {expected:?} failure"
            )));
        }
        Ok(())
    }

    #[cfg(test)]
    fn durable_flush_count(&self) -> u64 {
        self.durable_flush_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn install_prepare_record_hook_for_tests(
        started_tx: std::sync::mpsc::Sender<()>,
        release_rx: std::sync::mpsc::Receiver<()>,
    ) {
        PREPARE_RECORD_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(PrepareRecordHook {
                started_tx,
                release_rx,
            });
        });
    }

    fn notify_wal_advance(&self, lsn: Lsn) {
        let notifier = match self.wal_notifier.lock() {
            Ok(guard) => guard.as_ref().cloned(),
            Err(_) => return,
        };
        if let Some(notifier) = notifier {
            notifier.notify_new_wal(lsn);
        }
    }
}

fn validate_commit_policy(policy: WalCommitPolicy) -> DbResult<()> {
    match policy {
        WalCommitPolicy::Every(0) => Err(DbError::internal(
            "WAL commit policy Every(0) requires interval >= 1",
        )),
        _ => Ok(()),
    }
}

fn update_max_u64(target: &AtomicU64, candidate: u64) {
    let mut current = target.load(Ordering::Relaxed);
    while candidate > current {
        match target.compare_exchange_weak(current, candidate, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

fn update_max_u32(target: &AtomicU32, candidate: u32) {
    let mut current = target.load(Ordering::Relaxed);
    while candidate > current {
        match target.compare_exchange_weak(current, candidate, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn test_dir(name: &str) -> PathBuf {
        crate::test_support::unique_temp_path("wal-int-test", name)
    }

    fn test_config(dir: PathBuf) -> WalConfig {
        WalConfig {
            dir,
            segment_max_bytes: 16 * 1024 * 1024,
            sync_on_flush: false,
            group_commit_delay_micros: 0,
            wal_compression: aiondb_wal::WalCompression::None,
            wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
        }
    }

    fn test_config_with_group_delay(dir: PathBuf, delay_micros: u64) -> WalConfig {
        WalConfig {
            dir,
            segment_max_bytes: 16 * 1024 * 1024,
            sync_on_flush: false,
            group_commit_delay_micros: delay_micros,
            wal_compression: aiondb_wal::WalCompression::None,
            wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
        }
    }

    #[test]
    fn majority_write_concern_requires_strict_majority() {
        assert_eq!(compute_required_acks(CONCERN_MAJORITY, 0), 0);
        assert_eq!(compute_required_acks(CONCERN_MAJORITY, 1), 1);
        assert_eq!(compute_required_acks(CONCERN_MAJORITY, 2), 1);
        assert_eq!(compute_required_acks(CONCERN_MAJORITY, 3), 2);
    }

    #[test]
    fn factor_write_concern_does_not_degrade_when_replicas_are_disconnected() {
        assert_eq!(compute_required_acks(CONCERN_FACTOR_BASE + 2, 0), 2);
        assert_eq!(compute_required_acks(CONCERN_FACTOR_BASE + 2, 1), 2);
        assert_eq!(compute_required_acks(CONCERN_FACTOR_BASE + 2, 2), 2);
    }

    #[test]
    fn auto_txn_ids_are_sequential() {
        let dir = test_dir("auto_seq");
        let wal = WalIntegration::open(test_config(dir.clone())).unwrap();
        let id1 = wal.next_auto_txn_id();
        let id2 = wal.next_auto_txn_id();
        assert_ne!(id1, id2);
        assert_eq!(id1.get() + 1, id2.get());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_txn_ids_start_high() {
        let dir = test_dir("auto_high");
        let wal = WalIntegration::open(test_config(dir.clone())).unwrap();
        let id = wal.next_auto_txn_id();
        assert!(id.get() >= AUTO_TXN_BASE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_and_flush_writes_to_disk() {
        let dir = test_dir("log_flush");
        let wal = WalIntegration::open(test_config(dir.clone())).unwrap();
        let record = WalRecord::Checkpoint {
            last_committed_lsn: Lsn::new(0),
        };
        let lsn = wal.log_and_flush(&record).unwrap();
        assert_eq!(lsn, Lsn::new(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_batch_writes_multiple_records() {
        let dir = test_dir("log_batch");
        let wal = WalIntegration::open(test_config(dir.clone())).unwrap();
        let records = vec![
            WalRecord::Checkpoint {
                last_committed_lsn: Lsn::new(0),
            },
            WalRecord::Checkpoint {
                last_committed_lsn: Lsn::new(1),
            },
        ];
        let lsn = wal.log_batch_and_flush(&records).unwrap();
        assert_eq!(lsn, Lsn::new(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_and_commit_with_never_policy_skips_durable_flush() {
        let dir = test_dir("log_commit_never");
        let wal = WalIntegration::open_with_commit_policy(
            test_config(dir.clone()),
            WalCommitPolicy::Never,
        )
        .unwrap();
        let record = WalRecord::Checkpoint {
            last_committed_lsn: Lsn::new(0),
        };

        let lsn = wal.log_and_commit(&record).unwrap();

        assert_eq!(lsn, Lsn::new(1));
        assert_eq!(wal.durable_flush_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_batch_and_commit_batches_durable_flushes_for_every_policy() {
        let dir = test_dir("log_batch_commit_every");
        let wal = WalIntegration::open_with_commit_policy(
            test_config(dir.clone()),
            WalCommitPolicy::Every(2),
        )
        .unwrap();
        let record = WalRecord::Checkpoint {
            last_committed_lsn: Lsn::new(0),
        };

        let batch_lsn = wal
            .log_batch_and_commit(&[record.clone(), record.clone()])
            .unwrap();
        assert_eq!(batch_lsn, Lsn::new(2));
        assert_eq!(wal.durable_flush_count(), 0);

        let single_lsn = wal.log_and_commit(&record).unwrap();
        assert_eq!(single_lsn, Lsn::new(3));
        assert_eq!(wal.durable_flush_count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forced_durable_flush_resets_every_commit_counter() {
        let dir = test_dir("forced_flush_resets_every");
        let wal = WalIntegration::open_with_commit_policy(
            test_config(dir.clone()),
            WalCommitPolicy::Every(3),
        )
        .unwrap();
        let record = WalRecord::Checkpoint {
            last_committed_lsn: Lsn::new(0),
        };

        wal.log_and_commit(&record).unwrap();
        wal.log_and_commit(&record).unwrap();
        assert_eq!(wal.durable_flush_count(), 0);

        wal.log_and_flush(&record).unwrap();
        assert_eq!(wal.durable_flush_count(), 1);

        wal.log_and_commit(&record).unwrap();
        wal.log_and_commit(&record).unwrap();
        assert_eq!(wal.durable_flush_count(), 1);
        wal.log_and_commit(&record).unwrap();
        assert_eq!(wal.durable_flush_count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_policy_releases_writer_lock_while_durable_sync_waits() {
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = test_dir("every_policy_releases_lock_during_sync");
        let wal = Arc::new(
            WalIntegration::open_with_commit_policy(
                test_config(dir.clone()),
                WalCommitPolicy::Every(1),
            )
            .unwrap(),
        );
        let record = WalRecord::Checkpoint {
            last_committed_lsn: Lsn::new(0),
        };
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let wal_flush = Arc::clone(&wal);
        let record_flush = record.clone();
        let handle = thread::spawn(move || {
            aiondb_wal::writer::install_durable_sync_hook_for_tests(started_tx, release_rx);
            wal_flush.log_and_commit(&record_flush)
        });

        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("durable sync should start and block");

        let concurrent_lsn = wal.log(&record).expect("concurrent append should proceed");
        assert_eq!(concurrent_lsn, Lsn::new(2));

        release_tx
            .send(())
            .expect("durable sync release should be deliverable");
        let flushed_lsn = handle.join().expect("flush thread join").unwrap();
        assert_eq!(flushed_lsn, Lsn::new(1));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wal_record_preparation_happens_before_writer_lock() {
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = test_dir("prepare_before_writer_lock");
        let mut config = test_config(dir.clone());
        config.wal_compression = aiondb_wal::WalCompression::Zstd;
        let wal = Arc::new(WalIntegration::open(config).unwrap());
        let record = WalRecord::FullPageImage {
            relation_id: RelationId::new(7),
            page_number: 42,
            page_data: vec![b'x'; 32 * 1024],
        };
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let wal_blocked = Arc::clone(&wal);
        let record_blocked = record.clone();
        let handle = thread::spawn(move || {
            WalIntegration::install_prepare_record_hook_for_tests(started_tx, release_rx);
            wal_blocked.log(&record_blocked)
        });

        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("record preparation should start and block");

        let concurrent_lsn = wal.log(&record).expect("concurrent append should proceed");
        assert_eq!(concurrent_lsn, Lsn::new(1));

        release_tx
            .send(())
            .expect("record preparation release should be deliverable");
        let blocked_lsn = handle.join().expect("prepare thread join").unwrap();
        assert_eq!(blocked_lsn, Lsn::new(2));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn group_commit_delay_batches_concurrent_commits() {
        let dir = test_dir("group_commit_delay_batches");
        let wal = Arc::new(
            WalIntegration::open_with_commit_policy(
                // Keep a wider group-delay window to reduce scheduler jitter flakiness in CI.
                test_config_with_group_delay(dir.clone(), 200_000),
                WalCommitPolicy::Always,
            )
            .unwrap(),
        );

        let record = WalRecord::Checkpoint {
            last_committed_lsn: Lsn::new(0),
        };
        let barrier = Arc::new(Barrier::new(3));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let wal = Arc::clone(&wal);
            let barrier = Arc::clone(&barrier);
            let record = record.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                wal.log_and_commit(&record).unwrap();
            }));
        }

        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }

        let metrics = wal.runtime_metrics_snapshot().unwrap();
        assert!(
            metrics.group_commit_queue_depth_peak >= 2,
            "concurrent commits should overlap in the group-commit queue"
        );
        assert!(
            wal.durable_flush_count() <= 2,
            "group commit should not degenerate into more than one flush per concurrent commit"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn group_commit_delay_wakes_when_follower_joins() {
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = test_dir("group_commit_delay_wakes_on_follower");
        let wal = Arc::new(
            WalIntegration::open_with_commit_policy(
                test_config_with_group_delay(dir.clone(), 5_000_000),
                WalCommitPolicy::Always,
            )
            .unwrap(),
        );
        let record = WalRecord::Checkpoint {
            last_committed_lsn: Lsn::new(0),
        };
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let leader_wal = Arc::clone(&wal);
        let leader_record = record.clone();
        let leader = thread::spawn(move || {
            aiondb_wal::writer::install_durable_sync_hook_for_tests(started_tx, release_rx);
            leader_wal.log_and_commit(&leader_record)
        });

        assert!(
            started_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "leader should wait for the group-commit window before any follower joins"
        );

        let follower_wal = Arc::clone(&wal);
        let follower = thread::spawn(move || follower_wal.log_and_commit(&record));

        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("follower should wake the group-commit leader before the full delay");
        release_tx
            .send(())
            .expect("durable sync release should be deliverable");

        leader.join().expect("leader join").unwrap();
        follower.join().expect("follower join").unwrap();

        assert_eq!(
            wal.durable_flush_count(),
            1,
            "leader and follower should share one durable flush"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zero_group_commit_delay_does_not_batch_commits() {
        let dir = test_dir("zero_group_delay_no_batch");
        let wal = Arc::new(
            WalIntegration::open_with_commit_policy(
                test_config_with_group_delay(dir.clone(), 0),
                WalCommitPolicy::Always,
            )
            .unwrap(),
        );

        let record = WalRecord::Checkpoint {
            last_committed_lsn: Lsn::new(0),
        };
        wal.log_and_commit(&record).unwrap();
        wal.log_and_commit(&record).unwrap();

        assert_eq!(wal.durable_flush_count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_zero_commit_policy_is_rejected() {
        let dir = test_dir("every_zero_commit_policy");
        let err = WalIntegration::open_with_commit_policy(
            test_config(dir.clone()),
            WalCommitPolicy::Every(0),
        )
        .expect_err("Every(0) must be rejected");

        assert!(err
            .to_string()
            .contains("WAL commit policy Every(0) requires interval >= 1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_lsn_advances() {
        let dir = test_dir("next_lsn");
        let wal = WalIntegration::open(test_config(dir.clone())).unwrap();
        assert_eq!(wal.next_lsn().unwrap(), Lsn::new(1));

        let record = WalRecord::Checkpoint {
            last_committed_lsn: Lsn::new(0),
        };
        wal.log(&record).unwrap();
        assert_eq!(wal.next_lsn().unwrap(), Lsn::new(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_before_removes_old_segments() {
        let dir = test_dir("cleanup_before");
        let config = WalConfig {
            dir: dir.clone(),
            segment_max_bytes: 100, // Small to force many segments
            sync_on_flush: false,
            group_commit_delay_micros: 0,
            wal_compression: aiondb_wal::WalCompression::None,
            wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
        };

        let wal = WalIntegration::open(config).unwrap();
        let record = WalRecord::Checkpoint {
            last_committed_lsn: Lsn::new(0),
        };

        // Write many records to create multiple segments.
        let mut last_lsn = Lsn::ZERO;
        for _ in 0..30 {
            last_lsn = wal.log(&record).unwrap();
        }
        wal.flush().unwrap();

        let segments_before = aiondb_wal::segment::list_segments(&dir).unwrap();
        assert!(
            segments_before.len() >= 3,
            "Expected at least 3 segments, got {}",
            segments_before.len()
        );

        let removed = wal.cleanup_before(last_lsn).unwrap();
        assert!(removed > 0, "Expected some segments to be removed");

        let segments_after = aiondb_wal::segment::list_segments(&dir).unwrap();
        assert!(segments_after.len() < segments_before.len());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn runtime_metrics_track_written_and_durable_bytes() {
        let dir = test_dir("runtime_metrics_written_and_durable_bytes");
        let wal = WalIntegration::open(test_config(dir.clone())).unwrap();
        let record = WalRecord::Checkpoint {
            last_committed_lsn: Lsn::new(0),
        };

        wal.log(&record).unwrap();
        let after_append = wal.runtime_metrics_snapshot().unwrap();
        assert!(after_append.written_bytes_total > 0);
        assert_eq!(after_append.durable_bytes_total, 0);

        wal.flush_durable().unwrap();
        let after_flush = wal.runtime_metrics_snapshot().unwrap();
        assert_eq!(
            after_flush.written_bytes_total,
            after_append.written_bytes_total
        );
        assert_eq!(
            after_flush.durable_bytes_total,
            after_append.written_bytes_total
        );
        assert_eq!(after_flush.durable_flush_total, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn runtime_metrics_track_batch_written_and_durable_bytes() {
        let dir = test_dir("runtime_metrics_batch_written_and_durable_bytes");
        let wal = WalIntegration::open(test_config(dir.clone())).unwrap();
        let records = vec![
            WalRecord::Checkpoint {
                last_committed_lsn: Lsn::new(0),
            },
            WalRecord::Checkpoint {
                last_committed_lsn: Lsn::new(1),
            },
            WalRecord::Checkpoint {
                last_committed_lsn: Lsn::new(2),
            },
        ];

        let lsn = wal.log_batch(&records).unwrap().unwrap();
        assert_eq!(lsn, Lsn::new(3));

        let after_append = wal.runtime_metrics_snapshot().unwrap();
        assert!(after_append.written_bytes_total > 0);
        assert_eq!(after_append.durable_bytes_total, 0);

        wal.flush_durable().unwrap();
        let after_flush = wal.runtime_metrics_snapshot().unwrap();
        assert_eq!(
            after_flush.written_bytes_total,
            after_append.written_bytes_total
        );
        assert_eq!(
            after_flush.durable_bytes_total,
            after_append.written_bytes_total
        );
        assert_eq!(after_flush.durable_flush_total, 1);
        assert!(
            after_flush.written_bytes_total > 3,
            "batch accounting must include the encoded batch bytes, not only record count"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_policy_metrics_only_advance_durable_bytes_on_triggered_flush() {
        let dir = test_dir("every_policy_metrics_only_advance_on_flush");
        let wal = WalIntegration::open_with_commit_policy(
            test_config(dir.clone()),
            WalCommitPolicy::Every(3),
        )
        .unwrap();
        let record = WalRecord::Checkpoint {
            last_committed_lsn: Lsn::new(0),
        };

        wal.log_and_commit(&record).unwrap();
        let after_first = wal.runtime_metrics_snapshot().unwrap();
        assert!(after_first.written_bytes_total > 0);
        assert_eq!(after_first.durable_bytes_total, 0);

        wal.log_and_commit(&record).unwrap();
        let after_second = wal.runtime_metrics_snapshot().unwrap();
        assert!(after_second.written_bytes_total > after_first.written_bytes_total);
        assert_eq!(after_second.durable_bytes_total, 0);

        wal.log_and_commit(&record).unwrap();
        let after_third = wal.runtime_metrics_snapshot().unwrap();
        assert_eq!(
            after_third.durable_bytes_total,
            after_third.written_bytes_total
        );
        assert!(after_third.durable_bytes_total > after_second.durable_bytes_total);
        assert_eq!(after_third.durable_flush_total, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
