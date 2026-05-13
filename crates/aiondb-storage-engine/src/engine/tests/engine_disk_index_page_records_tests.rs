use super::*;

#[test]
fn build_disk_index_page_records_emits_root_shrink_leaf_record() {
    let dir = wal_test_dir("build_disk_index_page_records_root_shrink_leaf");
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).expect("create index_pages dir");
    let relation_id = RelationId::new(InMemoryStorage::disk_ordered_index_relation_id(
        IndexId::new(7_305),
    ));
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id.get()));

    let mut meta_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    meta_old[..8].copy_from_slice(b"AIONBTM1");
    meta_old[8..16].copy_from_slice(&(1u64).to_le_bytes());
    meta_old[16..20].copy_from_slice(&(2u32).to_le_bytes());
    meta_old[20..28].copy_from_slice(&(4u64).to_le_bytes());
    meta_old[28..36].copy_from_slice(&(77u64).to_le_bytes());
    let mut root_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut left_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut right_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_internal_page(&mut root_old, 2, &[(20, 3)]);
    write_disk_btree_leaf_page(&mut left_old, 3, &[(10, 1)]);
    write_disk_btree_leaf_page(&mut right_old, u64::MAX, &[(20, 2), (30, 3)]);
    std::fs::write(
        &relation_path,
        [&meta_old[..], &root_old[..], &left_old[..], &right_old[..]].concat(),
    )
    .expect("seed relation file");

    let mut meta_new = meta_old.clone();
    meta_new[8..16].copy_from_slice(&(2u64).to_le_bytes());
    meta_new[16..20].copy_from_slice(&(1u32).to_le_bytes());
    meta_new[20..28].copy_from_slice(&(3u64).to_le_bytes());
    meta_new[28..36].copy_from_slice(&(1u64).to_le_bytes());
    let mut left_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_leaf_page(&mut left_new, u64::MAX, &[(10, 1), (20, 2), (30, 3)]);

    let records = InMemoryStorage::build_disk_index_page_records(
        &index_pages_dir,
        relation_id,
        vec![(0, meta_new), (2, left_new)],
    )
    .expect("build disk index page records");

    assert!(records.iter().any(|record| matches!(
        record,
        WalRecord::DiskBtreeRootShrinkLeaf {
            root_page: 2,
            freed_pages,
            ..
        } if freed_pages == &vec![(1, 77)]
    )));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_disk_index_page_records_emits_root_shrink_internal_record() {
    let dir = wal_test_dir("build_disk_index_page_records_root_shrink_internal");
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).expect("create index_pages dir");
    let relation_id = RelationId::new(InMemoryStorage::disk_ordered_index_relation_id(
        IndexId::new(7_306),
    ));
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id.get()));

    let mut meta_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    meta_old[..8].copy_from_slice(b"AIONBTM1");
    meta_old[8..16].copy_from_slice(&(1u64).to_le_bytes());
    meta_old[16..20].copy_from_slice(&(3u32).to_le_bytes());
    meta_old[20..28].copy_from_slice(&(4u64).to_le_bytes());
    meta_old[28..36].copy_from_slice(&(88u64).to_le_bytes());
    let mut left_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut right_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut root_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_internal_page(&mut left_old, 10, &[(20, 11)]);
    write_disk_btree_internal_page(&mut right_old, 50, &[(60, 13), (70, 14)]);
    write_disk_btree_internal_page(&mut root_old, 2, &[(40, 3)]);
    std::fs::write(
        &relation_path,
        [&meta_old[..], &root_old[..], &left_old[..], &right_old[..]].concat(),
    )
    .expect("seed relation file");

    let mut meta_new = meta_old.clone();
    meta_new[8..16].copy_from_slice(&(2u64).to_le_bytes());
    meta_new[16..20].copy_from_slice(&(2u32).to_le_bytes());
    meta_new[20..28].copy_from_slice(&(3u64).to_le_bytes());
    meta_new[28..36].copy_from_slice(&(1u64).to_le_bytes());
    let mut left_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_internal_page(&mut left_new, 10, &[(20, 11), (40, 50), (60, 13), (70, 14)]);

    let records = InMemoryStorage::build_disk_index_page_records(
        &index_pages_dir,
        relation_id,
        vec![(0, meta_new), (2, left_new)],
    )
    .expect("build disk index page records");

    assert!(records.iter().any(|record| matches!(
        record,
        WalRecord::DiskBtreeRootShrinkInternal {
            root_page: 2,
            freed_pages,
            ..
        } if freed_pages == &vec![(1, 88)]
    )));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_disk_index_page_records_emits_internal_collapse_record() {
    let dir = wal_test_dir("build_disk_index_page_records_internal_collapse");
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).expect("create index_pages dir");
    let relation_id = RelationId::new(InMemoryStorage::disk_ordered_index_relation_id(
        IndexId::new(7_307),
    ));
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id.get()));

    let mut meta_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    meta_old[..8].copy_from_slice(b"AIONBTM1");
    meta_old[8..16].copy_from_slice(&(1u64).to_le_bytes());
    meta_old[16..20].copy_from_slice(&(3u32).to_le_bytes());
    meta_old[20..28].copy_from_slice(&(4u64).to_le_bytes());
    meta_old[28..36].copy_from_slice(&(66u64).to_le_bytes());
    let mut parent_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent_old[..8].copy_from_slice(b"AIONBTB1");
    parent_old[8] = 2;
    parent_old[10..12].copy_from_slice(&(1u16).to_le_bytes());
    parent_old[24..32].copy_from_slice(&(2u64).to_le_bytes());
    parent_old[32..40].copy_from_slice(&(40u64).to_le_bytes());
    parent_old[40..48].copy_from_slice(&(3u64).to_le_bytes());
    let mut collapsed_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    collapsed_old[..8].copy_from_slice(b"AIONBTB1");
    collapsed_old[8] = 2;
    collapsed_old[24..32].copy_from_slice(&(9u64).to_le_bytes());
    let filler = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    std::fs::write(
        &relation_path,
        [
            &meta_old[..],
            &parent_old[..],
            &collapsed_old[..],
            &filler[..],
        ]
        .concat(),
    )
    .expect("seed relation file");

    let mut parent_new = parent_old.clone();
    parent_new[24..32].copy_from_slice(&(9u64).to_le_bytes());
    let mut collapsed_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_leaf_page(&mut collapsed_new, 66, &[]);

    let records = InMemoryStorage::build_disk_index_page_records(
        &index_pages_dir,
        relation_id,
        vec![(1, parent_new), (2, collapsed_new)],
    )
    .expect("build disk index page records");

    assert!(records.iter().any(|record| matches!(
        record,
        WalRecord::DiskBtreeInternalCollapse {
            parent_page: 1,
            parent_slot: 0,
            replacement_child: 9,
            removed_page: 2,
            next_free_page: 66,
            ..
        }
    )));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_disk_index_page_records_emits_root_promote_single_child_record() {
    let dir = wal_test_dir("build_disk_index_page_records_root_promote_single_child");
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).expect("create index_pages dir");
    let relation_id = RelationId::new(InMemoryStorage::disk_ordered_index_relation_id(
        IndexId::new(7_308),
    ));
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id.get()));

    let mut meta_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    meta_old[..8].copy_from_slice(b"AIONBTM1");
    meta_old[8..16].copy_from_slice(&(1u64).to_le_bytes());
    meta_old[16..20].copy_from_slice(&(3u32).to_le_bytes());
    meta_old[20..28].copy_from_slice(&(3u64).to_le_bytes());
    meta_old[28..36].copy_from_slice(&(77u64).to_le_bytes());
    let mut root_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    root_old[..8].copy_from_slice(b"AIONBTB1");
    root_old[8] = 2;
    root_old[24..32].copy_from_slice(&(2u64).to_le_bytes());
    let mut child_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_internal_page(&mut child_old, 10, &[(20, 11)]);
    std::fs::write(
        &relation_path,
        [&meta_old[..], &root_old[..], &child_old[..]].concat(),
    )
    .expect("seed relation file");

    let mut meta_new = meta_old.clone();
    meta_new[8..16].copy_from_slice(&(2u64).to_le_bytes());
    meta_new[16..20].copy_from_slice(&(2u32).to_le_bytes());
    meta_new[20..28].copy_from_slice(&(3u64).to_le_bytes());
    meta_new[28..36].copy_from_slice(&(1u64).to_le_bytes());

    let records = InMemoryStorage::build_disk_index_page_records(
        &index_pages_dir,
        relation_id,
        vec![(0, meta_new)],
    )
    .expect("build disk index page records");

    assert!(records.iter().any(|record| matches!(
        record,
        WalRecord::DiskBtreeRootPromoteSingleChild {
            new_root_page: 2,
            removed_root_page: 1,
            next_free_page: 77,
            ..
        }
    )));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_disk_index_page_records_emits_root_promote_collapsed_chain_record() {
    let dir = wal_test_dir("build_disk_index_page_records_root_promote_collapsed_chain");
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).expect("create index_pages dir");
    let relation_id = RelationId::new(InMemoryStorage::disk_ordered_index_relation_id(
        IndexId::new(7_309),
    ));
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id.get()));

    let mut meta_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    meta_old[..8].copy_from_slice(b"AIONBTM1");
    meta_old[8..16].copy_from_slice(&(1u64).to_le_bytes());
    meta_old[16..20].copy_from_slice(&(4u32).to_le_bytes());
    meta_old[20..28].copy_from_slice(&(4u64).to_le_bytes());
    meta_old[28..36].copy_from_slice(&(77u64).to_le_bytes());
    let mut root_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    root_old[..8].copy_from_slice(b"AIONBTB1");
    root_old[8] = 2;
    root_old[24..32].copy_from_slice(&(2u64).to_le_bytes());
    let mut mid_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    mid_old[..8].copy_from_slice(b"AIONBTB1");
    mid_old[8] = 2;
    mid_old[24..32].copy_from_slice(&(3u64).to_le_bytes());
    let mut child_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_internal_page(&mut child_old, 10, &[(20, 11)]);
    std::fs::write(
        &relation_path,
        [&meta_old[..], &root_old[..], &mid_old[..], &child_old[..]].concat(),
    )
    .expect("seed relation file");

    let mut meta_new = meta_old.clone();
    meta_new[8..16].copy_from_slice(&(3u64).to_le_bytes());
    meta_new[16..20].copy_from_slice(&(3u32).to_le_bytes());
    meta_new[20..28].copy_from_slice(&(4u64).to_le_bytes());
    meta_new[28..36].copy_from_slice(&(1u64).to_le_bytes());

    let records = InMemoryStorage::build_disk_index_page_records(
        &index_pages_dir,
        relation_id,
        vec![(0, meta_new)],
    )
    .expect("build disk index page records");

    assert!(records.iter().any(|record| matches!(
        record,
        WalRecord::DiskBtreeRootPromoteCollapsedChain {
            new_root_page: 3,
            freed_pages,
            ..
        } if freed_pages == &vec![(1, 77), (2, 1)]
    )));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_disk_index_page_records_emits_deep_root_promote_collapsed_chain_record() {
    let dir = wal_test_dir("build_disk_index_page_records_deep_root_promote_collapsed_chain");
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).expect("create index_pages dir");
    let relation_id = RelationId::new(InMemoryStorage::disk_ordered_index_relation_id(
        IndexId::new(7_311),
    ));
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id.get()));

    let mut meta_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    meta_old[..8].copy_from_slice(b"AIONBTM1");
    meta_old[8..16].copy_from_slice(&(1u64).to_le_bytes());
    meta_old[16..20].copy_from_slice(&(5u32).to_le_bytes());
    meta_old[20..28].copy_from_slice(&(5u64).to_le_bytes());
    meta_old[28..36].copy_from_slice(&(77u64).to_le_bytes());
    let mut root_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    root_old[..8].copy_from_slice(b"AIONBTB1");
    root_old[8] = 2;
    root_old[24..32].copy_from_slice(&(2u64).to_le_bytes());
    let mut mid_a_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    mid_a_old[..8].copy_from_slice(b"AIONBTB1");
    mid_a_old[8] = 2;
    mid_a_old[24..32].copy_from_slice(&(3u64).to_le_bytes());
    let mut mid_b_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    mid_b_old[..8].copy_from_slice(b"AIONBTB1");
    mid_b_old[8] = 2;
    mid_b_old[24..32].copy_from_slice(&(4u64).to_le_bytes());
    let mut child_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_internal_page(&mut child_old, 10, &[(20, 11)]);
    std::fs::write(
        &relation_path,
        [
            &meta_old[..],
            &root_old[..],
            &mid_a_old[..],
            &mid_b_old[..],
            &child_old[..],
        ]
        .concat(),
    )
    .expect("seed relation file");

    let mut meta_new = meta_old.clone();
    meta_new[8..16].copy_from_slice(&(4u64).to_le_bytes());
    meta_new[16..20].copy_from_slice(&(2u32).to_le_bytes());
    meta_new[20..28].copy_from_slice(&(5u64).to_le_bytes());
    meta_new[28..36].copy_from_slice(&(1u64).to_le_bytes());

    let records = InMemoryStorage::build_disk_index_page_records(
        &index_pages_dir,
        relation_id,
        vec![(0, meta_new)],
    )
    .expect("build disk index page records");

    assert!(records.iter().any(|record| matches!(
        record,
        WalRecord::DiskBtreeRootPromoteCollapsedChain {
            new_root_page: 4,
            freed_pages,
            ..
        } if freed_pages == &vec![(1, 77), (2, 1), (3, 2)]
    )));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_disk_index_page_records_emits_internal_collapse_chain_record() {
    let dir = wal_test_dir("build_disk_index_page_records_internal_collapse_chain");
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).expect("create index_pages dir");
    let relation_id = RelationId::new(InMemoryStorage::disk_ordered_index_relation_id(
        IndexId::new(7_310),
    ));
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id.get()));

    let mut meta_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    meta_old[..8].copy_from_slice(b"AIONBTM1");
    meta_old[8..16].copy_from_slice(&(1u64).to_le_bytes());
    meta_old[16..20].copy_from_slice(&(4u32).to_le_bytes());
    meta_old[20..28].copy_from_slice(&(4u64).to_le_bytes());
    meta_old[28..36].copy_from_slice(&(55u64).to_le_bytes());
    let mut parent_a_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent_a_old[..8].copy_from_slice(b"AIONBTB1");
    parent_a_old[8] = 2;
    parent_a_old[24..32].copy_from_slice(&(2u64).to_le_bytes());
    let mut parent_b_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent_b_old[..8].copy_from_slice(b"AIONBTB1");
    parent_b_old[8] = 2;
    parent_b_old[24..32].copy_from_slice(&(3u64).to_le_bytes());
    let mut parent_c_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent_c_old[..8].copy_from_slice(b"AIONBTB1");
    parent_c_old[8] = 2;
    parent_c_old[24..32].copy_from_slice(&(11u64).to_le_bytes());
    std::fs::write(
        &relation_path,
        [
            &meta_old[..],
            &parent_a_old[..],
            &parent_b_old[..],
            &parent_c_old[..],
        ]
        .concat(),
    )
    .expect("seed relation file");

    let mut parent_a_new = parent_a_old.clone();
    parent_a_new[24..32].copy_from_slice(&(11u64).to_le_bytes());
    let mut parent_b_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_leaf_page(&mut parent_b_new, 3, &[]);
    let mut parent_c_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_leaf_page(&mut parent_c_new, 55, &[]);

    let records = InMemoryStorage::build_disk_index_page_records(
        &index_pages_dir,
        relation_id,
        vec![(1, parent_a_new), (2, parent_b_new), (3, parent_c_new)],
    )
    .expect("build disk index page records");

    assert!(records.iter().any(|record| matches!(
        record,
        WalRecord::DiskBtreeInternalCollapseChain { steps, .. }
        if steps == &vec![(2, 0, 3, 11, 3, 55), (1, 0, 11, 11, 2, 3)]
    )));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_disk_index_page_records_emits_internal_collapse_chain_record_for_nonleftmost_slot() {
    let dir = wal_test_dir("build_disk_index_page_records_internal_collapse_chain_nonleftmost");
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).expect("create index_pages dir");
    let relation_id = RelationId::new(InMemoryStorage::disk_ordered_index_relation_id(
        IndexId::new(7_312),
    ));
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id.get()));

    let mut meta_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    meta_old[..8].copy_from_slice(b"AIONBTM1");
    meta_old[8..16].copy_from_slice(&(1u64).to_le_bytes());
    meta_old[16..20].copy_from_slice(&(4u32).to_le_bytes());
    meta_old[20..28].copy_from_slice(&(4u64).to_le_bytes());
    meta_old[28..36].copy_from_slice(&(55u64).to_le_bytes());
    let mut parent_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_internal_page(&mut parent_old, 7, &[(40, 2), (80, 9)]);
    let mut parent_b_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent_b_old[..8].copy_from_slice(b"AIONBTB1");
    parent_b_old[8] = 2;
    parent_b_old[24..32].copy_from_slice(&(3u64).to_le_bytes());
    let mut parent_c_old = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent_c_old[..8].copy_from_slice(b"AIONBTB1");
    parent_c_old[8] = 2;
    parent_c_old[24..32].copy_from_slice(&(11u64).to_le_bytes());
    std::fs::write(
        &relation_path,
        [
            &meta_old[..],
            &parent_old[..],
            &parent_b_old[..],
            &parent_c_old[..],
        ]
        .concat(),
    )
    .expect("seed relation file");

    let mut parent_new = parent_old.clone();
    parent_new[40..48].copy_from_slice(&(11u64).to_le_bytes());
    let mut parent_b_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_leaf_page(&mut parent_b_new, 3, &[]);
    let mut parent_c_new = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    write_disk_btree_leaf_page(&mut parent_c_new, 55, &[]);

    let records = InMemoryStorage::build_disk_index_page_records(
        &index_pages_dir,
        relation_id,
        vec![(1, parent_new), (2, parent_b_new), (3, parent_c_new)],
    )
    .expect("build disk index page records");

    assert!(records.iter().any(|record| matches!(
        record,
        WalRecord::DiskBtreeInternalCollapseChain { steps, .. }
        if steps == &vec![(2, 0, 3, 11, 3, 55), (1, 1, 7, 11, 2, 3)]
    )));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn incremental_paged_refresh_can_emit_page_patch_records() {
    let (storage, dir) = storage_with_wal("incremental_paged_refresh_emits_page_patch");
    let table_id = RelationId::new(718);
    create_table(&storage, table_id);

    let bulk_txn = TxnId::new(71_800);
    storage
        .begin_txn(bulk_txn, IsolationLevel::ReadCommitted)
        .expect("begin bulk txn");
    for value in 0..2_000 {
        insert_row(&storage, bulk_txn, table_id, value, "alpha");
    }
    storage.commit_txn(bulk_txn, 1).expect("commit bulk txn");
    storage.checkpoint().expect("checkpoint");

    let patch_txn = TxnId::new(71_801);
    storage
        .begin_txn(patch_txn, IsolationLevel::ReadCommitted)
        .expect("begin patch txn");
    insert_row(&storage, patch_txn, table_id, 50_000, "omega");
    storage.commit_txn(patch_txn, 2).expect("commit patch txn");
    drop(storage);

    let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).expect("open WAL reader");
    let mut saw_table_patch = false;
    while let Some(entry) = reader.next_entry().expect("read WAL entry") {
        match entry.record {
            WalRecord::PagePatch {
                relation_id,
                page_number: _,
                segments,
            } => {
                if relation_id == table_id {
                    assert!(!segments.is_empty());
                    saw_table_patch = true;
                }
            }
            WalRecord::PagePatchBatch {
                relation_id,
                patches,
            } => {
                if relation_id == table_id && !patches.is_empty() {
                    saw_table_patch = true;
                }
            }
            WalRecord::PageSetU64Batch {
                relation_id,
                updates,
            } => {
                if relation_id == table_id && !updates.is_empty() {
                    saw_table_patch = true;
                }
            }
            _ => {}
        }
    }

    assert!(
        saw_table_patch,
        "post-checkpoint small paged-table mutation should be able to emit at least one compact PagePatch record"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn incremental_refresh_splits_large_page_patch_batches() {
    let (storage, dir) = storage_with_wal("incremental_refresh_splits_large_page_patch_batches");
    let table_id = RelationId::new(719);
    create_table(&storage, table_id);
    let bulk_name = "a".repeat(120);
    let patch_name = "b".repeat(120);

    let bulk_txn = TxnId::new(71_900);
    storage
        .begin_txn(bulk_txn, IsolationLevel::ReadCommitted)
        .expect("begin bulk txn");
    let mut tuple_ids = Vec::new();
    for value in 0..12_000 {
        let tuple_id = insert_row(&storage, bulk_txn, table_id, value, &bulk_name);
        if value % 32 == 0 {
            tuple_ids.push((value, tuple_id));
        }
    }
    storage.commit_txn(bulk_txn, 1).expect("commit bulk txn");
    storage.checkpoint().expect("checkpoint");

    let patch_txn = TxnId::new(71_901);
    storage
        .begin_txn(patch_txn, IsolationLevel::ReadCommitted)
        .expect("begin patch txn");
    for (value, tuple_id) in tuple_ids {
        storage
            .update(
                patch_txn,
                table_id,
                tuple_id,
                Row::new(vec![Value::Int(value), Value::Text(patch_name.clone())]),
            )
            .expect("update row");
    }
    storage.commit_txn(patch_txn, 2).expect("commit patch txn");
    drop(storage);

    let mut reader = WalReader::open(dir.clone(), Lsn::new(1)).expect("open WAL reader");
    let mut patch_batch_count = 0usize;
    let mut patched_pages = 0usize;
    while let Some(entry) = reader.next_entry().expect("read WAL entry") {
        match entry.record {
            WalRecord::PagePatchBatch {
                relation_id,
                patches,
            } if relation_id == table_id => {
                patch_batch_count += 1;
                let mut payload_bytes = 0usize;
                for (_, segments) in &patches {
                    payload_bytes += 8 + 4;
                    for (_, data) in segments {
                        payload_bytes += 2 + data.len();
                    }
                    patched_pages += 1;
                }
                assert!(payload_bytes <= 32 * 1024);
            }
            WalRecord::PageSetU64Batch {
                relation_id,
                updates,
            } if relation_id == table_id => {
                patch_batch_count += 1;
                let payload_bytes = updates.len() * (8 + 2 + 8);
                assert!(payload_bytes <= 32 * 1024);
                patched_pages += updates.len();
            }
            _ => {}
        }
    }

    assert!(patch_batch_count > 1);
    assert!(patched_pages > patch_batch_count);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn checkpoint_clears_disk_index_modified_page_history_for_later_commits() {
    let (storage, dir) =
        storage_with_wal_no_paged_mirror("checkpoint_clears_disk_index_modified_history");
    let table_id = RelationId::new(715);
    let index_id = IndexId::new(715);
    create_table(&storage, table_id);
    create_index(&storage, table_id, index_id);
    let relation_id = 0xD15C_0000_0000_0000u64 | (index_id.get() & 0x0000_FFFF_FFFF_FFFF);

    let bulk_txn = TxnId::new(71_500);
    storage
        .begin_txn(bulk_txn, IsolationLevel::ReadCommitted)
        .expect("begin bulk txn");
    for value in 0..2_000 {
        insert_row(&storage, bulk_txn, table_id, value, "alpha");
    }
    storage.commit_txn(bulk_txn, 1).expect("commit bulk txn");
    let modified_before_checkpoint = storage
        .disk_index_pool
        .as_ref()
        .expect("disk index pool")
        .snapshot_modified_relation_pages(relation_id)
        .expect("snapshot modified pages before checkpoint");
    assert!(
        !modified_before_checkpoint.is_empty(),
        "without per-commit paged persistence, disk index page tracking should accumulate modified pages before checkpoint"
    );

    storage.checkpoint().expect("checkpoint");

    let modified_after_checkpoint = storage
        .disk_index_pool
        .as_ref()
        .expect("disk index pool")
        .snapshot_modified_relation_pages(relation_id)
        .expect("snapshot modified pages after checkpoint");
    assert!(
        modified_after_checkpoint.is_empty(),
        "successful checkpoint should clear accumulated disk index modified-page tracking"
    );

    drop(storage);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn live_rows_can_offload_even_while_other_transactions_are_active() {
    let (storage, dir) = storage_with_wal("live_rows_offload_with_active_txn");
    let table_id = RelationId::new(704);
    create_table(&storage, table_id);
    let tuple_id = insert_row(&storage, TxnId::default(), table_id, 1, "alpha");

    let blocker = TxnId::new(9001);
    storage
        .begin_txn(blocker, IsolationLevel::ReadCommitted)
        .unwrap();
    let update_txn = TxnId::new(9002);
    storage
        .begin_txn(update_txn, IsolationLevel::ReadCommitted)
        .unwrap();
    let historical_snapshot = Snapshot::new(blocker, TxnId::new(9003), vec![update_txn]);
    storage
        .update(
            update_txn,
            table_id,
            tuple_id,
            Row::new(vec![Value::Int(2), Value::Text("beta".to_owned())]),
        )
        .unwrap();
    storage.commit_txn(update_txn, 1).unwrap();

    {
        let state = storage.state.read();
        let table = state.tables.get(&table_id).expect("table exists");
        assert!(
            table.is_paged_tuple(tuple_id),
            "committed live row should offload even while another transaction is active"
        );
        assert_eq!(
            table.load_latest_row(&state.overflow, tuple_id).unwrap(),
            None
        );
    }

    assert_eq!(
        storage
            .fetch(
                TxnId::default(),
                &historical_snapshot,
                table_id,
                tuple_id,
                None
            )
            .unwrap(),
        Some(Row::new(vec![
            Value::Int(1),
            Value::Text("alpha".to_owned())
        ]))
    );
    assert_eq!(
        storage
            .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
            .unwrap(),
        Some(Row::new(vec![
            Value::Int(2),
            Value::Text("beta".to_owned())
        ]))
    );

    storage.rollback_txn(blocker).unwrap();
    {
        let state = storage.state.read();
        let table = state.tables.get(&table_id).expect("table exists");
        assert!(table.is_paged_tuple(tuple_id));
        assert_eq!(
            table.load_latest_row(&state.overflow, tuple_id).unwrap(),
            None
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
