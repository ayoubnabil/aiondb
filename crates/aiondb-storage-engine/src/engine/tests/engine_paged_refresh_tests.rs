use super::*;

#[test]
fn failed_paged_refresh_forces_next_commit_to_rebuild_full_paged_state() {
    let (storage, dir) = storage_with_wal("failed_paged_refresh_forces_full_rebuild");
    let table1 = RelationId::new(710);
    let table2 = RelationId::new(711);
    create_table(&storage, table1);
    let tuple1 = insert_row(&storage, TxnId::default(), table1, 1, "old");

    let paged_tables = storage.paged_tables.as_ref().expect("paged tables enabled");
    assert_eq!(
        paged_tables.load_row(table1, tuple1).unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("old".to_owned())]))
    );

    super::paged_tables::inject_publish_current_failure();
    storage
        .update(
            TxnId::default(),
            table1,
            tuple1,
            Row::new(vec![Value::Int(1), Value::Text("new".to_owned())]),
        )
        .expect("autocommit update should remain visible even if paged refresh fails");

    assert_eq!(
        storage
            .fetch(TxnId::default(), &snapshot(), table1, tuple1, None)
            .unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("new".to_owned())]))
    );
    assert_eq!(
        paged_tables.load_row(table1, tuple1).unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("old".to_owned())])),
        "the failed refresh must not have advanced the paged image yet"
    );

    create_table(&storage, table2);

    assert_eq!(
        paged_tables.load_row(table1, tuple1).unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("new".to_owned())])),
        "the next successful refresh must rebuild from the full committed state"
    );
    assert_eq!(
        storage
            .fetch(TxnId::default(), &snapshot(), table1, tuple1, None)
            .unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("new".to_owned())]))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn incremental_paged_refresh_logs_full_page_images() {
    let (storage, dir) = storage_with_wal("incremental_paged_refresh_logs_fpi");
    let table_id = RelationId::new(712);
    create_table(&storage, table_id);

    insert_row(&storage, TxnId::default(), table_id, 1, "alpha");
    insert_row(&storage, TxnId::default(), table_id, 2, "beta");
    drop(storage);

    let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).expect("open WAL reader");
    let mut fpi_count = 0usize;
    while let Some(entry) = reader.next_entry().expect("read WAL entry") {
        match entry.record {
            WalRecord::FullPageImage {
                relation_id,
                page_number: _,
                page_data,
            } => {
                if relation_id == table_id {
                    assert_eq!(page_data.len(), aiondb_buffer_pool::PAGE_SIZE);
                    fpi_count += 1;
                }
            }
            WalRecord::FullPageImageBatch { relation_id, pages } => {
                if relation_id == table_id {
                    for (_, page_data) in pages {
                        assert_eq!(page_data.len(), aiondb_buffer_pool::PAGE_SIZE);
                        fpi_count += 1;
                    }
                }
            }
            WalRecord::PagePatch {
                relation_id,
                page_number: _,
                segments,
            } => {
                if relation_id == table_id {
                    assert!(!segments.is_empty());
                    fpi_count += 1;
                }
            }
            WalRecord::PagePatchBatch {
                relation_id,
                patches,
            } => {
                if relation_id == table_id {
                    for (_, segments) in patches {
                        assert!(!segments.is_empty());
                        fpi_count += 1;
                    }
                }
            }
            WalRecord::PageSetU64Batch {
                relation_id,
                updates,
            } => {
                if relation_id == table_id {
                    fpi_count += updates.len();
                }
            }
            _ => {}
        }
    }

    assert!(
        fpi_count > 0,
        "incremental paged refresh should emit at least one full-page image"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn incremental_refresh_logs_disk_index_full_page_images() {
    let (storage, dir) = storage_with_wal("incremental_refresh_logs_disk_index_fpi");
    let table_id = RelationId::new(713);
    let index_id = IndexId::new(713);
    create_table(&storage, table_id);
    create_index(&storage, table_id, index_id);

    insert_row(&storage, TxnId::default(), table_id, 1, "alpha");
    drop(storage);

    let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).expect("open WAL reader");
    let mut disk_index_fpi_count = 0usize;
    while let Some(entry) = reader.next_entry().expect("read WAL entry") {
        match entry.record {
            WalRecord::FullPageImage {
                relation_id,
                page_number: _,
                page_data,
            } => {
                let raw = relation_id.get();
                let is_disk_index = (raw & 0xFFFF_0000_0000_0000u64) == 0xD15C_0000_0000_0000u64
                    || (raw & 0xFFFF_0000_0000_0000u64) == 0xD15D_0000_0000_0000u64;
                if is_disk_index {
                    assert_eq!(page_data.len(), aiondb_buffer_pool::PAGE_SIZE);
                    disk_index_fpi_count += 1;
                }
            }
            WalRecord::FullPageImageBatch { relation_id, pages } => {
                let raw = relation_id.get();
                let is_disk_index = (raw & 0xFFFF_0000_0000_0000u64) == 0xD15C_0000_0000_0000u64
                    || (raw & 0xFFFF_0000_0000_0000u64) == 0xD15D_0000_0000_0000u64;
                if is_disk_index {
                    for (_, page_data) in pages {
                        assert_eq!(page_data.len(), aiondb_buffer_pool::PAGE_SIZE);
                        disk_index_fpi_count += 1;
                    }
                }
            }
            _ => {}
        }
    }

    assert!(
        disk_index_fpi_count > 0,
        "incremental refresh should emit at least one disk-index full-page image"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn incremental_refresh_logs_only_modified_disk_index_pages_per_commit() {
    let (storage, dir) = storage_with_wal("incremental_refresh_tracks_only_modified_disk_pages");
    let table_id = RelationId::new(714);
    let index_id = IndexId::new(714);
    create_table(&storage, table_id);
    create_index(&storage, table_id, index_id);

    let bulk_txn = TxnId::new(71_400);
    storage
        .begin_txn(bulk_txn, IsolationLevel::ReadCommitted)
        .expect("begin bulk txn");
    for value in 0..2_000 {
        insert_row(&storage, bulk_txn, table_id, value, "alpha");
    }
    storage.commit_txn(bulk_txn, 1).expect("commit bulk txn");

    let small_txn = TxnId::new(71_401);
    storage
        .begin_txn(small_txn, IsolationLevel::ReadCommitted)
        .expect("begin small txn");
    insert_row(&storage, small_txn, table_id, 50_000, "omega");
    storage.commit_txn(small_txn, 2).expect("commit small txn");

    drop(storage);

    let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).expect("open WAL reader");
    let mut after_bulk_commit = false;
    let mut after_small_commit = false;
    let mut bulk_disk_index_fpi_count = 0usize;
    let mut small_disk_index_fpi_count = 0usize;
    while let Some(entry) = reader.next_entry().expect("read WAL entry") {
        match entry.record {
            WalRecord::CommitTxn { txn_id, .. } if txn_id == bulk_txn => {
                after_bulk_commit = true;
            }
            WalRecord::BeginTxn { txn_id, .. } if txn_id == small_txn => {
                after_bulk_commit = false;
            }
            WalRecord::CommitTxn { txn_id, .. } if txn_id == small_txn => {
                after_small_commit = true;
            }
            WalRecord::FullPageImage {
                relation_id,
                page_number: _,
                page_data,
            } => {
                let raw = relation_id.get();
                let is_disk_index = (raw & 0xFFFF_0000_0000_0000u64) == 0xD15C_0000_0000_0000u64
                    || (raw & 0xFFFF_0000_0000_0000u64) == 0xD15D_0000_0000_0000u64;
                if is_disk_index {
                    assert_eq!(page_data.len(), aiondb_buffer_pool::PAGE_SIZE);
                    if after_small_commit {
                        small_disk_index_fpi_count += 1;
                    } else if after_bulk_commit {
                        bulk_disk_index_fpi_count += 1;
                    }
                }
            }
            WalRecord::FullPageImageBatch { relation_id, pages } => {
                let raw = relation_id.get();
                let is_disk_index = (raw & 0xFFFF_0000_0000_0000u64) == 0xD15C_0000_0000_0000u64
                    || (raw & 0xFFFF_0000_0000_0000u64) == 0xD15D_0000_0000_0000u64;
                if is_disk_index {
                    for (_, page_data) in pages {
                        assert_eq!(page_data.len(), aiondb_buffer_pool::PAGE_SIZE);
                        if after_small_commit {
                            small_disk_index_fpi_count += 1;
                        } else if after_bulk_commit {
                            bulk_disk_index_fpi_count += 1;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    assert!(
        bulk_disk_index_fpi_count > 0,
        "bulk commit should emit disk-index full-page images"
    );
    assert!(
        small_disk_index_fpi_count > 0,
        "small commit should still emit some disk-index full-page images"
    );
    assert!(
        small_disk_index_fpi_count < bulk_disk_index_fpi_count,
        "tracked disk-index page logging should emit fewer pages for the later small commit than for the initial bulk build"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn incremental_refresh_splits_large_disk_index_batches() {
    let (storage, dir) = storage_with_wal("incremental_refresh_splits_large_disk_index_batches");
    let table_id = RelationId::new(716);
    let index_id = IndexId::new(716);
    create_table(&storage, table_id);
    create_index(&storage, table_id, index_id);

    let bulk_txn = TxnId::new(71_600);
    storage
        .begin_txn(bulk_txn, IsolationLevel::ReadCommitted)
        .expect("begin bulk txn");
    for value in 0..10_000 {
        insert_row(&storage, bulk_txn, table_id, value, "alpha");
    }
    storage.commit_txn(bulk_txn, 1).expect("commit bulk txn");
    drop(storage);

    let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).expect("open WAL reader");
    let mut batch_count = 0usize;
    let mut batched_page_count = 0usize;
    while let Some(entry) = reader.next_entry().expect("read WAL entry") {
        if let WalRecord::FullPageImageBatch { relation_id, pages } = entry.record {
            let raw = relation_id.get();
            let is_disk_index = (raw & 0xFFFF_0000_0000_0000u64) == 0xD15C_0000_0000_0000u64
                || (raw & 0xFFFF_0000_0000_0000u64) == 0xD15D_0000_0000_0000u64;
            if is_disk_index {
                batch_count += 1;
                let mut payload_bytes = 0usize;
                for (_, page_data) in &pages {
                    assert_eq!(page_data.len(), aiondb_buffer_pool::PAGE_SIZE);
                    payload_bytes += 8 + page_data.len();
                }
                assert!(
                    payload_bytes <= 32 * 1024,
                    "disk-index batch payload should stay under the configured chunk limit"
                );
                batched_page_count += pages.len();
            }
        }
    }

    assert!(
        batch_count > 1,
        "large disk-index refresh should split into multiple batched WAL records"
    );
    assert!(batched_page_count > batch_count);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn incremental_refresh_can_emit_disk_index_page_patch_records() {
    let (storage, dir) = storage_with_wal("incremental_refresh_emits_disk_index_page_patch");
    let table_id = RelationId::new(717);
    let index_id = IndexId::new(717);
    create_table(&storage, table_id);
    create_index(&storage, table_id, index_id);

    let bulk_txn = TxnId::new(71_700);
    storage
        .begin_txn(bulk_txn, IsolationLevel::ReadCommitted)
        .expect("begin bulk txn");
    for value in 0..2_000 {
        insert_row(&storage, bulk_txn, table_id, value, "alpha");
    }
    storage.commit_txn(bulk_txn, 1).expect("commit bulk txn");
    storage.checkpoint().expect("checkpoint");

    let patch_txn = TxnId::new(71_701);
    storage
        .begin_txn(patch_txn, IsolationLevel::ReadCommitted)
        .expect("begin patch txn");
    insert_row(&storage, patch_txn, table_id, 50_000, "omega");
    storage.commit_txn(patch_txn, 2).expect("commit patch txn");
    drop(storage);

    let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).expect("open WAL reader");
    let mut saw_disk_index_patch = false;
    while let Some(entry) = reader.next_entry().expect("read WAL entry") {
        match entry.record {
            WalRecord::PagePatch {
                relation_id,
                page_number: _,
                segments,
            } => {
                let raw = relation_id.get();
                let is_disk_index = (raw & 0xFFFF_0000_0000_0000u64) == 0xD15C_0000_0000_0000u64
                    || (raw & 0xFFFF_0000_0000_0000u64) == 0xD15D_0000_0000_0000u64;
                if is_disk_index {
                    assert!(!segments.is_empty());
                    saw_disk_index_patch = true;
                }
            }
            WalRecord::PagePatchBatch {
                relation_id,
                patches,
            } => {
                let raw = relation_id.get();
                let is_disk_index = (raw & 0xFFFF_0000_0000_0000u64) == 0xD15C_0000_0000_0000u64
                    || (raw & 0xFFFF_0000_0000_0000u64) == 0xD15D_0000_0000_0000u64;
                if is_disk_index && !patches.is_empty() {
                    saw_disk_index_patch = true;
                }
            }
            WalRecord::PageSetU64Batch {
                relation_id,
                updates,
            } => {
                let raw = relation_id.get();
                let is_disk_index = (raw & 0xFFFF_0000_0000_0000u64) == 0xD15C_0000_0000_0000u64
                    || (raw & 0xFFFF_0000_0000_0000u64) == 0xD15D_0000_0000_0000u64;
                if is_disk_index && !updates.is_empty() {
                    saw_disk_index_patch = true;
                }
            }
            WalRecord::DiskBtreeMetaUpdate { relation_id, .. }
            | WalRecord::DiskBtreeLeafInsert { relation_id, .. }
            | WalRecord::DiskBtreeLeafDelete { relation_id, .. } => {
                let raw = relation_id.get();
                let is_disk_index = (raw & 0xFFFF_0000_0000_0000u64) == 0xD15C_0000_0000_0000u64
                    || (raw & 0xFFFF_0000_0000_0000u64) == 0xD15D_0000_0000_0000u64;
                if is_disk_index {
                    saw_disk_index_patch = true;
                }
            }
            _ => {}
        }
    }

    assert!(
        saw_disk_index_patch,
        "post-checkpoint small disk-index mutation should be able to emit at least one compact PagePatch record"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_disk_index_page_records_emits_leaf_redistribute_record() {
    let dir = wal_test_dir("build_disk_index_page_records_leaf_redistribute");
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).expect("create index_pages dir");
    let relation_id = RelationId::new(InMemoryStorage::disk_ordered_index_relation_id(
        IndexId::new(7_301),
    ));
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id.get()));

    let mut parent_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut left_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut right_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_internal_page(&mut parent_old, 1, &[(20, 2)]);
    write_disk_btree_leaf_page(&mut left_old, 2, &[(10, 1)]);
    write_disk_btree_leaf_page(&mut right_old, u64::MAX, &[(20, 2), (30, 3), (40, 4)]);
    std::fs::write(
        &relation_path,
        [&parent_old[..], &left_old[..], &right_old[..]].concat(),
    )
    .expect("seed relation file");

    let mut parent_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut left_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut right_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_internal_page(&mut parent_new, 1, &[(30, 2)]);
    write_disk_btree_leaf_page(&mut left_new, 2, &[(10, 1), (20, 2)]);
    write_disk_btree_leaf_page(&mut right_new, u64::MAX, &[(30, 3), (40, 4)]);

    let records = InMemoryStorage::build_disk_index_page_records(
        &index_pages_dir,
        relation_id,
        vec![(0, parent_new), (1, left_new), (2, right_new)],
    )
    .expect("build disk index page records");

    assert_eq!(records.len(), 1);
    match &records[0] {
        WalRecord::DiskBtreeLeafRedistribute {
            relation_id: record_relation_id,
            left_page,
            right_page,
            parent_page,
            parent_slot,
            parent_first_child,
            left_entries,
            right_entries,
            right_right_sibling,
            new_separator,
        } => {
            assert_eq!(*record_relation_id, relation_id);
            assert_eq!(*left_page, 1);
            assert_eq!(*right_page, 2);
            assert_eq!(*parent_page, 0);
            assert_eq!(*parent_slot, 0);
            assert_eq!(*parent_first_child, 1);
            assert_eq!(left_entries, &vec![(10, 1), (20, 2)]);
            assert_eq!(right_entries, &vec![(30, 3), (40, 4)]);
            assert_eq!(*right_right_sibling, u64::MAX);
            assert_eq!(*new_separator, 30);
        }
        other => panic!("expected DiskBtreeLeafRedistribute, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_disk_index_page_records_emits_internal_redistribute_record() {
    let dir = wal_test_dir("build_disk_index_page_records_internal_redistribute");
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).expect("create index_pages dir");
    let relation_id = RelationId::new(InMemoryStorage::disk_ordered_index_relation_id(
        IndexId::new(7_302),
    ));
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id.get()));

    let mut parent_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut left_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut right_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_internal_page(&mut parent_old, 1, &[(30, 2)]);
    write_disk_btree_internal_page(&mut left_old, 10, &[(20, 11)]);
    write_disk_btree_internal_page(&mut right_old, 30, &[(40, 12), (50, 13), (60, 14)]);
    std::fs::write(
        &relation_path,
        [&parent_old[..], &left_old[..], &right_old[..]].concat(),
    )
    .expect("seed relation file");

    let mut parent_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut left_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut right_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_internal_page(&mut parent_new, 1, &[(40, 2)]);
    write_disk_btree_internal_page(&mut left_new, 10, &[(20, 11), (30, 30)]);
    write_disk_btree_internal_page(&mut right_new, 12, &[(50, 13), (60, 14)]);

    let records = InMemoryStorage::build_disk_index_page_records(
        &index_pages_dir,
        relation_id,
        vec![(0, parent_new), (1, left_new), (2, right_new)],
    )
    .expect("build disk index page records");

    assert_eq!(records.len(), 1);
    match &records[0] {
        WalRecord::DiskBtreeInternalRedistribute {
            relation_id: record_relation_id,
            left_page,
            right_page,
            parent_page,
            parent_slot,
            parent_first_child,
            left_first_child,
            right_first_child,
            left_entries,
            right_entries,
            new_separator,
        } => {
            assert_eq!(*record_relation_id, relation_id);
            assert_eq!(*left_page, 1);
            assert_eq!(*right_page, 2);
            assert_eq!(*parent_page, 0);
            assert_eq!(*parent_slot, 0);
            assert_eq!(*parent_first_child, 1);
            assert_eq!(*left_first_child, 10);
            assert_eq!(*right_first_child, 12);
            assert_eq!(left_entries, &vec![(20, 11), (30, 30)]);
            assert_eq!(right_entries, &vec![(50, 13), (60, 14)]);
            assert_eq!(*new_separator, 40);
        }
        other => panic!("expected DiskBtreeInternalRedistribute, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_disk_index_page_records_emits_leaf_merge_record() {
    let dir = wal_test_dir("build_disk_index_page_records_leaf_merge");
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).expect("create index_pages dir");
    let relation_id = RelationId::new(InMemoryStorage::disk_ordered_index_relation_id(
        IndexId::new(7_303),
    ));
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id.get()));

    let mut meta_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    meta_old[..8].copy_from_slice(b"AIONBTM1");
    meta_old[8..16].copy_from_slice(&(1u64).to_le_bytes());
    meta_old[16..20].copy_from_slice(&(2u32).to_le_bytes());
    meta_old[20..28].copy_from_slice(&(4u64).to_le_bytes());
    meta_old[28..36].copy_from_slice(&(99u64).to_le_bytes());
    let mut parent_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut left_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut right_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_internal_page(&mut parent_old, 2, &[(20, 3)]);
    write_disk_btree_leaf_page(&mut left_old, 3, &[(10, 1)]);
    write_disk_btree_leaf_page(&mut right_old, u64::MAX, &[(20, 2), (30, 3)]);
    std::fs::write(
        &relation_path,
        [
            &meta_old[..],
            &parent_old[..],
            &left_old[..],
            &right_old[..],
        ]
        .concat(),
    )
    .expect("seed relation file");

    let mut meta_new = meta_old.clone();
    meta_new[28..36].copy_from_slice(&(3u64).to_le_bytes());
    let mut parent_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut left_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut right_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_internal_page(&mut parent_new, 2, &[]);
    write_disk_btree_leaf_page(&mut left_new, u64::MAX, &[(10, 1), (20, 2), (30, 3)]);
    write_disk_btree_leaf_page(&mut right_new, 99, &[]);

    let records = InMemoryStorage::build_disk_index_page_records(
        &index_pages_dir,
        relation_id,
        vec![
            (0, meta_new),
            (1, parent_new),
            (2, left_new),
            (3, right_new),
        ],
    )
    .expect("build disk index page records");

    assert!(records.iter().any(|record| matches!(
        record,
        WalRecord::DiskBtreeLeafMerge {
            left_page: 2,
            right_page: 3,
            removed_separator: 20,
            next_free_page: 99,
            ..
        }
    )));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_disk_index_page_records_emits_internal_merge_record() {
    let dir = wal_test_dir("build_disk_index_page_records_internal_merge");
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).expect("create index_pages dir");
    let relation_id = RelationId::new(InMemoryStorage::disk_ordered_index_relation_id(
        IndexId::new(7_304),
    ));
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id.get()));

    let mut meta_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    meta_old[..8].copy_from_slice(b"AIONBTM1");
    meta_old[8..16].copy_from_slice(&(1u64).to_le_bytes());
    meta_old[16..20].copy_from_slice(&(3u32).to_le_bytes());
    meta_old[20..28].copy_from_slice(&(4u64).to_le_bytes());
    meta_old[28..36].copy_from_slice(&(88u64).to_le_bytes());
    let mut parent_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut left_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut right_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_internal_page(&mut parent_old, 2, &[(40, 3)]);
    write_disk_btree_internal_page(&mut left_old, 10, &[(20, 11)]);
    write_disk_btree_internal_page(&mut right_old, 50, &[(60, 13), (70, 14)]);
    std::fs::write(
        &relation_path,
        [
            &meta_old[..],
            &parent_old[..],
            &left_old[..],
            &right_old[..],
        ]
        .concat(),
    )
    .expect("seed relation file");

    let mut meta_new = meta_old.clone();
    meta_new[28..36].copy_from_slice(&(3u64).to_le_bytes());
    let mut parent_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut left_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut right_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_internal_page(&mut parent_new, 2, &[]);
    write_disk_btree_internal_page(&mut left_new, 10, &[(20, 11), (40, 50), (60, 13), (70, 14)]);
    write_disk_btree_leaf_page(&mut right_new, 88, &[]);

    let records = InMemoryStorage::build_disk_index_page_records(
        &index_pages_dir,
        relation_id,
        vec![
            (0, meta_new),
            (1, parent_new),
            (2, left_new),
            (3, right_new),
        ],
    )
    .expect("build disk index page records");

    assert!(records.iter().any(|record| matches!(
        record,
        WalRecord::DiskBtreeInternalMerge {
            left_page: 2,
            right_page: 3,
            removed_separator: 40,
            next_free_page: 88,
            ..
        }
    )));

    let _ = std::fs::remove_dir_all(&dir);
}
