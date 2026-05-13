//! Append-only WAL writer.
//!
//! # Durability invariants (production-critical)
//!
//! * **Append → assign LSN.** [`WalWriter::append`] must atomically encode
//!   the record, link `prev_lsn`, advance `last_lsn`, and write to the
//!   active segment **before** returning the LSN to callers. Reordering
//!   these steps allows a partial publish where another thread observes
//!   an LSN that does not yet correspond to durable bytes.
//! * **Group commit fences.** Higher layers may issue many `append`s and
//!   then call [`WalWriter::flush_durable`] once. The writer guarantees
//!   that after `flush_durable` returns Ok, every byte for every record
//!   whose LSN is `<= last_lsn` is durable on disk and the segment file
//!   has been fsynced.
//! * **Rotation.** If the encoded entry would overflow
//!   [`WalConfig::segment_max_bytes`], the active segment is durably
//!   flushed and a fresh segment is created **before** the entry is
//!   written. The old segment is fsynced and the directory entry is
//!   committed so a crash mid-rotation cannot leave a dangling tail.
//! * **Recovery.** On open, the writer scans the existing segment list,
//!   validates the per-record LSN chain, and truncates a corrupt tail
//!   only on the last active segment. Corrupted archived segments are a
//!   data loss.
//! * **LSN modes.** Both `Logical` and `ByteOffset` LSN progressions are
//!   supported; only one is active at a time, controlled by
//!   `AIONDB_WAL_LSN_MODE` (env) or [`WalConfig::wal_lsn_mode`]. Switching
//!   modes at runtime is unsafe and not supported.
//! * **Sync method.** Durable flushes default to `fdatasync`-style behavior
//!   via `sync_data()`, but can be overridden with `AIONDB_WAL_SYNC_METHOD`
//!   (`fdatasync`, `fsync`, `fullsync`).

use std::fs::File;
use std::io::{BufWriter, Write};
#[cfg(target_vendor = "apple")]
use std::os::fd::AsRawFd;
use std::sync::{mpsc, OnceLock};

use aiondb_core::{
    convert::{u32_to_usize_saturating, usize_to_u64_saturating},
    DbError, DbResult,
};

use crate::codec;
use crate::lsn::Lsn;
use crate::record::{WalEntry, WalRecord};
use crate::segment::{self, LocalWalAuthState, SegmentId, SegmentLsnMode};
use crate::{WalConfig, WalLsnMode};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LsnStepMode {
    Logical,
    ByteOffset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WalSyncMethod {
    Fdatasync,
    Fsync,
    Fullsync,
}

const DEFAULT_MAX_SEGMENT_SCAN_BYTES: u64 = 64 * 1024 * 1024;
const MIN_MAX_SEGMENT_SCAN_BYTES: u64 = 1024 * 1024;
const MAX_MAX_SEGMENT_SCAN_BYTES: u64 = segment::WAL_SEGMENT_SCAN_HARD_LIMIT_BYTES;

fn parse_wal_sync_method(value: Option<&str>) -> WalSyncMethod {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("fsync" | "sync_all") => WalSyncMethod::Fsync,
        Some("fullsync" | "full_fsync" | "full") => WalSyncMethod::Fullsync,
        _ => WalSyncMethod::Fdatasync,
    }
}

fn preferred_wal_sync_method() -> WalSyncMethod {
    parse_wal_sync_method(std::env::var("AIONDB_WAL_SYNC_METHOD").ok().as_deref())
}

fn parse_max_segment_scan_bytes(value: Option<&str>) -> u64 {
    value
        .and_then(|raw| raw.parse::<u64>().ok())
        .map_or(DEFAULT_MAX_SEGMENT_SCAN_BYTES, |bytes| {
            bytes.clamp(MIN_MAX_SEGMENT_SCAN_BYTES, MAX_MAX_SEGMENT_SCAN_BYTES)
        })
}

fn max_segment_scan_bytes() -> u64 {
    static MAX_SEGMENT_SCAN_BYTES: OnceLock<u64> = OnceLock::new();
    *MAX_SEGMENT_SCAN_BYTES.get_or_init(|| {
        parse_max_segment_scan_bytes(
            std::env::var("AIONDB_WAL_MAX_SEGMENT_READ_BYTES")
                .ok()
                .as_deref(),
        )
    })
}

fn effective_segment_scan_cap(config: &WalConfig) -> u64 {
    let configured_budget = config.segment_max_bytes.saturating_mul(2);
    max_segment_scan_bytes()
        .max(configured_budget)
        .min(segment::WAL_SEGMENT_SCAN_HARD_LIMIT_BYTES)
}

fn read_segment_bytes_for_scan(
    config: &WalConfig,
    seg_id: SegmentId,
    context: &str,
) -> DbResult<Vec<u8>> {
    let cap = effective_segment_scan_cap(config);
    segment::read_segment_bytes_bounded(&config.dir, seg_id, cap, context)
}

fn lsn_step_mode_from_env() -> Option<LsnStepMode> {
    std::env::var("AIONDB_WAL_LSN_MODE").ok().map(|value| {
        match WalLsnMode::from_str_value(&value) {
            Some(WalLsnMode::Logical) => LsnStepMode::Logical,
            Some(WalLsnMode::ByteOffset) | None => LsnStepMode::ByteOffset,
        }
    })
}

fn configured_lsn_step_mode(config: &WalConfig) -> LsnStepMode {
    match config.wal_lsn_mode {
        WalLsnMode::Logical => LsnStepMode::Logical,
        WalLsnMode::ByteOffset => LsnStepMode::ByteOffset,
    }
}

fn wal_lsn_mode_from_step_mode(mode: LsnStepMode) -> WalLsnMode {
    match mode {
        LsnStepMode::Logical => WalLsnMode::Logical,
        LsnStepMode::ByteOffset => WalLsnMode::ByteOffset,
    }
}

/// Appends WAL entries to segment files, handling rotation when segments
/// exceed the configured maximum size.
pub struct WalWriter {
    config: WalConfig,
    current_segment: SegmentId,
    current_file: BufWriter<File>,
    current_size: u64,
    local_auth_state: Option<LocalWalAuthState>,
    local_auth_persisted_len: Option<u64>,
    next_lsn: Lsn,
    last_lsn: Option<Lsn>,
    last_entry_bytes: Option<u64>,
    lsn_step_mode: LsnStepMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AppendBatchResult {
    pub last_lsn: Lsn,
    pub total_bytes: u64,
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_DURABLE_FLUSH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NEXT_APPEND_AFTER_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct DurableSyncHook {
    started_tx: mpsc::Sender<()>,
    release_rx: mpsc::Receiver<()>,
}

thread_local! {
    static DURABLE_SYNC_HOOK: std::cell::RefCell<Option<DurableSyncHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn inject_fail_next_append_after_write() {
    FAIL_NEXT_APPEND_AFTER_WRITE.set(true);
}

#[doc(hidden)]
pub fn install_durable_sync_hook_for_tests(
    started_tx: mpsc::Sender<()>,
    release_rx: mpsc::Receiver<()>,
) {
    DURABLE_SYNC_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(DurableSyncHook {
            started_tx,
            release_rx,
        });
    });
}

impl WalWriter {
    fn wrap_current_file(file: File) -> BufWriter<File> {
        BufWriter::new(file)
    }

    /// Open the WAL for writing.
    ///
    /// Scans existing segments to find the correct next LSN and active
    /// segment. If the WAL directory is empty a fresh segment is created.
    ///
    /// On recovery the last segment is truncated at the last valid entry
    /// boundary so that future appends are not hidden behind corrupt bytes.
    /// If the last segment is empty, earlier segments are scanned to recover
    /// the correct highest LSN.
    pub fn open(config: WalConfig) -> DbResult<Self> {
        segment::ensure_wal_dir(&config.dir)?;
        let segments = segment::list_segments(&config.dir)?;
        let expected_identity = segment::resolve_cluster_identity_from_wal_dir(&config.dir);
        let preferred_mode =
            lsn_step_mode_from_env().unwrap_or_else(|| configured_lsn_step_mode(&config));

        if segments.is_empty() {
            // Fresh WAL - start at segment 1, LSN 1.
            let seg_id = SegmentId::new(1);
            let file = segment::open_segment_for_append(
                &config.dir,
                seg_id,
                wal_lsn_mode_from_step_mode(preferred_mode),
            )?;
            let size = segment::segment_size(&config.dir, seg_id)?;
            let initial_bytes = segment::read_segment_bytes_bounded(
                &config.dir,
                seg_id,
                size,
                "startup auth init",
            )?;
            let local_auth_state = LocalWalAuthState::from_existing_segment_bytes(&initial_bytes)?;
            return Ok(Self {
                config,
                current_segment: seg_id,
                current_file: Self::wrap_current_file(file),
                current_size: size,
                local_auth_state,
                local_auth_persisted_len: None,
                next_lsn: Lsn::new(1),
                last_lsn: None,
                last_entry_bytes: None,
                lsn_step_mode: preferred_mode,
            });
        }

        let Some(&last_seg) = segments.last() else {
            return Err(DbError::internal(
                "WAL segment list unexpectedly empty after initial scan",
            ));
        };

        // Scan segments to find the highest valid LSN and detect the
        // LSN progression mode used by this WAL stream.
        let mut highest_lsn: Option<Lsn> = None;
        let mut highest_entry_bytes: Option<u64> = None;
        let mut previous_lsn: Option<Lsn> = None;
        let mut previous_entry_bytes: Option<u64> = None;
        let mut detected_mode: Option<LsnStepMode> = None;
        let mut header_mode: Option<LsnStepMode> = None;
        let mut header_system_identifier: Option<u64> = None;
        let mut header_timeline_id: Option<u32> = None;
        let mut last_seg_size_after_auth: Option<u64> = None;
        // Cumulative cap on the bytes loaded into memory during the recovery
        // scan. Each segment is already capped at `WAL_SEGMENT_SCAN_HARD_LIMIT
        // _BYTES` (256 MiB), but a hostile attacker who drops many forged
        // segments could otherwise multiply that bound by N (audit wal F5).
        const MAX_TOTAL_RECOVERY_SCAN_BYTES: u64 = 4 * 1024 * 1024 * 1024;
        let mut total_scan_bytes: u64 = 0;

        for &seg_id in &segments {
            let mut data = read_segment_bytes_for_scan(&config, seg_id, "startup scan")?;
            let auth = segment::verify_local_segment_integrity_if_configured(
                &config.dir,
                seg_id,
                seg_id == last_seg,
                &data,
            )?;
            if auth.trusted_len < u64::try_from(data.len()).unwrap_or(u64::MAX) {
                let path = config.dir.join(seg_id.filename());
                let trunc_file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .map_err(|e| DbError::internal(format!("WAL truncate failed: {e}")))?;
                trunc_file
                    .set_len(auth.trusted_len)
                    .map_err(|e| DbError::internal(format!("WAL truncate failed: {e}")))?;
                trunc_file
                    .sync_all()
                    .map_err(|e| DbError::internal(format!("WAL truncate failed: {e}")))?;
                segment::sync_dir(&config.dir)?;
                let trusted_usize = usize::try_from(auth.trusted_len)
                    .map_err(|_| DbError::internal("WAL trusted length exceeds usize"))?;
                data.truncate(trusted_usize);
            }
            if seg_id == last_seg {
                last_seg_size_after_auth = Some(u64::try_from(data.len()).unwrap_or(u64::MAX));
            }
            total_scan_bytes =
                total_scan_bytes.saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
            if total_scan_bytes > MAX_TOTAL_RECOVERY_SCAN_BYTES {
                return Err(DbError::internal(format!(
                    "WAL: cumulative recovery scan exceeded {MAX_TOTAL_RECOVERY_SCAN_BYTES} bytes"
                )));
            }

            let header = segment::parse_segment_header(&data)?;
            if let Some(lsn_mode) = header.lsn_mode {
                let mode = match lsn_mode {
                    SegmentLsnMode::Logical => LsnStepMode::Logical,
                    SegmentLsnMode::ByteOffset => LsnStepMode::ByteOffset,
                };
                if let Some(previous_header_mode) = header_mode {
                    if previous_header_mode != mode {
                        return Err(DbError::internal(format!(
                            "WAL segment header mode mismatch: segment {} uses {:?}, previous segments use {:?}",
                            seg_id.get(),
                            mode,
                            previous_header_mode
                        )));
                    }
                } else {
                    header_mode = Some(mode);
                }
            }
            if let Some(system_identifier) = header.system_identifier {
                if let Some(previous_system_identifier) = header_system_identifier {
                    if previous_system_identifier != system_identifier {
                        return Err(DbError::internal(format!(
                            "WAL segment header system identifier mismatch: segment {} uses {}, previous segments use {}",
                            seg_id.get(),
                            system_identifier,
                            previous_system_identifier
                        )));
                    }
                } else {
                    header_system_identifier = Some(system_identifier);
                }
            }
            if let Some(timeline_id) = header.timeline_id {
                if let Some(previous_timeline_id) = header_timeline_id {
                    if previous_timeline_id != timeline_id {
                        return Err(DbError::internal(format!(
                            "WAL segment header timeline mismatch: segment {} uses {}, previous segments use {}",
                            seg_id.get(),
                            timeline_id,
                            previous_timeline_id
                        )));
                    }
                } else {
                    header_timeline_id = Some(timeline_id);
                }
            }

            let mut offset = header.entry_offset;
            while offset < data.len() {
                match codec::decode_entry(&data[offset..]) {
                    Ok((entry, consumed)) => {
                        let consumed_u64 = usize_to_u64_saturating(consumed);

                        if let Some(prev_lsn) = previous_lsn {
                            if entry.prev_lsn != Lsn::ZERO && entry.prev_lsn != prev_lsn {
                                if seg_id == last_seg {
                                    break;
                                }
                                return Err(DbError::internal(format!(
                                    "WAL corruption in archived segment {} at offset {}: backward-chain mismatch expected prev_lsn {}, found {}",
                                    seg_id.get(),
                                    offset,
                                    prev_lsn.get(),
                                    entry.prev_lsn.get()
                                )));
                            }

                            let prev_entry_bytes = previous_entry_bytes.unwrap_or(1);
                            let logical_expected =
                                prev_lsn.checked_advance(1).ok_or_else(|| {
                                    DbError::internal(format!(
                                    "WAL LSN overflow in archived segment {} at offset {} after {}",
                                    seg_id.get(),
                                    offset,
                                    prev_lsn.get()
                                ))
                                })?;
                            let byte_expected = prev_lsn
                                .checked_advance(prev_entry_bytes)
                                .ok_or_else(|| {
                                    DbError::internal(format!(
                                        "WAL LSN overflow in archived segment {} at offset {} after {}",
                                        seg_id.get(),
                                        offset,
                                        prev_lsn.get()
                                    ))
                                })?;

                            match detected_mode {
                                Some(LsnStepMode::Logical) if entry.lsn != logical_expected => {
                                    if seg_id == last_seg {
                                        break;
                                    }
                                    return Err(DbError::internal(format!(
                                        "WAL corruption in archived segment {} at offset {}: expected logical LSN {}, found {}",
                                        seg_id.get(),
                                        offset,
                                        logical_expected.get(),
                                        entry.lsn.get()
                                    )));
                                }
                                Some(LsnStepMode::ByteOffset) if entry.lsn != byte_expected => {
                                    if seg_id == last_seg {
                                        break;
                                    }
                                    return Err(DbError::internal(format!(
                                        "WAL corruption in archived segment {} at offset {}: expected byte-offset LSN {}, found {}",
                                        seg_id.get(),
                                        offset,
                                        byte_expected.get(),
                                        entry.lsn.get()
                                    )));
                                }
                                None => {
                                    if entry.lsn == logical_expected {
                                        detected_mode = Some(LsnStepMode::Logical);
                                    } else if entry.lsn == byte_expected {
                                        detected_mode = Some(LsnStepMode::ByteOffset);
                                    } else {
                                        if seg_id == last_seg {
                                            break;
                                        }
                                        return Err(DbError::internal(format!(
                                            "WAL corruption in archived segment {} at offset {}: expected LSN {} (logical) or {} (byte-offset), found {}",
                                            seg_id.get(),
                                            offset,
                                            logical_expected.get(),
                                            byte_expected.get(),
                                            entry.lsn.get()
                                        )));
                                    }
                                }
                                _ => {}
                            }
                        }

                        highest_lsn = Some(entry.lsn);
                        highest_entry_bytes = Some(consumed_u64);
                        previous_lsn = Some(entry.lsn);
                        previous_entry_bytes = Some(consumed_u64);
                        offset += consumed;
                    }
                    Err(_) => break,
                }
            }

            if seg_id != last_seg && offset < data.len() {
                return Err(DbError::internal(format!(
                    "WAL corruption in archived segment {} at offset {}",
                    seg_id.get(),
                    offset
                )));
            }

            // Truncate any corrupt tail on the last segment so that future
            // appends are not hidden behind corrupt bytes.
            if seg_id == last_seg && offset < data.len() {
                let path = config.dir.join(seg_id.filename());
                let truncated_size = usize_to_u64_saturating(offset);
                let trunc_file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .map_err(|e| DbError::internal(format!("WAL truncate failed: {e}")))?;
                trunc_file
                    .set_len(truncated_size)
                    .map_err(|e| DbError::internal(format!("WAL truncate failed: {e}")))?;
                trunc_file
                    .sync_all()
                    .map_err(|e| DbError::internal(format!("WAL truncate failed: {e}")))?;
                segment::sync_dir(&config.dir)?;
                last_seg_size_after_auth = Some(truncated_size);
            }
        }

        // Get the (possibly truncated) size of the last segment.
        let size =
            last_seg_size_after_auth.unwrap_or(segment::segment_size(&config.dir, last_seg)?);
        if let Some(header_mode) = header_mode {
            if header_mode != preferred_mode {
                return Err(DbError::internal(format!(
                    "WAL LSN mode mismatch: requested '{}' but existing WAL header requires '{}'",
                    wal_lsn_mode_from_step_mode(preferred_mode).as_str(),
                    wal_lsn_mode_from_step_mode(header_mode).as_str()
                )));
            }
        }
        if let Some(system_identifier) = header_system_identifier {
            if let Some(expected_system_identifier) = expected_identity.system_identifier {
                if expected_system_identifier != system_identifier {
                    return Err(DbError::internal(format!(
                        "WAL system identifier mismatch: header has {}, local replication metadata has {}",
                        system_identifier, expected_system_identifier
                    )));
                }
            }
        }
        if let Some(timeline_id) = header_timeline_id {
            if let Some(expected_timeline_id) = expected_identity.timeline_id {
                if expected_timeline_id != timeline_id {
                    return Err(DbError::internal(format!(
                        "WAL timeline mismatch: header has {}, local replication metadata has {}",
                        timeline_id, expected_timeline_id
                    )));
                }
            }
        }

        let file = segment::open_segment_for_append(
            &config.dir,
            last_seg,
            wal_lsn_mode_from_step_mode(detected_mode.unwrap_or(preferred_mode)),
        )?;
        let initial_bytes =
            segment::read_segment_bytes_bounded(&config.dir, last_seg, size, "resume auth init")?;
        let local_auth_state = LocalWalAuthState::from_existing_segment_bytes(&initial_bytes)?;

        let lsn_step_mode = header_mode.or(detected_mode).unwrap_or(preferred_mode);
        let next_lsn = if let Some(last_lsn) = highest_lsn {
            let step = match lsn_step_mode {
                LsnStepMode::Logical => 1,
                LsnStepMode::ByteOffset => highest_entry_bytes.unwrap_or(1),
            };
            last_lsn.checked_advance(step).ok_or_else(|| {
                DbError::internal(format!(
                    "WAL LSN overflow while resuming after {}",
                    last_lsn.get()
                ))
            })?
        } else {
            Lsn::new(1)
        };

        Ok(Self {
            config,
            current_segment: last_seg,
            current_file: Self::wrap_current_file(file),
            current_size: size,
            local_auth_state,
            local_auth_persisted_len: None,
            next_lsn,
            last_lsn: highest_lsn,
            last_entry_bytes: highest_entry_bytes,
            lsn_step_mode,
        })
    }

    /// Append a record to the WAL. Returns the LSN assigned to this entry.
    pub fn append(&mut self, record: &WalRecord) -> DbResult<Lsn> {
        let prepared = codec::prepare_record_with_compression(record, self.config.wal_compression)?;
        self.append_prepared(&prepared)
    }

    pub fn append_prepared(&mut self, prepared: &codec::PreparedWalRecord) -> DbResult<Lsn> {
        let lsn = self.next_lsn;
        let encoded = codec::encode_prepared_entry(
            lsn,
            self.last_lsn.unwrap_or(Lsn::ZERO),
            WalEntry::LEGACY_DATABASE_ID,
            prepared,
        )?;
        let encoded_len = u64::try_from(encoded.len())
            .map_err(|_| DbError::internal("WAL: encoded entry length exceeds u64"))?;
        let next_lsn = self.next_lsn_from(lsn, encoded_len)?;

        // Rotate to a new segment if the current one would exceed the limit.
        // Use `checked_add` so a hypothetical 64-bit-rollover state cannot
        let projected = self
            .current_size
            .checked_add(encoded_len)
            .ok_or_else(|| DbError::internal("WAL: current_size + encoded_len overflows u64"))?;
        if self.current_size > 0 && projected > self.config.segment_max_bytes {
            self.rotate()?;
        }

        if let Err(error) = self.current_file.write_all(&encoded) {
            return Err(self.recover_failed_append(error));
        }

        #[cfg(test)]
        {
            let injected = FAIL_NEXT_APPEND_AFTER_WRITE.replace(false);
            if injected {
                return Err(self.recover_failed_append(std::io::Error::other(
                    "injected WAL append failure after write",
                )));
            }
        }

        self.current_size = self
            .current_size
            .checked_add(encoded_len)
            .ok_or_else(|| DbError::internal("WAL: current_size overflows u64"))?;
        if let Some(auth) = &mut self.local_auth_state {
            auth.update(&encoded);
        }
        self.last_lsn = Some(lsn);
        self.last_entry_bytes = Some(encoded_len);
        self.next_lsn = next_lsn;

        Ok(lsn)
    }

    pub fn append_prepared_batch(
        &mut self,
        prepared_records: &[codec::PreparedWalRecord],
    ) -> DbResult<Option<AppendBatchResult>> {
        if prepared_records.is_empty() {
            return Ok(None);
        }

        let mut chunk = Vec::new();
        let mut chunk_size = self.current_size;
        let mut chunk_last_lsn = self.last_lsn;
        let mut chunk_next_lsn = self.next_lsn;
        let mut batch_last_lsn = Lsn::ZERO;
        let mut batch_total_bytes = 0u64;
        let mut last_entry_bytes = None;

        for prepared in prepared_records {
            let lsn = chunk_next_lsn;
            let encoded = codec::encode_prepared_entry(
                lsn,
                chunk_last_lsn.unwrap_or(Lsn::ZERO),
                WalEntry::LEGACY_DATABASE_ID,
                prepared,
            )?;
            let encoded_len = u64::try_from(encoded.len())
                .map_err(|_| DbError::internal("WAL: encoded entry length exceeds u64"))?;
            let projected = chunk_size
                .checked_add(encoded_len)
                .ok_or_else(|| DbError::internal("WAL: chunk_size + encoded_len overflows u64"))?;

            if chunk_size > 0 && projected > self.config.segment_max_bytes {
                self.write_encoded_chunk(&chunk)?;
                self.last_lsn = chunk_last_lsn;
                self.last_entry_bytes = last_entry_bytes;
                self.next_lsn = chunk_next_lsn;
                chunk.clear();
                self.rotate()?;
                chunk_size = self.current_size;
                chunk_last_lsn = self.last_lsn;
                chunk_next_lsn = self.next_lsn;

                let lsn = chunk_next_lsn;
                let encoded_after_rotate = codec::encode_prepared_entry(
                    lsn,
                    chunk_last_lsn.unwrap_or(Lsn::ZERO),
                    WalEntry::LEGACY_DATABASE_ID,
                    prepared,
                )?;
                let encoded_len_after_rotate = u64::try_from(encoded_after_rotate.len())
                    .map_err(|_| DbError::internal("WAL: encoded entry length exceeds u64"))?;
                chunk.extend_from_slice(&encoded_after_rotate);
                chunk_size = chunk_size
                    .checked_add(encoded_len_after_rotate)
                    .ok_or_else(|| DbError::internal("WAL: chunk_size overflows u64"))?;
                chunk_last_lsn = Some(lsn);
                chunk_next_lsn = self.next_lsn_from(lsn, encoded_len_after_rotate)?;
                batch_last_lsn = lsn;
                batch_total_bytes = batch_total_bytes
                    .checked_add(encoded_len_after_rotate)
                    .ok_or_else(|| DbError::internal("WAL: batch total bytes overflows u64"))?;
                last_entry_bytes = Some(encoded_len_after_rotate);
                continue;
            }

            chunk.extend_from_slice(&encoded);
            chunk_size = projected;
            chunk_last_lsn = Some(lsn);
            chunk_next_lsn = self.next_lsn_from(lsn, encoded_len)?;
            batch_last_lsn = lsn;
            batch_total_bytes = batch_total_bytes
                .checked_add(encoded_len)
                .ok_or_else(|| DbError::internal("WAL: batch total bytes overflows u64"))?;
            last_entry_bytes = Some(encoded_len);
        }

        self.write_encoded_chunk(&chunk)?;
        self.last_lsn = chunk_last_lsn;
        self.last_entry_bytes = last_entry_bytes;
        self.next_lsn = chunk_next_lsn;

        Ok(Some(AppendBatchResult {
            last_lsn: batch_last_lsn,
            total_bytes: batch_total_bytes,
        }))
    }

    /// Flush buffered data to the OS. If `sync_on_flush` is enabled in the
    /// configuration an `fsync` is issued as well.
    pub fn flush(&mut self) -> DbResult<()> {
        self.current_file
            .flush()
            .map_err(|e| DbError::internal(format!("WAL flush failed: {e}")))?;
        if self.config.sync_on_flush {
            self.sync_current_file()?;
            self.persist_local_auth_if_needed()?;
        }
        Ok(())
    }

    /// Flush buffered data and force it to stable storage, regardless of the
    /// configured `sync_on_flush` policy.
    pub fn flush_durable(&mut self) -> DbResult<()> {
        if let Some((file, _durable_lsn)) = self.prepare_durable_sync()? {
            Self::sync_file_durable(&file)?;
            self.persist_local_auth_if_needed()?;
        }
        Ok(())
    }

    /// Flush buffered bytes into the kernel page cache, clone the active file
    /// descriptor, and return the last LSN covered by that flush. Callers can
    /// then run the expensive durable sync on the clone without holding the
    /// writer lock.
    pub fn prepare_durable_sync(&mut self) -> DbResult<Option<(File, Lsn)>> {
        self.current_file
            .flush()
            .map_err(|e| DbError::internal(format!("WAL flush failed: {e}")))?;
        let Some(durable_lsn) = self.last_lsn else {
            return Ok(None);
        };
        let cloned = self
            .current_file
            .get_ref()
            .try_clone()
            .map_err(|e| DbError::internal(format!("WAL file clone failed: {e}")))?;
        Ok(Some((cloned, durable_lsn)))
    }

    /// Returns the next LSN that will be assigned.
    pub fn next_lsn(&self) -> Lsn {
        self.next_lsn
    }

    /// Returns the last LSN durably present in WAL, if any.
    pub fn last_lsn(&self) -> Option<Lsn> {
        self.last_lsn
    }

    /// Returns the encoded byte length of the last successfully appended entry.
    pub fn last_entry_bytes(&self) -> Option<u64> {
        self.last_entry_bytes
    }

    /// Returns the current active segment ID.
    pub fn current_segment(&self) -> SegmentId {
        self.current_segment
    }

    /// Remove WAL segments whose entries are all before `before_lsn`.
    ///
    /// A segment is considered safe to remove when:
    /// - It is not the current active segment, AND
    /// - All its entries have LSNs strictly less than `before_lsn`.
    ///
    /// To determine this without reading every entry, we check whether the
    /// *next* segment in sorted order has a first entry with LSN ≤ `before_lsn`.
    /// If so, every entry in the preceding segment must be before the cutoff.
    ///
    /// Returns the number of segments removed.
    pub fn remove_segments_before(&mut self, before_lsn: Lsn) -> DbResult<u64> {
        self.remove_segments_before_with_min_segments(before_lsn, 0)
    }

    /// Remove WAL segments whose entries are all before `before_lsn`, while
    /// retaining at least `min_segments_to_keep` newest segments.
    pub fn remove_segments_before_with_min_segments(
        &mut self,
        before_lsn: Lsn,
        min_segments_to_keep: u32,
    ) -> DbResult<u64> {
        let segments = segment::list_segments(&self.config.dir)?;

        if segments.len() <= 1 {
            return Ok(0); // Nothing to remove - at most one segment.
        }

        let mut removed = 0u64;
        let keep_from = segments
            .len()
            .saturating_sub(u32_to_usize_saturating(min_segments_to_keep));

        // Walk pairs: for each segment[i], use segment[i+1]'s first entry LSN
        // to decide whether segment[i] is fully before the cutoff.
        for i in 0..segments.len() - 1 {
            let seg_id = segments[i];

            // Never remove the current active segment.
            if seg_id == self.current_segment || i >= keep_from {
                continue;
            }

            let next_seg_id = segments[i + 1];

            // Read the first entry of the next segment to find where it starts.
            let first_lsn_of_next = self.first_lsn_in_segment(next_seg_id)?;

            match first_lsn_of_next {
                Some(lsn) if lsn <= before_lsn => {
                    // All entries in seg_id have LSN < lsn <= before_lsn, safe to remove.
                    segment::archive_segment_if_configured(&self.config.dir, seg_id)?;
                    segment::recycle_segment(&self.config.dir, seg_id)?;
                    removed += 1;
                }
                _ => {
                    // The next segment starts at or after before_lsn, or is empty.
                    // Cannot be sure this segment is fully before the cutoff.
                }
            }
        }

        Ok(removed)
    }

    /// Read the first valid entry in a segment and return its LSN.
    /// Returns `None` if the segment is empty or has no valid entries.
    fn first_lsn_in_segment(&self, seg_id: SegmentId) -> DbResult<Option<Lsn>> {
        let data = read_segment_bytes_for_scan(&self.config, seg_id, "segment retention scan")?;

        let offset = segment::entry_data_offset(&data)?;

        if offset >= data.len() {
            return Ok(None);
        }

        match codec::decode_entry(&data[offset..]) {
            Ok((entry, _)) => Ok(Some(entry.lsn)),
            Err(_) => Ok(None),
        }
    }

    /// Flush the current segment and rotate to a new one.
    fn rotate(&mut self) -> DbResult<()> {
        let closed_segment = self.current_segment;
        // Once we switch away from a segment we must force it durable now;
        // later commit fsyncs only cover the current active segment.
        self.flush_durable()?;
        segment::archive_segment_if_configured(&self.config.dir, closed_segment)?;
        let new_seg = self.current_segment.checked_next().ok_or_else(|| {
            DbError::internal(format!(
                "WAL segment id overflow while rotating from segment {}",
                self.current_segment.get()
            ))
        })?;
        let file = match segment::open_recycled_segment_for_append(
            &self.config.dir,
            new_seg,
            wal_lsn_mode_from_step_mode(self.lsn_step_mode),
        )? {
            Some(file) => file,
            None => segment::open_segment_for_append(
                &self.config.dir,
                new_seg,
                wal_lsn_mode_from_step_mode(self.lsn_step_mode),
            )?,
        };
        self.current_segment = new_seg;
        self.current_file = Self::wrap_current_file(file);
        self.current_size = segment::segment_size(&self.config.dir, new_seg)?;
        let initial_bytes = segment::read_segment_bytes_bounded(
            &self.config.dir,
            new_seg,
            self.current_size,
            "rotate auth init",
        )?;
        self.local_auth_state = LocalWalAuthState::from_existing_segment_bytes(&initial_bytes)?;
        self.local_auth_persisted_len = None;
        Ok(())
    }

    fn recover_failed_append(&mut self, write_error: std::io::Error) -> DbError {
        let path = self.config.dir.join(self.current_segment.filename());
        let recovery_result = (|| -> DbResult<()> {
            let placeholder = Self::wrap_current_file(
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .map_err(|e| DbError::internal(format!("WAL reopen failed: {e}")))?,
            );
            let old_writer = std::mem::replace(&mut self.current_file, placeholder);
            let (_old_file, _buffered) = old_writer.into_parts();

            let trunc_file = std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .map_err(|e| DbError::internal(format!("WAL truncate failed: {e}")))?;
            trunc_file
                .set_len(self.current_size)
                .map_err(|e| DbError::internal(format!("WAL truncate failed: {e}")))?;
            trunc_file
                .sync_all()
                .map_err(|e| DbError::internal(format!("WAL truncate failed: {e}")))?;
            segment::sync_dir(&self.config.dir)?;
            self.current_file = Self::wrap_current_file(segment::open_segment_for_append(
                &self.config.dir,
                self.current_segment,
                wal_lsn_mode_from_step_mode(self.lsn_step_mode),
            )?);
            Ok(())
        })();

        match recovery_result {
            Ok(()) => DbError::internal(format!(
                "WAL write failed and the partial tail was truncated: {write_error}"
            )),
            Err(recovery_error) => DbError::internal(format!(
                "WAL write failed and recovery truncate also failed: {write_error}; {recovery_error}"
            )),
        }
    }

    fn sync_current_file(&self) -> DbResult<()> {
        Self::sync_file_durable(self.current_file.get_ref())
    }

    fn write_encoded_chunk(&mut self, encoded: &[u8]) -> DbResult<()> {
        if encoded.is_empty() {
            return Ok(());
        }

        let encoded_len = u64::try_from(encoded.len())
            .map_err(|_| DbError::internal("WAL: encoded chunk length exceeds u64"))?;
        let next_size = self
            .current_size
            .checked_add(encoded_len)
            .ok_or_else(|| DbError::internal("WAL: current_size overflows u64"))?;
        if let Err(error) = self.current_file.write_all(encoded) {
            return Err(self.recover_failed_append(error));
        }

        #[cfg(test)]
        {
            let injected = FAIL_NEXT_APPEND_AFTER_WRITE.replace(false);
            if injected {
                return Err(self.recover_failed_append(std::io::Error::other(
                    "injected WAL append failure after write",
                )));
            }
        }

        self.current_size = next_size;
        if let Some(auth) = &mut self.local_auth_state {
            auth.update(encoded);
        }
        Ok(())
    }

    fn persist_local_auth_if_needed(&mut self) -> DbResult<()> {
        let Some(auth) = &self.local_auth_state else {
            return Ok(());
        };
        if self.local_auth_persisted_len == Some(self.current_size) {
            return Ok(());
        }
        auth.persist(&self.config.dir, self.current_segment, self.current_size)?;
        self.local_auth_persisted_len = Some(self.current_size);
        Ok(())
    }

    pub fn sync_file_durable(file: &File) -> DbResult<()> {
        #[cfg(test)]
        {
            let injected = FAIL_NEXT_DURABLE_FLUSH.replace(false);
            if injected {
                return Err(DbError::internal(
                    "injected WAL durable flush failure".to_string(),
                ));
            }
        }

        if let Some(hook) = DURABLE_SYNC_HOOK.with(|slot| slot.borrow_mut().take()) {
            let _ = hook.started_tx.send(());
            hook.release_rx.recv().map_err(|e| {
                DbError::internal(format!("durable sync hook release wait failed: {e}"))
            })?;
        }

        match preferred_wal_sync_method() {
            WalSyncMethod::Fdatasync => file
                .sync_data()
                .map_err(|e| DbError::internal(format!("WAL sync failed: {e}"))),
            WalSyncMethod::Fsync => file
                .sync_all()
                .map_err(|e| DbError::internal(format!("WAL sync failed: {e}"))),
            WalSyncMethod::Fullsync => sync_file_fullsync(file),
        }
    }

    fn next_lsn_from(&self, lsn: Lsn, entry_bytes: u64) -> DbResult<Lsn> {
        let step_bytes = match self.lsn_step_mode {
            LsnStepMode::Logical => 1,
            LsnStepMode::ByteOffset => entry_bytes,
        };
        lsn.checked_advance(step_bytes).ok_or_else(|| {
            DbError::internal(format!(
                "WAL LSN overflow while advancing from {} by {}",
                lsn.get(),
                step_bytes
            ))
        })
    }
}

#[cfg(target_vendor = "apple")]
fn sync_file_fullsync(file: &File) -> DbResult<()> {
    use std::os::raw::c_int;

    const F_FULLFSYNC: c_int = 51;

    unsafe extern "C" {
        fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    }

    let fd = file.as_raw_fd();
    // SAFETY: `fd` is borrowed live from a `&File` for the duration of the
    // call, so it is a valid open file descriptor. `F_FULLFSYNC` takes no
    // additional variadic arguments.
    let rc = unsafe { fcntl(fd, F_FULLFSYNC) };
    if rc == -1 {
        return Err(DbError::internal(format!(
            "WAL fullsync failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[cfg(not(target_vendor = "apple"))]
fn sync_file_fullsync(file: &File) -> DbResult<()> {
    file.sync_all()
        .map_err(|e| DbError::internal(format!("WAL fullsync fallback failed: {e}")))
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod tests;
