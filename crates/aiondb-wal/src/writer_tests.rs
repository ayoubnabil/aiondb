use std::io::Write;
use std::path::PathBuf;

use aiondb_core::checksum::compute_crc32c;
use aiondb_core::RelationId;

use super::*;

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

struct LocalWalAuthGuard;

impl LocalWalAuthGuard {
    fn enable() -> Self {
        segment::set_test_local_hmac_key_override(Some(vec![b'k'; 32]));
        Self
    }
}

impl Drop for LocalWalAuthGuard {
    fn drop(&mut self) {
        segment::clear_test_local_hmac_key_override();
    }
}

#[test]
fn wal_sync_method_defaults_to_fdatasync() {
    assert_eq!(parse_wal_sync_method(None), WalSyncMethod::Fdatasync);
    assert_eq!(
        parse_wal_sync_method(Some("not-a-method")),
        WalSyncMethod::Fdatasync
    );
}

#[test]
fn wal_sync_method_parses_supported_values() {
    assert_eq!(
        parse_wal_sync_method(Some("fdatasync")),
        WalSyncMethod::Fdatasync
    );
    assert_eq!(parse_wal_sync_method(Some("fsync")), WalSyncMethod::Fsync);
    assert_eq!(
        parse_wal_sync_method(Some("sync_all")),
        WalSyncMethod::Fsync
    );
    assert_eq!(
        parse_wal_sync_method(Some("fullsync")),
        WalSyncMethod::Fullsync
    );
    assert_eq!(
        parse_wal_sync_method(Some("FULL_FSYNC")),
        WalSyncMethod::Fullsync
    );
}

fn sample_record() -> WalRecord {
    WalRecord::Checkpoint {
        last_committed_lsn: Lsn::new(0),
    }
}

#[test]
fn writer_creates_wal_directory() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_creates_dir");
    assert!(!dir.exists());

    let _writer = WalWriter::open(test_config(dir.clone())).unwrap();
    assert!(dir.is_dir());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_appends_and_reads_back() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_appends");
    let config = test_config(dir.clone());

    let mut writer = WalWriter::open(config).unwrap();
    let lsn = writer.append(&sample_record()).unwrap();
    writer.flush().unwrap();

    assert_eq!(lsn, Lsn::new(1));

    // Read the file back and decode
    let mut file = segment::open_segment_for_read(&dir, SegmentId::new(1)).unwrap();
    let mut data = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut data).unwrap();
    assert!(!data.is_empty());

    let entry_offset = segment::entry_data_offset(&data).unwrap();
    let (entry, consumed) = codec::decode_entry(&data[entry_offset..]).unwrap();
    assert_eq!(entry.lsn, Lsn::new(1));
    assert_eq!(entry_offset + consumed, data.len());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_open_rejects_locally_tampered_segment_with_recomputed_crc() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let _auth = LocalWalAuthGuard::enable();
    let dir = segment::test_dir("writer_rejects_tampered_authenticated_wal");
    let config = test_config(dir.clone());

    let mut writer = WalWriter::open(config.clone()).unwrap();
    writer.append(&sample_record()).unwrap();
    writer.flush_durable().unwrap();
    drop(writer);

    let path = dir.join(SegmentId::new(1).filename());
    let mut bytes = std::fs::read(&path).unwrap();
    let entry_offset = segment::entry_data_offset(&bytes).unwrap();
    let (_entry, consumed) = codec::decode_entry(&bytes[entry_offset..]).unwrap();
    let entry_start = entry_offset;
    let entry_end = entry_offset + consumed;
    let payload_last_byte = entry_end - 5;
    bytes[payload_last_byte] ^= 0x01;
    let checksum_start = entry_start + 4;
    let checksum_end = entry_end - 4;
    let checksum = compute_crc32c(&bytes[checksum_start..checksum_end]);
    bytes[checksum_end..entry_end].copy_from_slice(&checksum.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    let err = WalWriter::open(config)
        .err()
        .expect("tampered authenticated WAL must be rejected");
    assert!(
        err.to_string()
            .contains("WAL local integrity verification failed"),
        "{err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_append_prepared_batch_appends_multiple_records() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_prepared_batch");
    let config = test_config(dir.clone());
    let mut writer = WalWriter::open(config).unwrap();
    let prepared = vec![
        codec::prepare_record_with_compression(&sample_record(), crate::WalCompression::None)
            .unwrap(),
        codec::prepare_record_with_compression(&sample_record(), crate::WalCompression::None)
            .unwrap(),
    ];

    let result = writer.append_prepared_batch(&prepared).unwrap().unwrap();
    writer.flush().unwrap();

    assert_eq!(result.last_lsn, Lsn::new(2));
    assert!(result.total_bytes > 0);
    assert_eq!(writer.next_lsn(), Lsn::new(3));

    let mut file = segment::open_segment_for_read(&dir, SegmentId::new(1)).unwrap();
    let mut data = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut data).unwrap();
    let entry_offset = segment::entry_data_offset(&data).unwrap();
    let (entry1, consumed1) = codec::decode_entry(&data[entry_offset..]).unwrap();
    let (entry2, consumed2) = codec::decode_entry(&data[entry_offset + consumed1..]).unwrap();
    assert_eq!(entry1.lsn, Lsn::new(1));
    assert_eq!(entry2.lsn, Lsn::new(2));
    assert_eq!(entry_offset + consumed1 + consumed2, data.len());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn write_encoded_chunk_rejects_size_overflow_before_write() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_chunk_size_overflow");
    let config = test_config(dir.clone());
    let mut writer = WalWriter::open(config).unwrap();
    let size_before = segment::segment_size(&dir, SegmentId::new(1)).unwrap();
    writer.current_size = u64::MAX;

    let err = writer.write_encoded_chunk(&[0xAA]).unwrap_err();
    assert!(err.to_string().contains("current_size overflows"));
    writer.flush().unwrap();

    assert_eq!(
        segment::segment_size(&dir, SegmentId::new(1)).unwrap(),
        size_before
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_append_prepared_batch_rotates_when_segment_fills() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_prepared_batch_rotate");
    let mut config = test_config(dir.clone());
    config.segment_max_bytes = 512;
    let mut writer = WalWriter::open(config).unwrap();
    let prepared = vec![
        codec::prepare_record_with_compression(
            &WalRecord::FullPageImage {
                relation_id: RelationId::new(1),
                page_number: 0,
                page_data: vec![0xAB; 220],
            },
            crate::WalCompression::None,
        )
        .unwrap(),
        codec::prepare_record_with_compression(
            &WalRecord::FullPageImage {
                relation_id: RelationId::new(1),
                page_number: 1,
                page_data: vec![0xBC; 220],
            },
            crate::WalCompression::None,
        )
        .unwrap(),
        codec::prepare_record_with_compression(
            &WalRecord::FullPageImage {
                relation_id: RelationId::new(1),
                page_number: 2,
                page_data: vec![0xCD; 220],
            },
            crate::WalCompression::None,
        )
        .unwrap(),
    ];

    let result = writer.append_prepared_batch(&prepared).unwrap().unwrap();
    writer.flush().unwrap();

    assert_eq!(result.last_lsn, Lsn::new(3));
    assert_eq!(writer.next_lsn(), Lsn::new(4));
    let segments = segment::list_segments(&dir).unwrap();
    assert!(segments.len() >= 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_reopens_after_rotating_prepared_batch_and_preserves_chain() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_prepared_batch_reopen_chain");
    let mut config = test_config(dir.clone());
    config.segment_max_bytes = 512;

    {
        let mut writer = WalWriter::open(config.clone()).unwrap();
        let prepared = vec![
            codec::prepare_record_with_compression(
                &WalRecord::FullPageImage {
                    relation_id: RelationId::new(9),
                    page_number: 0,
                    page_data: vec![0x11; 220],
                },
                crate::WalCompression::None,
            )
            .unwrap(),
            codec::prepare_record_with_compression(
                &WalRecord::FullPageImage {
                    relation_id: RelationId::new(9),
                    page_number: 1,
                    page_data: vec![0x22; 220],
                },
                crate::WalCompression::None,
            )
            .unwrap(),
            codec::prepare_record_with_compression(
                &WalRecord::FullPageImage {
                    relation_id: RelationId::new(9),
                    page_number: 2,
                    page_data: vec![0x33; 220],
                },
                crate::WalCompression::None,
            )
            .unwrap(),
        ];

        let result = writer.append_prepared_batch(&prepared).unwrap().unwrap();
        assert_eq!(result.last_lsn, Lsn::new(3));
        assert!(writer.current_segment().get() > 1);
        writer.flush().unwrap();
    }

    let mut writer = WalWriter::open(config).unwrap();
    assert_eq!(writer.next_lsn(), Lsn::new(4));
    assert_eq!(writer.last_lsn(), Some(Lsn::new(3)));

    let lsn4 = writer.append(&sample_record()).unwrap();
    assert_eq!(lsn4, Lsn::new(4));
    writer.flush().unwrap();
    drop(writer);

    let mut reader = crate::reader::WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
    let entries = reader.collect_all().unwrap();
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].lsn, Lsn::new(1));
    assert_eq!(entries[0].prev_lsn, Lsn::ZERO);
    assert_eq!(entries[1].lsn, Lsn::new(2));
    assert_eq!(entries[1].prev_lsn, Lsn::new(1));
    assert_eq!(entries[2].lsn, Lsn::new(3));
    assert_eq!(entries[2].prev_lsn, Lsn::new(2));
    assert_eq!(entries[3].lsn, Lsn::new(4));
    assert_eq!(entries[3].prev_lsn, Lsn::new(3));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_open_accepts_headerless_segment() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_headerless");
    std::fs::create_dir_all(&dir).unwrap();

    let seg_path = dir.join(SegmentId::new(1).filename());
    let mut file = std::fs::File::create(&seg_path).unwrap();
    let entry = WalEntry {
        lsn: Lsn::new(1),
        prev_lsn: Lsn::ZERO,
        database_id: WalEntry::LEGACY_DATABASE_ID,
        record: sample_record(),
    };
    let encoded = codec::encode_entry(&entry).unwrap();
    file.write_all(&encoded).unwrap();
    file.flush().unwrap();
    drop(file);

    let writer = WalWriter::open(test_config(dir.clone())).unwrap();
    assert_eq!(writer.next_lsn(), Lsn::new(2));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_open_rejects_lsn_mode_mismatch_from_segment_header() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_header_mode_mismatch");
    let mut config = test_config(dir.clone());
    {
        let mut writer = WalWriter::open(config.clone()).unwrap();
        writer.append(&sample_record()).unwrap();
        writer.flush().unwrap();
    }

    config.wal_lsn_mode = crate::WalLsnMode::ByteOffset;
    let err = match WalWriter::open(config) {
        Ok(_) => panic!("header mode mismatch must fail"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("WAL LSN mode mismatch"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_open_rejects_oversized_segment_before_scan() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_oversized_segment");
    std::fs::create_dir_all(&dir).unwrap();

    let seg_id = SegmentId::new(1);
    let path = dir.join(seg_id.filename());
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(segment::WAL_SEGMENT_SCAN_HARD_LIMIT_BYTES + 1)
        .unwrap();
    drop(file);

    let Err(err) = WalWriter::open(test_config(dir.clone())) else {
        panic!("writer startup must reject oversized WAL segment")
    };
    assert!(err.to_string().contains("exceeds safety limit"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_emits_backward_chain_for_entries_after_first() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_backward_chain");
    let config = test_config(dir.clone());

    let mut writer = WalWriter::open(config).unwrap();
    let first_lsn = writer.append(&sample_record()).unwrap();
    let second_lsn = writer.append(&sample_record()).unwrap();
    writer.flush().unwrap();
    drop(writer);

    let mut reader = crate::reader::WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
    let first = reader.next_entry().unwrap().unwrap();
    let second = reader.next_entry().unwrap().unwrap();

    assert_eq!(first.lsn, first_lsn);
    assert_eq!(second.lsn, second_lsn);
    assert_eq!(second.prev_lsn, first_lsn);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_open_detects_byte_offset_lsn_progression() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_byte_offset_progression");
    std::fs::create_dir_all(&dir).unwrap();

    let entry1 = WalEntry {
        lsn: Lsn::new(1),
        prev_lsn: Lsn::ZERO,
        database_id: WalEntry::LEGACY_DATABASE_ID,
        record: sample_record(),
    };
    let encoded1 = codec::encode_entry(&entry1).unwrap();

    let entry2_lsn = entry1
        .lsn
        .advance(u64::try_from(encoded1.len()).unwrap_or(u64::MAX));
    let entry2 = WalEntry {
        lsn: entry2_lsn,
        prev_lsn: entry1.lsn,
        database_id: WalEntry::LEGACY_DATABASE_ID,
        record: sample_record(),
    };
    let encoded2 = codec::encode_entry(&entry2).unwrap();

    let seg_path = dir.join(SegmentId::new(1).filename());
    let mut file = std::fs::File::create(&seg_path).unwrap();
    file.write_all(&encoded1).unwrap();
    file.write_all(&encoded2).unwrap();
    file.flush().unwrap();
    drop(file);

    let mut writer = WalWriter::open(test_config(dir.clone())).unwrap();
    let expected_next_lsn = entry2_lsn.advance(u64::try_from(encoded2.len()).unwrap_or(u64::MAX));
    assert_eq!(writer.next_lsn(), expected_next_lsn);
    assert_eq!(writer.last_lsn(), Some(entry2_lsn));

    let appended_lsn = writer.append(&sample_record()).unwrap();
    assert_eq!(appended_lsn, expected_next_lsn);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_rotates_segment_when_full() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_rotates");
    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 100, // Very small to force rotation
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: crate::WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    };

    let mut writer = WalWriter::open(config).unwrap();
    assert_eq!(writer.current_segment(), SegmentId::new(1));

    // Write entries until rotation occurs
    let mut last_segment = writer.current_segment();
    for _ in 0..20 {
        writer.append(&sample_record()).unwrap();
        if writer.current_segment() != last_segment {
            break;
        }
        last_segment = writer.current_segment();
    }

    // Should have rotated at least once
    assert!(
        writer.current_segment().get() > 1,
        "Expected rotation to occur"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_resumes_from_existing_segments() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_resumes");
    let config = test_config(dir.clone());

    // Write some entries, then drop the writer
    let next_lsn;
    {
        let mut writer = WalWriter::open(config.clone()).unwrap();
        for _ in 0..5 {
            writer.append(&sample_record()).unwrap();
        }
        writer.flush().unwrap();
        next_lsn = writer.next_lsn();
    }

    // Re-open and verify we resume from the correct LSN
    let writer2 = WalWriter::open(config).unwrap();
    assert_eq!(writer2.next_lsn(), next_lsn);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_flush_syncs_data() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_flush_sync");
    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 16 * 1024 * 1024,
        sync_on_flush: true,
        group_commit_delay_micros: 0,
        wal_compression: crate::WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    };

    let mut writer = WalWriter::open(config).unwrap();
    writer.append(&sample_record()).unwrap();
    // This should not panic even with sync_on_flush = true
    writer.flush().unwrap();

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_durable_flush_syncs_even_when_config_disables_sync_on_flush() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_durable_flush_forces_sync");
    let config = test_config(dir.clone());

    let mut writer = WalWriter::open(config).unwrap();
    writer.append(&sample_record()).unwrap();
    FAIL_NEXT_DURABLE_FLUSH.set(true);
    let err = writer
        .flush_durable()
        .expect_err("durable flush must still sync with sync_on_flush=false");
    assert!(err
        .to_string()
        .contains("injected WAL durable flush failure"));

    writer.flush_durable().unwrap();

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_assigns_sequential_lsns() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_sequential_lsns");
    let config = test_config(dir.clone());

    let mut writer = WalWriter::open(config).unwrap();
    let lsn1 = writer.append(&sample_record()).unwrap();
    let lsn2 = writer.append(&sample_record()).unwrap();
    let lsn3 = writer.append(&sample_record()).unwrap();

    assert_eq!(lsn1, Lsn::new(1));
    assert_eq!(lsn2, Lsn::new(2));
    assert_eq!(lsn3, Lsn::new(3));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_recovers_lsn_from_previous_segment_when_last_is_empty() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_recover_empty_last");
    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 100, // Small to force rotation
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: crate::WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    };

    // Write enough entries to rotate at least once.
    let last_lsn;
    {
        let mut writer = WalWriter::open(config.clone()).unwrap();
        let mut lsn = Lsn::ZERO;
        for _ in 0..20 {
            lsn = writer.append(&sample_record()).unwrap();
        }
        writer.flush().unwrap();
        last_lsn = lsn;

        // Simulate a crash after rotation by creating an empty next segment.
        let empty_seg = writer.current_segment().next();
        segment::open_segment_for_append(&dir, empty_seg, crate::WalLsnMode::Logical).unwrap();
    }

    // Re-open - must recover the correct next LSN even though the last
    // segment file is empty.
    let writer2 = WalWriter::open(config).unwrap();
    assert_eq!(writer2.next_lsn(), last_lsn.advance(1));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_truncates_corrupt_tail_on_recovery() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_truncate_corrupt");
    let config = test_config(dir.clone());

    // Write some entries.
    {
        let mut writer = WalWriter::open(config.clone()).unwrap();
        for _ in 0..5 {
            writer.append(&sample_record()).unwrap();
        }
        writer.flush().unwrap();
    }

    // Append garbage bytes to the segment file to simulate a partial write.
    {
        let seg_id = segment::SegmentId::new(1);
        let mut file =
            segment::open_segment_for_append(&dir, seg_id, crate::WalLsnMode::Logical).unwrap();
        file.write_all(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x00])
            .unwrap();
        file.flush().unwrap();
    }

    // Re-open - should truncate the corrupt tail and resume correctly.
    let mut writer2 = WalWriter::open(config.clone()).unwrap();
    assert_eq!(writer2.next_lsn(), Lsn::new(6));
    assert_eq!(
        writer2.current_size,
        segment::segment_size(&dir, SegmentId::new(1)).unwrap()
    );

    // New entries should be readable.
    let lsn6 = writer2.append(&sample_record()).unwrap();
    writer2.flush().unwrap();
    assert_eq!(lsn6, Lsn::new(6));
    drop(writer2);

    // Verify all 6 entries are readable.
    let mut reader = crate::reader::WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
    let entries = reader.collect_all().unwrap();
    assert_eq!(entries.len(), 6);
    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(entry.lsn, Lsn::new(i as u64 + 1));
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_truncate_requires_directory_sync_after_recovery_truncate() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_truncate_requires_dir_sync");
    let config = test_config(dir.clone());

    {
        let mut writer = WalWriter::open(config.clone()).unwrap();
        for _ in 0..5 {
            writer.append(&sample_record()).unwrap();
        }
        writer.flush().unwrap();
    }

    {
        let seg_id = segment::SegmentId::new(1);
        let mut file =
            segment::open_segment_for_append(&dir, seg_id, crate::WalLsnMode::Logical).unwrap();
        file.write_all(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x00])
            .unwrap();
        file.flush().unwrap();
    }

    // `WalWriter::open` syncs the WAL dir and its parent during
    // `ensure_wal_dir`; the next sync is the one that persists the
    // truncation metadata.
    segment::inject_dir_sync_failure_after(2);
    let Err(err) = WalWriter::open(config) else {
        panic!("recovery truncate must fail if the WAL directory cannot be synced");
    };
    assert!(err.to_string().contains("syncing WAL directory"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_open_rejects_corruption_in_archived_segment() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_rejects_archived_corruption");
    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 100,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: crate::WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    };

    {
        let mut writer = WalWriter::open(config.clone()).unwrap();
        for _ in 0..20 {
            writer.append(&sample_record()).unwrap();
        }
        writer.flush().unwrap();

        // Simulate a crash right after rotating to a fresh segment so the
        // previously active segment becomes archived.
        let empty_seg = writer.current_segment().next();
        segment::open_segment_for_append(&dir, empty_seg, crate::WalLsnMode::Logical).unwrap();
    }

    let archived_seg = segment::list_segments(&dir)
        .unwrap()
        .iter()
        .rev()
        .nth(1)
        .copied()
        .expect("expected archived segment before the empty tail segment");
    let mut file =
        segment::open_segment_for_append(&dir, archived_seg, crate::WalLsnMode::Logical).unwrap();
    file.write_all(&[0xDE, 0xAD, 0xBE, 0xEF]).unwrap();
    file.flush().unwrap();
    drop(file);

    let Err(err) = WalWriter::open(config) else {
        panic!("startup must fail if an archived segment contains trailing corruption")
    };
    assert!(err
        .to_string()
        .contains("WAL corruption in archived segment"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remove_segments_before_cleans_old_segments() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("remove_segs_before");
    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 100, // Small to force many rotations
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: crate::WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    };

    let mut writer = WalWriter::open(config).unwrap();

    // Write enough entries to create multiple segments.
    let mut lsns = Vec::new();
    for _ in 0..30 {
        lsns.push(writer.append(&sample_record()).unwrap());
    }
    writer.flush().unwrap();

    let segments_before = segment::list_segments(&dir).unwrap();
    assert!(
        segments_before.len() >= 3,
        "Expected at least 3 segments, got {}",
        segments_before.len()
    );

    // Pick a cutoff LSN somewhere in the middle.
    let cutoff = lsns[15]; // LSN 16

    let removed = writer.remove_segments_before(cutoff).unwrap();
    assert!(removed > 0, "Expected some segments to be removed");

    let segments_after = segment::list_segments(&dir).unwrap();
    assert!(segments_after.len() < segments_before.len());

    // The current/active segment must still exist.
    assert!(segments_after.contains(&writer.current_segment()));

    // All remaining entries from cutoff onward should still be readable.
    drop(writer);
    let mut reader = crate::reader::WalReader::open(dir.clone(), cutoff).unwrap();
    let entries = reader.collect_all().unwrap();
    assert!(!entries.is_empty());
    assert!(entries[0].lsn >= cutoff);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remove_segments_before_does_not_remove_active_segment() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("remove_segs_active");
    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 16 * 1024 * 1024, // Large - everything in one segment
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: crate::WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    };

    let mut writer = WalWriter::open(config).unwrap();
    for _ in 0..5 {
        writer.append(&sample_record()).unwrap();
    }
    writer.flush().unwrap();

    // Only one segment, which is the active one - nothing should be removed.
    let removed = writer.remove_segments_before(Lsn::new(100)).unwrap();
    assert_eq!(removed, 0);

    let segments = segment::list_segments(&dir).unwrap();
    assert_eq!(segments.len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remove_segments_before_lsn_zero_removes_nothing() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("remove_segs_zero");
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

    let segments_before = segment::list_segments(&dir).unwrap();

    // LSN 0 means nothing is before the cutoff.
    let removed = writer.remove_segments_before(Lsn::ZERO).unwrap();
    assert_eq!(removed, 0);

    let segments_after = segment::list_segments(&dir).unwrap();
    assert_eq!(segments_before.len(), segments_after.len());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remove_segments_preserves_data_integrity() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("remove_segs_integrity");
    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 100,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: crate::WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    };

    let mut writer = WalWriter::open(config).unwrap();
    let mut all_lsns = Vec::new();
    for _ in 0..40 {
        all_lsns.push(writer.append(&sample_record()).unwrap());
    }
    writer.flush().unwrap();

    // Use a cutoff near the end.
    let cutoff = all_lsns[30]; // LSN 31
    let removed = writer.remove_segments_before(cutoff).unwrap();
    assert!(removed > 0);

    // Write more entries after cleanup to verify writer still works.
    let post_cleanup_lsn = writer.append(&sample_record()).unwrap();
    writer.flush().unwrap();
    assert_eq!(post_cleanup_lsn, Lsn::new(41));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remove_segments_before_keeps_configured_newest_segments() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("remove_segs_keep_floor");
    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 100,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: crate::WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    };

    let mut writer = WalWriter::open(config).unwrap();
    for _ in 0..40 {
        writer.append(&sample_record()).unwrap();
    }
    writer.flush().unwrap();

    let segments_before = segment::list_segments(&dir).unwrap();
    assert!(
        segments_before.len() >= 3,
        "expected multiple segments before cleanup, got {}",
        segments_before.len()
    );

    let removed = writer
        .remove_segments_before_with_min_segments(Lsn::new(u64::MAX), 2)
        .unwrap();
    assert!(removed > 0, "cleanup should remove some archived segments");

    let segments_after = segment::list_segments(&dir).unwrap();
    assert_eq!(
        segments_after.len(),
        segments_before.len().min(2),
        "cleanup must retain the configured newest segment floor"
    );
    assert_eq!(
        segments_after,
        segments_before[segments_before.len() - segments_after.len()..].to_vec(),
        "cleanup must retain the newest segments, not arbitrary survivors"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_truncates_failed_append_before_retrying() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_failed_append_truncates_tail");
    let config = test_config(dir.clone());

    let mut writer = WalWriter::open(config.clone()).unwrap();
    writer.append(&sample_record()).unwrap();
    writer.flush().unwrap();

    FAIL_NEXT_APPEND_AFTER_WRITE.set(true);
    let err = writer
        .append(&sample_record())
        .expect_err("injected append failure must surface");
    assert!(err.to_string().contains("partial tail was truncated"));
    assert_eq!(writer.next_lsn(), Lsn::new(2));

    let retry_lsn = writer.append(&sample_record()).unwrap();
    writer.flush().unwrap();
    assert_eq!(retry_lsn, Lsn::new(2));

    let mut reader = crate::reader::WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
    let entries = reader.collect_all().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].lsn, Lsn::new(1));
    assert_eq!(entries[1].lsn, Lsn::new(2));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_truncates_failed_append_batch_before_retrying() {
    FAIL_NEXT_DURABLE_FLUSH.set(false);
    FAIL_NEXT_APPEND_AFTER_WRITE.set(false);
    let dir = segment::test_dir("writer_failed_append_batch_truncates_tail");
    let config = test_config(dir.clone());

    let mut writer = WalWriter::open(config.clone()).unwrap();
    writer.append(&sample_record()).unwrap();
    writer.flush().unwrap();

    let prepared = vec![
        codec::prepare_record_with_compression(&sample_record(), crate::WalCompression::None)
            .unwrap(),
        codec::prepare_record_with_compression(&sample_record(), crate::WalCompression::None)
            .unwrap(),
    ];

    FAIL_NEXT_APPEND_AFTER_WRITE.set(true);
    let err = writer
        .append_prepared_batch(&prepared)
        .expect_err("injected append batch failure must surface");
    assert!(err.to_string().contains("partial tail was truncated"));
    assert_eq!(writer.next_lsn(), Lsn::new(2));

    let retry = writer.append_prepared_batch(&prepared).unwrap().unwrap();
    writer.flush().unwrap();
    assert_eq!(retry.last_lsn, Lsn::new(3));

    let mut reader = crate::reader::WalReader::open(dir.clone(), Lsn::new(1)).unwrap();
    let entries = reader.collect_all().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].lsn, Lsn::new(1));
    assert_eq!(entries[1].lsn, Lsn::new(2));
    assert_eq!(entries[2].lsn, Lsn::new(3));

    let _ = std::fs::remove_dir_all(&dir);
}
