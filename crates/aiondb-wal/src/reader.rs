use std::{path::PathBuf, sync::OnceLock};

use aiondb_core::{convert::usize_to_u64_saturating, DbError, DbResult};
use tracing::warn;

use crate::codec;
use crate::lsn::Lsn;
use crate::record::WalEntry;
use crate::segment::{self, SegmentId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LsnStepMode {
    Logical,
    ByteOffset,
}

const DEFAULT_MAX_SEGMENT_READ_BYTES: u64 = 64 * 1024 * 1024;
const MIN_MAX_SEGMENT_READ_BYTES: u64 = 1024 * 1024;
const MAX_MAX_SEGMENT_READ_BYTES: u64 = segment::WAL_SEGMENT_SCAN_HARD_LIMIT_BYTES;

fn parse_max_segment_read_bytes(value: Option<&str>) -> u64 {
    value
        .and_then(|raw| raw.parse::<u64>().ok())
        .map_or(DEFAULT_MAX_SEGMENT_READ_BYTES, |bytes| {
            bytes.clamp(MIN_MAX_SEGMENT_READ_BYTES, MAX_MAX_SEGMENT_READ_BYTES)
        })
}

fn max_segment_read_bytes() -> u64 {
    static MAX_SEGMENT_READ_BYTES: OnceLock<u64> = OnceLock::new();
    *MAX_SEGMENT_READ_BYTES.get_or_init(|| {
        parse_max_segment_read_bytes(
            std::env::var("AIONDB_WAL_MAX_SEGMENT_READ_BYTES")
                .ok()
                .as_deref(),
        )
    })
}

/// Iterator over WAL entries across all segments.
///
/// Reads entries sequentially from the first segment onward, skipping any
/// entries whose LSN is less than the configured `start_lsn`.
pub struct WalReader {
    dir: PathBuf,
    segments: Vec<SegmentId>,
    segment_index: usize,
    current_segment: Option<SegmentId>,
    current_data: Vec<u8>,
    offset: usize,
    start_lsn: Lsn,
    last_seen_lsn: Option<Lsn>,
    last_entry_bytes: Option<u64>,
    lsn_step_mode: Option<LsnStepMode>,
}

impl WalReader {
    /// Create a reader that iterates all entries with `lsn >= start_lsn`.
    pub fn open(dir: PathBuf, start_lsn: Lsn) -> DbResult<Self> {
        let mut segments = segment::list_segments(&dir)?;
        if segment::restore_from_archive_enabled() {
            if let Some(archive_dir) = segment::archive_dir_from_env() {
                let archive_segments = segment::list_segments_if_exists(&archive_dir)?;
                segments.extend(archive_segments);
                segments.sort();
                segments.dedup();
            }
        }
        Ok(Self {
            dir,
            segments,
            segment_index: 0,
            current_segment: None,
            current_data: Vec::new(),
            offset: 0,
            start_lsn,
            last_seen_lsn: None,
            last_entry_bytes: None,
            lsn_step_mode: None,
        })
    }

    /// Read the next WAL entry. Returns `None` when all entries have been
    /// exhausted.
    pub fn next_entry(&mut self) -> DbResult<Option<WalEntry>> {
        self.next_entry_with_len()
            .map(|entry| entry.map(|(entry, _)| entry))
    }

    /// Read the next WAL entry and its encoded on-disk byte length.
    ///
    /// The returned size is the exact number of bytes consumed from the WAL
    /// stream for this entry (including length header and checksum footer).
    pub fn next_entry_with_len(&mut self) -> DbResult<Option<(WalEntry, u64)>> {
        loop {
            // Try to decode from the current buffer.
            if self.offset < self.current_data.len() {
                match codec::decode_entry(&self.current_data[self.offset..]) {
                    Ok((entry, consumed)) => {
                        self.offset += consumed;
                        let consumed_u64 = usize_to_u64_saturating(consumed);
                        if let Some(previous_lsn) = self.last_seen_lsn {
                            if entry.prev_lsn != Lsn::ZERO && entry.prev_lsn != previous_lsn {
                                return Err(DbError::internal(format!(
                                    "WAL backward-chain mismatch while reading {}: expected prev_lsn {}, found {}",
                                    self.dir.display(),
                                    previous_lsn.get(),
                                    entry.prev_lsn.get()
                                )));
                            }

                            let previous_entry_bytes = self.last_entry_bytes.unwrap_or(1);
                            let logical_expected =
                                previous_lsn.checked_advance(1).ok_or_else(|| {
                                    DbError::internal(format!(
                                        "WAL LSN overflow while reading {} after {}",
                                        self.dir.display(),
                                        previous_lsn.get()
                                    ))
                                })?;
                            let byte_expected = previous_lsn
                                .checked_advance(previous_entry_bytes)
                                .ok_or_else(|| {
                                    DbError::internal(format!(
                                        "WAL LSN overflow while reading {} after {}",
                                        self.dir.display(),
                                        previous_lsn.get()
                                    ))
                                })?;

                            match self.lsn_step_mode {
                                Some(LsnStepMode::Logical) => {
                                    if entry.lsn != logical_expected {
                                        return Err(DbError::internal(format!(
                                            "WAL gap detected while reading {}: expected logical LSN {}, found {}",
                                            self.dir.display(),
                                            logical_expected.get(),
                                            entry.lsn.get()
                                        )));
                                    }
                                }
                                Some(LsnStepMode::ByteOffset) => {
                                    if entry.lsn != byte_expected {
                                        return Err(DbError::internal(format!(
                                            "WAL gap detected while reading {}: expected byte-offset LSN {}, found {}",
                                            self.dir.display(),
                                            byte_expected.get(),
                                            entry.lsn.get()
                                        )));
                                    }
                                }
                                None => {
                                    if entry.lsn == logical_expected {
                                        self.lsn_step_mode = Some(LsnStepMode::Logical);
                                    } else if entry.lsn == byte_expected {
                                        self.lsn_step_mode = Some(LsnStepMode::ByteOffset);
                                    } else {
                                        return Err(DbError::internal(format!(
                                            "WAL gap detected while reading {}: expected LSN {} (logical) or {} (byte-offset), found {}",
                                            self.dir.display(),
                                            logical_expected.get(),
                                            byte_expected.get(),
                                            entry.lsn.get()
                                        )));
                                    }
                                }
                            }
                        }
                        self.last_seen_lsn = Some(entry.lsn);
                        self.last_entry_bytes = Some(consumed_u64);
                        if entry.lsn >= self.start_lsn {
                            return Ok(Some((entry, consumed_u64)));
                        }
                        // Skip entries before start_lsn.
                        continue;
                    }
                    Err(e) => {
                        let segment_id = self.current_segment.ok_or_else(|| {
                            DbError::internal(
                                "WAL reader encountered corrupt data without an active segment",
                            )
                        })?;
                        if self.current_segment_is_last() {
                            // Only the active tail of the newest segment may be
                            // partially written after a crash. Archived segments
                            // must be fully valid; otherwise recovery/replication
                            warn!(
                                segment_id = segment_id.get(),
                                offset = self.offset,
                                "stopping at corrupt/partial WAL tail in last segment: {e}"
                            );
                            self.offset = self.current_data.len();
                        } else {
                            return Err(DbError::internal(format!(
                                "WAL corruption in archived segment {} at offset {}: {e}",
                                segment_id.get(),
                                self.offset
                            )));
                        }
                    }
                }
            }

            // Load the next segment.
            if !self.load_next_segment()? {
                return Ok(None);
            }
        }
    }

    /// Load the next segment file into the internal buffer.
    fn load_next_segment(&mut self) -> DbResult<bool> {
        if self.segment_index >= self.segments.len() {
            return Ok(false);
        }

        let seg_id = self.segments[self.segment_index];
        self.segment_index += 1;
        self.current_segment = Some(seg_id);

        let segment_path = self.dir.join(seg_id.filename());
        if !segment_path.exists() {
            let restored = segment::restore_segment_if_configured(&self.dir, seg_id)?;
            if restored {
                warn!(
                    segment_id = seg_id.get(),
                    path = %segment_path.display(),
                    "restored missing WAL segment from archive"
                );
            }
        }

        let max_segment_bytes = max_segment_read_bytes();
        self.current_data = segment::read_segment_bytes_bounded(
            &self.dir,
            seg_id,
            max_segment_bytes,
            "reader segment load",
        )?;
        let verification = segment::verify_local_segment_integrity_if_configured(
            &self.dir,
            seg_id,
            self.current_segment_is_last(),
            &self.current_data,
        )?;
        if verification.truncated_unauthenticated_tail {
            let trusted = usize::try_from(verification.trusted_len).map_err(|_| {
                DbError::internal("WAL trusted length exceeds usize while loading segment")
            })?;
            self.current_data.truncate(trusted);
        }

        self.offset = segment::entry_data_offset(&self.current_data)?;

        Ok(true)
    }

    fn current_segment_is_last(&self) -> bool {
        self.current_segment
            .zip(self.segments.last().copied())
            .is_some_and(|(current, last)| current == last)
    }

    /// Collect all remaining entries into a `Vec`.
    pub fn collect_all(&mut self) -> DbResult<Vec<WalEntry>> {
        let mut entries = Vec::new();
        while let Some(entry) = self.next_entry()? {
            entries.push(entry);
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;

    use super::*;
    use crate::record::WalRecord;
    use crate::segment;
    use crate::writer::WalWriter;
    use crate::WalConfig;

    fn test_config(dir: PathBuf) -> WalConfig {
        WalConfig {
            dir,
            segment_max_bytes: 16 * 1024 * 1024,
            sync_on_flush: false,
            group_commit_delay_micros: 0,
            wal_compression: crate::WalCompression::None,
            wal_lsn_mode: crate::WalLsnMode::Logical,
        }
    }

    fn sample_record() -> WalRecord {
        WalRecord::Checkpoint {
            last_committed_lsn: Lsn::new(0),
        }
    }

    #[test]
    fn reader_empty_wal_returns_none() {
        let dir = segment::test_dir("reader_empty");
        segment::ensure_wal_dir(&dir).unwrap();

        let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
        assert!(reader.next_entry().unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_accepts_headerless_segment() {
        let dir = segment::test_dir("reader_headerless");
        std::fs::create_dir_all(&dir).unwrap();

        let entry = WalEntry {
            lsn: Lsn::new(1),
            prev_lsn: Lsn::ZERO,
            database_id: WalEntry::LEGACY_DATABASE_ID,
            record: sample_record(),
        };
        let encoded = codec::encode_entry(&entry).unwrap();
        let mut file = std::fs::File::create(dir.join(SegmentId::new(1).filename())).unwrap();
        file.write_all(&encoded).unwrap();
        file.flush().unwrap();
        drop(file);

        let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
        let entries = reader.collect_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].lsn, Lsn::new(1));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_reads_all_entries() {
        let dir = segment::test_dir("reader_reads_all");
        let config = test_config(dir.clone());

        let mut writer = WalWriter::open(config).unwrap();
        for _ in 0..10 {
            writer.append(&sample_record()).unwrap();
        }
        writer.flush().unwrap();
        drop(writer);

        let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
        let entries = reader.collect_all().unwrap();
        assert_eq!(entries.len(), 10);

        // Verify LSNs are sequential
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.lsn, Lsn::new(i as u64 + 1));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_starts_from_lsn() {
        let dir = segment::test_dir("reader_starts_from");
        let config = test_config(dir.clone());

        let mut writer = WalWriter::open(config).unwrap();
        for _ in 0..10 {
            writer.append(&sample_record()).unwrap();
        }
        writer.flush().unwrap();
        drop(writer);

        // Read starting from LSN 5
        let mut reader = WalReader::open(dir.clone(), Lsn::new(5)).unwrap();
        let entries = reader.collect_all().unwrap();
        assert_eq!(entries.len(), 6); // LSNs 5, 6, 7, 8, 9, 10

        assert_eq!(entries[0].lsn, Lsn::new(5));
        assert_eq!(entries[5].lsn, Lsn::new(10));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_spans_multiple_segments() {
        let dir = segment::test_dir("reader_multi_seg");
        let config = WalConfig {
            dir: dir.clone(),
            segment_max_bytes: 100, // Very small to force rotation
            sync_on_flush: false,
            group_commit_delay_micros: 0,
            wal_compression: crate::WalCompression::None,
            wal_lsn_mode: crate::WalLsnMode::Logical,
        };

        let mut writer = WalWriter::open(config).unwrap();
        for _ in 0..20 {
            writer.append(&sample_record()).unwrap();
        }
        writer.flush().unwrap();

        // Verify we have multiple segments
        let segments = segment::list_segments(&dir).unwrap();
        assert!(
            segments.len() > 1,
            "Expected multiple segments, got {}",
            segments.len()
        );
        drop(writer);

        // Read all entries back
        let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
        let entries = reader.collect_all().unwrap();
        assert_eq!(entries.len(), 20);

        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.lsn, Lsn::new(i as u64 + 1));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Recovery must be idempotent: opening the same WAL twice from the same
    /// start LSN must yield the same entry sequence, byte for byte.
    ///
    /// This is the v0.2 recovery contract. A replayer that re-runs after a
    /// crash mid-replay must see exactly what it saw the first time.
    #[test]
    fn reader_replay_is_idempotent_within_a_single_run() {
        use crate::record::WalRecord;
        use crate::Lsn;
        use aiondb_core::{RelationId, Row, TupleId, TxnId, Value};

        let dir = segment::test_dir("reader_idempotent_single");
        let config = test_config(dir.clone());

        let records: Vec<WalRecord> = (1..=8u64)
            .map(|i| WalRecord::AutocommitInsertRow {
                txn_id: TxnId::new(i),
                table_id: RelationId::new(7),
                tuple_id: TupleId::new(i),
                row: Row::new(vec![Value::Int(i as i32)]),
            })
            .collect();

        let mut writer = WalWriter::open(config).unwrap();
        for record in &records {
            writer.append(record).unwrap();
        }
        writer.flush().unwrap();
        drop(writer);

        let mut first = WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
        let first_pass = first.collect_all().unwrap();

        let mut second = WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
        let second_pass = second.collect_all().unwrap();

        assert_eq!(first_pass.len(), records.len(), "entry count drift");
        assert_eq!(first_pass, second_pass, "replay must be deterministic");

        // And replaying from a checkpoint mid-stream must still match the
        // suffix of the full replay.
        let mut suffix = WalReader::open(dir.clone(), Lsn::new(4)).unwrap();
        let suffix_pass = suffix.collect_all().unwrap();
        assert_eq!(suffix_pass, first_pass[3..].to_vec());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Recovery must also be idempotent across reopens: after closing the
    /// reader and reopening it on the same on-disk state, the second open
    /// must replay the same entries (no progress is silently consumed).
    #[test]
    fn reader_replay_is_idempotent_across_reopen() {
        use crate::record::WalRecord;
        use crate::Lsn;
        use aiondb_core::{RelationId, TupleId, TxnId};

        let dir = segment::test_dir("reader_idempotent_reopen");
        let config = test_config(dir.clone());

        let mut writer = WalWriter::open(config).unwrap();
        for i in 1..=5u64 {
            writer
                .append(&WalRecord::AutocommitDeleteRow {
                    txn_id: TxnId::new(i),
                    table_id: RelationId::new(11),
                    tuple_id: TupleId::new(i),
                })
                .unwrap();
        }
        writer.flush().unwrap();
        drop(writer);

        let first_pass: Vec<_> = {
            let mut r = WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
            r.collect_all().unwrap()
        };
        let second_pass: Vec<_> = {
            let mut r = WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
            r.collect_all().unwrap()
        };
        assert_eq!(first_pass, second_pass);
        assert_eq!(first_pass.len(), 5);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_collect_all() {
        let dir = segment::test_dir("reader_collect");
        let config = test_config(dir.clone());

        let mut writer = WalWriter::open(config).unwrap();
        for _ in 0..7 {
            writer.append(&sample_record()).unwrap();
        }
        writer.flush().unwrap();
        drop(writer);

        let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
        let entries = reader.collect_all().unwrap();
        assert_eq!(entries.len(), 7);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_handles_empty_segment() {
        let dir = segment::test_dir("reader_empty_seg");
        segment::ensure_wal_dir(&dir).unwrap();

        // Create an empty segment file
        let seg_id = segment::SegmentId::new(1);
        let _file =
            segment::open_segment_for_append(&dir, seg_id, crate::WalLsnMode::Logical).unwrap();

        let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
        assert!(reader.next_entry().unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_rejects_oversized_segment_before_decode() {
        let dir = segment::test_dir("reader_oversized_segment");
        std::fs::create_dir_all(&dir).unwrap();

        let seg_id = SegmentId::new(1);
        let path = dir.join(seg_id.filename());
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(segment::WAL_SEGMENT_SCAN_HARD_LIMIT_BYTES + 1)
            .unwrap();
        drop(file);

        let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
        let err = reader
            .next_entry()
            .expect_err("oversized segment must fail before decode");
        assert!(err.to_string().contains("exceeds safety limit"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_errors_on_corruption_in_archived_segment() {
        let dir = segment::test_dir("reader_archived_corruption");
        let config = WalConfig {
            dir: dir.clone(),
            segment_max_bytes: 100,
            sync_on_flush: false,
            group_commit_delay_micros: 0,
            wal_compression: crate::WalCompression::None,
            wal_lsn_mode: crate::WalLsnMode::Logical,
        };

        let mut writer = WalWriter::open(config).unwrap();
        for _ in 0..20 {
            writer.append(&sample_record()).unwrap();
        }
        writer.flush().unwrap();
        drop(writer);

        let segments = segment::list_segments(&dir).unwrap();
        assert!(
            segments.len() > 1,
            "expected multiple segments so the first one is archived"
        );

        let archived_seg = segments[0];
        let archived_path = dir.join(archived_seg.filename());
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&archived_path)
            .unwrap();
        use std::io::{Read, Seek, SeekFrom};
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        let corrupt_offset = bytes.len() / 2;
        file.seek(SeekFrom::Start(corrupt_offset as u64)).unwrap();
        file.write_all(&[bytes[corrupt_offset] ^ 0xFF]).unwrap();
        file.flush().unwrap();
        drop(file);

        let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
        let err = reader
            .collect_all()
            .expect_err("archived-segment corruption must not be skipped");
        assert!(err
            .to_string()
            .contains("WAL corruption in archived segment"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_errors_on_gap_between_segments() {
        let dir = segment::test_dir("reader_gap_between_segments");
        let config = WalConfig {
            dir: dir.clone(),
            segment_max_bytes: 100,
            sync_on_flush: false,
            group_commit_delay_micros: 0,
            wal_compression: crate::WalCompression::None,
            wal_lsn_mode: crate::WalLsnMode::Logical,
        };

        let mut writer = WalWriter::open(config).unwrap();
        for _ in 0..20 {
            writer.append(&sample_record()).unwrap();
        }
        writer.flush().unwrap();
        drop(writer);

        let segments = segment::list_segments(&dir).unwrap();
        assert!(
            segments.len() > 2,
            "expected at least three segments to remove one from the middle"
        );

        let missing_segment = segments[1];
        std::fs::remove_file(dir.join(missing_segment.filename())).unwrap();

        let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
        let err = reader
            .collect_all()
            .expect_err("reader must reject mid-history WAL gaps");
        let message = err.to_string();
        assert!(
            message.contains("WAL gap detected") || message.contains("backward-chain mismatch"),
            "unexpected error: {message}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_stops_at_corrupt_tail_in_last_segment() {
        let dir = segment::test_dir("reader_last_segment_tail_corruption");
        let config = test_config(dir.clone());

        let mut writer = WalWriter::open(config).unwrap();
        for _ in 0..5 {
            writer.append(&sample_record()).unwrap();
        }
        writer.flush().unwrap();
        drop(writer);

        let last_seg = *segment::list_segments(&dir).unwrap().last().unwrap();
        let mut file =
            segment::open_segment_for_append(&dir, last_seg, crate::WalLsnMode::Logical).unwrap();
        file.write_all(&[0xAA, 0xBB, 0xCC]).unwrap();
        file.flush().unwrap();
        drop(file);

        let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
        let entries = reader.collect_all().unwrap();
        assert_eq!(entries.len(), 5);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_rejects_backward_chain_mismatch() {
        let dir = segment::test_dir("reader_backward_chain_mismatch");
        std::fs::create_dir_all(&dir).unwrap();

        let entry1 = WalEntry {
            lsn: Lsn::new(1),
            prev_lsn: Lsn::ZERO,
            database_id: WalEntry::LEGACY_DATABASE_ID,
            record: sample_record(),
        };
        let entry2 = WalEntry {
            lsn: Lsn::new(2),
            prev_lsn: Lsn::new(999),
            database_id: WalEntry::LEGACY_DATABASE_ID,
            record: sample_record(),
        };

        let mut file = std::fs::File::create(dir.join(SegmentId::new(1).filename())).unwrap();
        file.write_all(&codec::encode_entry(&entry1).unwrap())
            .unwrap();
        file.write_all(
            &codec::encode_entry_with_compression(&entry2, crate::WalCompression::None).unwrap(),
        )
        .unwrap();
        file.flush().unwrap();
        drop(file);

        let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
        let err = reader
            .collect_all()
            .expect_err("reader must reject mismatched xl_prev chain");
        assert!(err.to_string().contains("backward-chain mismatch"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
