use super::*;

#[test]
fn recover_replays_disk_btree_meta_update() {
    use aiondb_storage_api::StorageTxnParticipant;

    let dir = test_dir("recover_disk_btree_meta_update");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let index_id = IndexId::new(996);
    let index_desc = IndexStorageDescriptor {
        index_id,
        table_id: desc.table_id,
        unique: false,
        nulls_not_distinct: false,
        gin: false,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        }],
        include_columns: vec![],
        hnsw_options: None,
    };

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        storage
            .create_table_storage(TxnId::default(), &desc)
            .expect("table should be created");
        for value in 0..16 {
            storage
                .insert(
                    TxnId::default(),
                    desc.table_id,
                    Row::new(vec![Value::Int(value), Value::Text("meta-update".into())]),
                )
                .expect("row should be inserted");
        }
        storage
            .create_index_storage(TxnId::default(), &index_desc)
            .expect("index should be created");
        storage
            .checkpoint()
            .expect("checkpoint should materialize index");
    }

    let relation_id = RelationId::new(disk_ordered_relation_id(index_id));
    let mut writer = WalWriter::open(config.clone()).unwrap();
    writer
        .append(&WalRecord::DiskBtreeMetaUpdate {
            relation_id,
            root_page: 1,
            height: 9,
            page_count: 123,
            free_list_head: 77,
        })
        .unwrap();
    writer.flush().unwrap();
    drop(writer);

    let (recovered, _) = InMemoryStorage::open_with_recovery(config).unwrap();
    let disk_indexes = recovered.disk_ordered_indexes.read();
    let stats = disk_indexes
        .get(&index_id)
        .expect("disk ordered index should reopen after recovery")
        .stats()
        .expect("disk ordered index stats after recovery");
    assert_eq!(stats.height, 9);
    assert_eq!(stats.page_count, 123);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_leaf_insert() {
    use aiondb_storage_api::StorageTxnParticipant;

    let dir = test_dir("recover_disk_btree_leaf_insert");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let index_id = IndexId::new(997);
    let index_desc = IndexStorageDescriptor {
        index_id,
        table_id: desc.table_id,
        unique: false,
        nulls_not_distinct: false,
        gin: false,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        }],
        include_columns: vec![],
        hnsw_options: None,
    };

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        storage
            .create_table_storage(TxnId::default(), &desc)
            .expect("table should be created");
        for value in 0..5 {
            storage
                .insert(
                    TxnId::default(),
                    desc.table_id,
                    Row::new(vec![Value::Int(value), Value::Text("leaf-insert".into())]),
                )
                .expect("row should be inserted");
        }
        storage
            .create_index_storage(TxnId::default(), &index_desc)
            .expect("index should be created");
        storage
            .checkpoint()
            .expect("checkpoint should materialize index");
    }

    let before_entries = read_disk_leaf_entries(&dir, index_id, 1);
    let inserted_key = before_entries.last().unwrap().0.saturating_add(1);
    let relation_id = RelationId::new(disk_ordered_relation_id(index_id));
    crate::engine::recovery::apply_disk_btree_leaf_insert(
        &dir.join("index_pages"),
        relation_id,
        1,
        inserted_key,
        999,
    )
    .unwrap();
    let after_entries = read_disk_leaf_entries(&dir, index_id, 1);
    assert!(after_entries.contains(&(inserted_key, 999)));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_leaf_delete() {
    use aiondb_storage_api::StorageTxnParticipant;

    let dir = test_dir("recover_disk_btree_leaf_delete");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let index_id = IndexId::new(998);
    let index_desc = IndexStorageDescriptor {
        index_id,
        table_id: desc.table_id,
        unique: false,
        nulls_not_distinct: false,
        gin: false,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        }],
        include_columns: vec![],
        hnsw_options: None,
    };

    let removed_entry = {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        storage
            .create_table_storage(TxnId::default(), &desc)
            .expect("table should be created");
        for value in 0..5 {
            storage
                .insert(
                    TxnId::default(),
                    desc.table_id,
                    Row::new(vec![Value::Int(value), Value::Text("leaf-delete".into())]),
                )
                .expect("row should be inserted");
        }
        storage
            .create_index_storage(TxnId::default(), &index_desc)
            .expect("index should be created");
        storage
            .checkpoint()
            .expect("checkpoint should materialize index");
        read_disk_leaf_entries(&dir, index_id, 1)
            .into_iter()
            .find(|(_, value)| *value > 0)
            .expect("leaf should contain at least one entry")
    };

    let relation_id = RelationId::new(disk_ordered_relation_id(index_id));
    crate::engine::recovery::apply_disk_btree_leaf_delete(
        &dir.join("index_pages"),
        relation_id,
        1,
        removed_entry.0,
        removed_entry.1,
    )
    .unwrap();
    let after_entries = read_disk_leaf_entries(&dir, index_id, 1);
    assert!(!after_entries.contains(&removed_entry));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_leaf_split() {
    use aiondb_storage_api::StorageTxnParticipant;

    let dir = test_dir("recover_disk_btree_leaf_split");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let index_id = IndexId::new(999);
    let index_desc = IndexStorageDescriptor {
        index_id,
        table_id: desc.table_id,
        unique: false,
        nulls_not_distinct: false,
        gin: false,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        }],
        include_columns: vec![],
        hnsw_options: None,
    };

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        storage
            .create_table_storage(TxnId::default(), &desc)
            .expect("table should be created");
        for value in 0..4 {
            storage
                .insert(
                    TxnId::default(),
                    desc.table_id,
                    Row::new(vec![Value::Int(value), Value::Text("leaf-split".into())]),
                )
                .expect("row should be inserted");
        }
        storage
            .create_index_storage(TxnId::default(), &index_desc)
            .expect("index should be created");
        storage
            .checkpoint()
            .expect("checkpoint should materialize index");
    }

    let before_entries = read_disk_leaf_entries(&dir, index_id, 1);
    let left_entries = before_entries[..2].to_vec();
    let right_entries = before_entries[2..].to_vec();
    let relation_id = RelationId::new(disk_ordered_relation_id(index_id));
    crate::engine::recovery::apply_disk_btree_leaf_split(
        &dir.join("index_pages"),
        relation_id,
        1,
        2,
        u64::MAX,
        &left_entries,
        &right_entries,
    )
    .unwrap();

    let left_after = read_disk_leaf_entries(&dir, index_id, 1);
    let right_after = read_disk_leaf_entries(&dir, index_id, 2);
    assert_eq!(left_after, left_entries);
    assert_eq!(right_after, right_entries);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_internal_insert() {
    let dir = test_dir("recover_disk_btree_internal_insert");
    let index_id = IndexId::new(1000);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id));
    let mut page = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    page[..8].copy_from_slice(b"AIONBTB1");
    page[8] = 2;
    page[10..12].copy_from_slice(&(1u16).to_le_bytes());
    page[24..32].copy_from_slice(&(1u64).to_le_bytes());
    page[32..40].copy_from_slice(&(10u64).to_le_bytes());
    page[40..48].copy_from_slice(&(2u64).to_le_bytes());
    std::fs::write(&relation_path, &page).unwrap();

    let (_first_child, before_entries) = read_disk_internal_entries(&dir, index_id, 0);
    let inserted_separator = before_entries.last().unwrap().0.saturating_add(1);
    crate::engine::recovery::apply_disk_btree_internal_insert(
        &index_pages_dir,
        RelationId::new(relation_id),
        0,
        inserted_separator,
        999,
    )
    .unwrap();

    let (_first_child_after, after_entries) = read_disk_internal_entries(&dir, index_id, 0);
    assert!(after_entries.contains(&(inserted_separator, 999)));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_internal_split() {
    let dir = test_dir("recover_disk_btree_internal_split");
    let index_id = IndexId::new(1001);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id));
    let mut left = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    left[..8].copy_from_slice(b"AIONBTB1");
    left[8] = 2;
    left[10..12].copy_from_slice(&(5u16).to_le_bytes());
    left[24..32].copy_from_slice(&(1u64).to_le_bytes());
    for (idx, (k, v)) in [(20u64, 2u64), (30, 3), (40, 4), (50, 5), (60, 6)]
        .into_iter()
        .enumerate()
    {
        let offset = 32 + idx * 16;
        left[offset..offset + 8].copy_from_slice(&k.to_le_bytes());
        left[offset + 8..offset + 16].copy_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&relation_path, &left).unwrap();

    crate::engine::recovery::apply_disk_btree_internal_split(
        &index_pages_dir,
        RelationId::new(relation_id),
        0,
        1,
        1,
        4,
        &[(20, 2), (30, 3)],
        &[(50, 5), (60, 6)],
    )
    .unwrap();

    let (left_first_child, left_entries) = read_disk_internal_entries(&dir, index_id, 0);
    let (right_first_child, right_entries) = read_disk_internal_entries(&dir, index_id, 1);
    assert_eq!(left_first_child, 1);
    assert_eq!(left_entries, vec![(20, 2), (30, 3)]);
    assert_eq!(right_first_child, 4);
    assert_eq!(right_entries, vec![(50, 5), (60, 6)]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_root_grow() {
    let dir = test_dir("recover_disk_btree_root_grow");
    let index_id = IndexId::new(1002);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);

    crate::engine::recovery::apply_disk_btree_root_grow(
        &index_pages_dir,
        RelationId::new(relation_id),
        3,
        1,
        40,
        5,
    )
    .unwrap();

    let (first_child, entries) = read_disk_internal_entries(&dir, index_id, 3);
    assert_eq!(first_child, 1);
    assert_eq!(entries, vec![(40, 5)]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_internal_delete() {
    let dir = test_dir("recover_disk_btree_internal_delete");
    let index_id = IndexId::new(1003);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id));
    let mut page = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    page[..8].copy_from_slice(b"AIONBTB1");
    page[8] = 2;
    page[10..12].copy_from_slice(&(3u16).to_le_bytes());
    page[24..32].copy_from_slice(&(1u64).to_le_bytes());
    for (idx, (k, v)) in [(20u64, 2u64), (40, 4), (60, 6)].into_iter().enumerate() {
        let offset = 32 + idx * 16;
        page[offset..offset + 8].copy_from_slice(&k.to_le_bytes());
        page[offset + 8..offset + 16].copy_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&relation_path, &page).unwrap();

    crate::engine::recovery::apply_disk_btree_internal_delete(
        &index_pages_dir,
        RelationId::new(relation_id),
        0,
        40,
        4,
    )
    .unwrap();

    let (first_child, entries) = read_disk_internal_entries(&dir, index_id, 0);
    assert_eq!(first_child, 1);
    assert_eq!(entries, vec![(20, 2), (60, 6)]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_leaf_redistribute() {
    let dir = test_dir("recover_disk_btree_leaf_redistribute");
    let index_id = IndexId::new(1004);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id));
    let mut parent = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent[..8].copy_from_slice(b"AIONBTB1");
    parent[8] = 2;
    parent[10..12].copy_from_slice(&(1u16).to_le_bytes());
    parent[24..32].copy_from_slice(&(1u64).to_le_bytes());
    parent[32..40].copy_from_slice(&(30u64).to_le_bytes());
    parent[40..48].copy_from_slice(&(2u64).to_le_bytes());
    let mut left = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    left[..8].copy_from_slice(b"AIONBTB1");
    left[8] = 1;
    left[10..12].copy_from_slice(&(1u16).to_le_bytes());
    left[16..24].copy_from_slice(&(2u64).to_le_bytes());
    left[32..40].copy_from_slice(&(10u64).to_le_bytes());
    left[40..48].copy_from_slice(&(1u64).to_le_bytes());
    let mut right = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    right[..8].copy_from_slice(b"AIONBTB1");
    right[8] = 1;
    right[10..12].copy_from_slice(&(3u16).to_le_bytes());
    right[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    for (idx, (k, v)) in [(20u64, 2u64), (30, 3), (40, 4)].into_iter().enumerate() {
        let offset = 32 + idx * 16;
        right[offset..offset + 8].copy_from_slice(&k.to_le_bytes());
        right[offset + 8..offset + 16].copy_from_slice(&v.to_le_bytes());
    }
    std::fs::write(
        &relation_path,
        [&parent[..], &left[..], &right[..]].concat(),
    )
    .unwrap();

    crate::engine::recovery::apply_disk_btree_leaf_redistribute(
        &index_pages_dir,
        RelationId::new(relation_id),
        1,
        2,
        0,
        0,
        1,
        &[(10, 1), (20, 2)],
        &[(30, 3), (40, 4)],
        u64::MAX,
        30,
    )
    .unwrap();

    let parent_entries = read_disk_internal_entries(&dir, index_id, 0).1;
    let left_entries = read_disk_leaf_entries(&dir, index_id, 1);
    let right_entries = read_disk_leaf_entries(&dir, index_id, 2);
    assert_eq!(parent_entries, vec![(30, 2)]);
    assert_eq!(left_entries, vec![(10, 1), (20, 2)]);
    assert_eq!(right_entries, vec![(30, 3), (40, 4)]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_internal_redistribute() {
    let dir = test_dir("recover_disk_btree_internal_redistribute");
    let index_id = IndexId::new(1005);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id));
    let mut parent = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent[..8].copy_from_slice(b"AIONBTB1");
    parent[8] = 2;
    parent[10..12].copy_from_slice(&(1u16).to_le_bytes());
    parent[24..32].copy_from_slice(&(1u64).to_le_bytes());
    parent[32..40].copy_from_slice(&(30u64).to_le_bytes());
    parent[40..48].copy_from_slice(&(2u64).to_le_bytes());
    let mut left = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    left[..8].copy_from_slice(b"AIONBTB1");
    left[8] = 2;
    left[10..12].copy_from_slice(&(1u16).to_le_bytes());
    left[24..32].copy_from_slice(&(10u64).to_le_bytes());
    left[32..40].copy_from_slice(&(20u64).to_le_bytes());
    left[40..48].copy_from_slice(&(11u64).to_le_bytes());
    let mut right = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    right[..8].copy_from_slice(b"AIONBTB1");
    right[8] = 2;
    right[10..12].copy_from_slice(&(3u16).to_le_bytes());
    right[24..32].copy_from_slice(&(30u64).to_le_bytes());
    for (idx, (k, v)) in [(40u64, 12u64), (50, 13), (60, 14)].into_iter().enumerate() {
        let offset = 32 + idx * 16;
        right[offset..offset + 8].copy_from_slice(&k.to_le_bytes());
        right[offset + 8..offset + 16].copy_from_slice(&v.to_le_bytes());
    }
    std::fs::write(
        &relation_path,
        [&parent[..], &left[..], &right[..]].concat(),
    )
    .unwrap();

    crate::engine::recovery::apply_disk_btree_internal_redistribute(
        &index_pages_dir,
        RelationId::new(relation_id),
        1,
        2,
        0,
        0,
        1,
        10,
        12,
        &[(20, 11), (30, 30)],
        &[(50, 13), (60, 14)],
        40,
    )
    .unwrap();

    let parent_entries = read_disk_internal_entries(&dir, index_id, 0).1;
    let (left_first_child, left_entries) = read_disk_internal_entries(&dir, index_id, 1);
    let (right_first_child, right_entries) = read_disk_internal_entries(&dir, index_id, 2);
    assert_eq!(parent_entries, vec![(40, 2)]);
    assert_eq!(left_first_child, 10);
    assert_eq!(left_entries, vec![(20, 11), (30, 30)]);
    assert_eq!(right_first_child, 12);
    assert_eq!(right_entries, vec![(50, 13), (60, 14)]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_leaf_merge() {
    let dir = test_dir("recover_disk_btree_leaf_merge");
    let index_id = IndexId::new(1006);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id));

    let mut meta = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    meta[..8].copy_from_slice(b"AIONBTM1");
    meta[8..16].copy_from_slice(&(1u64).to_le_bytes());
    meta[16..20].copy_from_slice(&(2u32).to_le_bytes());
    meta[20..28].copy_from_slice(&(4u64).to_le_bytes());
    meta[28..36].copy_from_slice(&(99u64).to_le_bytes());
    let mut parent = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent[..8].copy_from_slice(b"AIONBTB1");
    parent[8] = 2;
    parent[10..12].copy_from_slice(&(1u16).to_le_bytes());
    parent[24..32].copy_from_slice(&(2u64).to_le_bytes());
    parent[32..40].copy_from_slice(&(20u64).to_le_bytes());
    parent[40..48].copy_from_slice(&(3u64).to_le_bytes());
    let mut left = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    left[..8].copy_from_slice(b"AIONBTB1");
    left[8] = 1;
    left[10..12].copy_from_slice(&(1u16).to_le_bytes());
    left[16..24].copy_from_slice(&(2u64).to_le_bytes());
    left[32..40].copy_from_slice(&(10u64).to_le_bytes());
    left[40..48].copy_from_slice(&(1u64).to_le_bytes());
    let mut right = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    right[..8].copy_from_slice(b"AIONBTB1");
    right[8] = 1;
    right[10..12].copy_from_slice(&(2u16).to_le_bytes());
    right[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    for (idx, (k, v)) in [(20u64, 2u64), (30, 3)].into_iter().enumerate() {
        let offset = 32 + idx * 16;
        right[offset..offset + 8].copy_from_slice(&k.to_le_bytes());
        right[offset + 8..offset + 16].copy_from_slice(&v.to_le_bytes());
    }
    std::fs::write(
        &relation_path,
        [&meta[..], &parent[..], &left[..], &right[..]].concat(),
    )
    .unwrap();

    crate::engine::recovery::apply_disk_btree_leaf_merge(
        &index_pages_dir,
        RelationId::new(relation_id),
        2,
        3,
        1,
        2,
        20,
        &[(10, 1), (20, 2), (30, 3)],
        u64::MAX,
        99,
    )
    .unwrap();

    let parent_entries = read_disk_internal_entries(&dir, index_id, 1).1;
    let left_entries = read_disk_leaf_entries(&dir, index_id, 2);
    let right_entries = read_disk_leaf_entries(&dir, index_id, 3);
    assert!(parent_entries.is_empty());
    assert_eq!(left_entries, vec![(10, 1), (20, 2), (30, 3)]);
    assert!(right_entries.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_internal_merge() {
    let dir = test_dir("recover_disk_btree_internal_merge");
    let index_id = IndexId::new(1007);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id));

    let mut meta = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    meta[..8].copy_from_slice(b"AIONBTM1");
    meta[8..16].copy_from_slice(&(1u64).to_le_bytes());
    meta[16..20].copy_from_slice(&(3u32).to_le_bytes());
    meta[20..28].copy_from_slice(&(4u64).to_le_bytes());
    meta[28..36].copy_from_slice(&(88u64).to_le_bytes());
    let mut parent = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent[..8].copy_from_slice(b"AIONBTB1");
    parent[8] = 2;
    parent[10..12].copy_from_slice(&(1u16).to_le_bytes());
    parent[24..32].copy_from_slice(&(2u64).to_le_bytes());
    parent[32..40].copy_from_slice(&(40u64).to_le_bytes());
    parent[40..48].copy_from_slice(&(3u64).to_le_bytes());
    let mut left = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    left[..8].copy_from_slice(b"AIONBTB1");
    left[8] = 2;
    left[10..12].copy_from_slice(&(1u16).to_le_bytes());
    left[24..32].copy_from_slice(&(10u64).to_le_bytes());
    left[32..40].copy_from_slice(&(20u64).to_le_bytes());
    left[40..48].copy_from_slice(&(11u64).to_le_bytes());
    let mut right = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    right[..8].copy_from_slice(b"AIONBTB1");
    right[8] = 2;
    right[10..12].copy_from_slice(&(2u16).to_le_bytes());
    right[24..32].copy_from_slice(&(50u64).to_le_bytes());
    for (idx, (k, v)) in [(60u64, 13u64), (70, 14)].into_iter().enumerate() {
        let offset = 32 + idx * 16;
        right[offset..offset + 8].copy_from_slice(&k.to_le_bytes());
        right[offset + 8..offset + 16].copy_from_slice(&v.to_le_bytes());
    }
    std::fs::write(
        &relation_path,
        [&meta[..], &parent[..], &left[..], &right[..]].concat(),
    )
    .unwrap();

    crate::engine::recovery::apply_disk_btree_internal_merge(
        &index_pages_dir,
        RelationId::new(relation_id),
        2,
        3,
        1,
        2,
        40,
        10,
        &[(20, 11), (40, 50), (60, 13), (70, 14)],
        88,
    )
    .unwrap();

    let parent_entries = read_disk_internal_entries(&dir, index_id, 1).1;
    let (left_first_child, left_entries) = read_disk_internal_entries(&dir, index_id, 2);
    let right_entries = read_disk_leaf_entries(&dir, index_id, 3);
    assert!(parent_entries.is_empty());
    assert_eq!(left_first_child, 10);
    assert_eq!(left_entries, vec![(20, 11), (40, 50), (60, 13), (70, 14)]);
    assert!(right_entries.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_root_shrink_leaf() {
    let dir = test_dir("recover_disk_btree_root_shrink_leaf");
    let index_id = IndexId::new(1008);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id));

    let mut leaf = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    leaf[..8].copy_from_slice(b"AIONBTB1");
    std::fs::write(&relation_path, [&leaf[..], &leaf[..], &leaf[..]].concat()).unwrap();

    crate::engine::recovery::apply_disk_btree_root_shrink_leaf(
        &index_pages_dir,
        RelationId::new(relation_id),
        1,
        &[(10, 1), (20, 2), (30, 3)],
        u64::MAX,
        &[(0, 77), (2, 0)],
    )
    .unwrap();

    let root_entries = read_disk_leaf_entries(&dir, index_id, 1);
    let freed_zero = read_disk_leaf_entries(&dir, index_id, 0);
    let freed_two = read_disk_leaf_entries(&dir, index_id, 2);
    assert_eq!(root_entries, vec![(10, 1), (20, 2), (30, 3)]);
    assert!(freed_zero.is_empty());
    assert!(freed_two.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_root_shrink_internal() {
    let dir = test_dir("recover_disk_btree_root_shrink_internal");
    let index_id = IndexId::new(1009);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id));

    let mut leaf = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    leaf[..8].copy_from_slice(b"AIONBTB1");
    std::fs::write(&relation_path, [&leaf[..], &leaf[..], &leaf[..]].concat()).unwrap();

    crate::engine::recovery::apply_disk_btree_root_shrink_internal(
        &index_pages_dir,
        RelationId::new(relation_id),
        1,
        10,
        &[(20, 11), (40, 50), (60, 13)],
        &[(0, 88), (2, 0)],
    )
    .unwrap();

    let (root_first_child, root_entries) = read_disk_internal_entries(&dir, index_id, 1);
    let freed_zero = read_disk_leaf_entries(&dir, index_id, 0);
    let freed_two = read_disk_leaf_entries(&dir, index_id, 2);
    assert_eq!(root_first_child, 10);
    assert_eq!(root_entries, vec![(20, 11), (40, 50), (60, 13)]);
    assert!(freed_zero.is_empty());
    assert!(freed_two.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_internal_collapse() {
    let dir = test_dir("recover_disk_btree_internal_collapse");
    let index_id = IndexId::new(1010);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id));

    let mut parent = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent[..8].copy_from_slice(b"AIONBTB1");
    parent[8] = 2;
    parent[10..12].copy_from_slice(&(1u16).to_le_bytes());
    parent[24..32].copy_from_slice(&(2u64).to_le_bytes());
    parent[32..40].copy_from_slice(&(40u64).to_le_bytes());
    parent[40..48].copy_from_slice(&(3u64).to_le_bytes());
    let mut collapsed = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    collapsed[..8].copy_from_slice(b"AIONBTB1");
    collapsed[8] = 2;
    collapsed[24..32].copy_from_slice(&(9u64).to_le_bytes());
    let filler = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    std::fs::write(
        &relation_path,
        [&filler[..], &parent[..], &collapsed[..], &filler[..]].concat(),
    )
    .unwrap();

    crate::engine::recovery::apply_disk_btree_internal_collapse(
        &index_pages_dir,
        RelationId::new(relation_id),
        1,
        0,
        9,
        9,
        2,
        55,
    )
    .unwrap();

    let (first_child, entries) = read_disk_internal_entries(&dir, index_id, 1);
    let freed_entries = read_disk_leaf_entries(&dir, index_id, 2);
    assert_eq!(first_child, 9);
    assert_eq!(entries, vec![(40, 3)]);
    assert!(freed_entries.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_root_promote_single_child() {
    let dir = test_dir("recover_disk_btree_root_promote_single_child");
    let index_id = IndexId::new(1011);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id));

    let mut root = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    root[..8].copy_from_slice(b"AIONBTB1");
    root[8] = 2;
    root[24..32].copy_from_slice(&(2u64).to_le_bytes());
    let filler = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    std::fs::write(
        &relation_path,
        [&filler[..], &root[..], &filler[..]].concat(),
    )
    .unwrap();

    crate::engine::recovery::apply_disk_btree_root_promote_single_child(
        &index_pages_dir,
        RelationId::new(relation_id),
        1,
        77,
    )
    .unwrap();

    let freed_entries = read_disk_leaf_entries(&dir, index_id, 1);
    assert!(freed_entries.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_root_promote_collapsed_chain() {
    let dir = test_dir("recover_disk_btree_root_promote_collapsed_chain");
    let index_id = IndexId::new(1012);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id));

    let filler = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    std::fs::write(
        &relation_path,
        [&filler[..], &filler[..], &filler[..], &filler[..]].concat(),
    )
    .unwrap();

    crate::engine::recovery::apply_disk_btree_root_promote_collapsed_chain(
        &index_pages_dir,
        RelationId::new(relation_id),
        &[(1, 77), (2, 1)],
    )
    .unwrap();

    let freed_one = read_disk_leaf_entries(&dir, index_id, 1);
    let freed_two = read_disk_leaf_entries(&dir, index_id, 2);
    assert!(freed_one.is_empty());
    assert!(freed_two.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_deep_root_promote_collapsed_chain() {
    let dir = test_dir("recover_disk_btree_deep_root_promote_collapsed_chain");
    let index_id = IndexId::new(1014);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id));

    let filler = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    std::fs::write(
        &relation_path,
        [
            &filler[..],
            &filler[..],
            &filler[..],
            &filler[..],
            &filler[..],
        ]
        .concat(),
    )
    .unwrap();

    crate::engine::recovery::apply_disk_btree_root_promote_collapsed_chain(
        &index_pages_dir,
        RelationId::new(relation_id),
        &[(1, 77), (2, 1), (3, 2)],
    )
    .unwrap();

    let freed_one = read_disk_leaf_entries(&dir, index_id, 1);
    let freed_two = read_disk_leaf_entries(&dir, index_id, 2);
    let freed_three = read_disk_leaf_entries(&dir, index_id, 3);
    assert!(freed_one.is_empty());
    assert!(freed_two.is_empty());
    assert!(freed_three.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_internal_collapse_chain() {
    let dir = test_dir("recover_disk_btree_internal_collapse_chain");
    let index_id = IndexId::new(1013);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id));

    let mut parent_a = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent_a[..8].copy_from_slice(b"AIONBTB1");
    parent_a[8] = 2;
    parent_a[24..32].copy_from_slice(&(2u64).to_le_bytes());
    let mut parent_b = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent_b[..8].copy_from_slice(b"AIONBTB1");
    parent_b[8] = 2;
    parent_b[24..32].copy_from_slice(&(3u64).to_le_bytes());
    let mut parent_c = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent_c[..8].copy_from_slice(b"AIONBTB1");
    parent_c[8] = 2;
    parent_c[24..32].copy_from_slice(&(11u64).to_le_bytes());
    let filler = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    std::fs::write(
        &relation_path,
        [&filler[..], &parent_a[..], &parent_b[..], &parent_c[..]].concat(),
    )
    .unwrap();

    crate::engine::recovery::apply_disk_btree_internal_collapse_chain(
        &index_pages_dir,
        RelationId::new(relation_id),
        &[(2, 0, 3, 11, 3, 55), (1, 0, 11, 11, 2, 3)],
    )
    .unwrap();

    let (first_child_a, entries_a) = read_disk_internal_entries(&dir, index_id, 1);
    let entries_b = read_disk_leaf_entries(&dir, index_id, 2);
    let entries_c = read_disk_leaf_entries(&dir, index_id, 3);
    assert_eq!(first_child_a, 11);
    assert!(entries_a.is_empty());
    assert!(entries_b.is_empty());
    assert!(entries_c.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_disk_btree_internal_collapse_chain_for_nonleftmost_slot() {
    let dir = test_dir("recover_disk_btree_internal_collapse_chain_nonleftmost");
    let index_id = IndexId::new(1015);
    let index_pages_dir = dir.join("index_pages");
    std::fs::create_dir_all(&index_pages_dir).unwrap();
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = index_pages_dir.join(format!("data_{:06}.db", relation_id));

    let mut parent = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent[..8].copy_from_slice(b"AIONBTB1");
    parent[8] = 2;
    parent[10..12].copy_from_slice(&(2u16).to_le_bytes());
    parent[24..32].copy_from_slice(&(7u64).to_le_bytes());
    parent[32..40].copy_from_slice(&(40u64).to_le_bytes());
    parent[40..48].copy_from_slice(&(2u64).to_le_bytes());
    parent[48..56].copy_from_slice(&(80u64).to_le_bytes());
    parent[56..64].copy_from_slice(&(9u64).to_le_bytes());
    let mut parent_b = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent_b[..8].copy_from_slice(b"AIONBTB1");
    parent_b[8] = 2;
    parent_b[24..32].copy_from_slice(&(3u64).to_le_bytes());
    let mut parent_c = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    parent_c[..8].copy_from_slice(b"AIONBTB1");
    parent_c[8] = 2;
    parent_c[24..32].copy_from_slice(&(11u64).to_le_bytes());
    let filler = vec![0u8; aiondb_buffer_pool::PAGE_SIZE];
    std::fs::write(
        &relation_path,
        [&filler[..], &parent[..], &parent_b[..], &parent_c[..]].concat(),
    )
    .unwrap();

    crate::engine::recovery::apply_disk_btree_internal_collapse_chain(
        &index_pages_dir,
        RelationId::new(relation_id),
        &[(2, 0, 3, 11, 3, 55), (1, 1, 7, 11, 2, 3)],
    )
    .unwrap();

    let (first_child_parent, entries_parent) = read_disk_internal_entries(&dir, index_id, 1);
    let entries_b = read_disk_leaf_entries(&dir, index_id, 2);
    let entries_c = read_disk_leaf_entries(&dir, index_id, 3);
    assert_eq!(first_child_parent, 7);
    assert_eq!(entries_parent, vec![(40, 11), (80, 9)]);
    assert!(entries_b.is_empty());
    assert!(entries_c.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}
