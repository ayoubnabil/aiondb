use super::*;

#[test]
fn recover_replays_full_page_image_into_paged_tables() {
    use aiondb_buffer_pool::PAGE_SIZE;
    use aiondb_storage_api::StorageTxnParticipant;

    let dir = test_dir("recover_full_page_image");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        storage
            .create_table_storage(TxnId::default(), &desc)
            .expect("table should be created");
        storage
            .insert(
                TxnId::default(),
                desc.table_id,
                Row::new(vec![Value::Int(1), Value::Text("fpw-seed".into())]),
            )
            .expect("row should be inserted");
        storage
            .checkpoint()
            .expect("checkpoint should materialize paged table state");
    }

    let page_image = vec![0xEF; PAGE_SIZE];
    let mut writer = WalWriter::open(config.clone()).unwrap();
    writer
        .append(&WalRecord::FullPageImage {
            relation_id: desc.table_id,
            page_number: 0,
            page_data: page_image.clone(),
        })
        .unwrap();
    writer.flush().unwrap();
    drop(writer);

    let _ = InMemoryStorage::open_with_recovery(config).unwrap();

    let table_root = dir.join("table_pages");
    let current_version = std::fs::read_to_string(table_root.join("CURRENT")).unwrap();
    let relation_path = table_root
        .join(current_version.trim())
        .join(format!("data_{:06}.db", desc.table_id.get()));
    let bytes = std::fs::read(relation_path).unwrap();
    assert!(bytes.len() >= PAGE_SIZE);
    assert_eq!(&bytes[..PAGE_SIZE], &page_image[..]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_full_page_image_into_disk_index_pages() {
    use aiondb_buffer_pool::PAGE_SIZE;
    use aiondb_storage_api::StorageTxnParticipant;

    let dir = test_dir("recover_disk_index_full_page_image");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let index_id = IndexId::new(990);
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
            ivf_flat_options: None,
    };

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        storage
            .create_table_storage(TxnId::default(), &desc)
            .expect("table should be created");
        storage
            .insert(
                TxnId::default(),
                desc.table_id,
                Row::new(vec![Value::Int(1), Value::Text("disk-fpw".into())]),
            )
            .expect("row should be inserted");
        storage
            .create_index_storage(TxnId::default(), &index_desc)
            .expect("index should be created");
        storage
            .checkpoint()
            .expect("checkpoint should materialize disk index state");
    }

    let relation_id = RelationId::new(disk_ordered_relation_id(index_id));
    let relation_path = dir
        .join("index_pages")
        .join(format!("data_{:06}.db", relation_id.get()));
    let bytes = std::fs::read(&relation_path).unwrap();
    assert!(bytes.len() >= PAGE_SIZE);
    let mut page_image = bytes[..PAGE_SIZE].to_vec();
    page_image[20..28].copy_from_slice(&777u64.to_le_bytes());

    let mut writer = WalWriter::open(config.clone()).unwrap();
    writer
        .append(&WalRecord::FullPageImage {
            relation_id,
            page_number: 0,
            page_data: page_image.clone(),
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
    assert_eq!(stats.page_count, 777);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_full_page_image_batch_into_disk_index_pages() {
    use aiondb_buffer_pool::PAGE_SIZE;
    use aiondb_storage_api::StorageTxnParticipant;

    let dir = test_dir("recover_disk_index_full_page_image_batch");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let index_id = IndexId::new(991);
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
            ivf_flat_options: None,
    };

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        storage
            .create_table_storage(TxnId::default(), &desc)
            .expect("table should be created");
        for value in 0..200 {
            storage
                .insert(
                    TxnId::default(),
                    desc.table_id,
                    Row::new(vec![Value::Int(value), Value::Text("disk-batch".into())]),
                )
                .expect("row should be inserted");
        }
        storage
            .create_index_storage(TxnId::default(), &index_desc)
            .expect("index should be created");
        storage
            .checkpoint()
            .expect("checkpoint should materialize disk index state");
    }

    let relation_id = RelationId::new(disk_ordered_relation_id(index_id));
    let relation_path = dir
        .join("index_pages")
        .join(format!("data_{:06}.db", relation_id.get()));
    let bytes = std::fs::read(&relation_path).unwrap();
    assert!(bytes.len() >= PAGE_SIZE);

    let mut page0 = bytes[..PAGE_SIZE].to_vec();
    page0[20..28].copy_from_slice(&888u64.to_le_bytes());
    let page1 = if bytes.len() >= PAGE_SIZE * 2 {
        bytes[PAGE_SIZE..PAGE_SIZE * 2].to_vec()
    } else {
        bytes[..PAGE_SIZE].to_vec()
    };

    let mut writer = WalWriter::open(config.clone()).unwrap();
    writer
        .append(&WalRecord::FullPageImageBatch {
            relation_id,
            pages: vec![(0, page0), (1, page1)],
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
    assert_eq!(stats.page_count, 888);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_page_patch_into_disk_index_pages() {
    use aiondb_storage_api::StorageTxnParticipant;

    let dir = test_dir("recover_disk_index_page_patch");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let index_id = IndexId::new(992);
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
            ivf_flat_options: None,
    };

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        storage
            .create_table_storage(TxnId::default(), &desc)
            .expect("table should be created");
        storage
            .insert(
                TxnId::default(),
                desc.table_id,
                Row::new(vec![Value::Int(1), Value::Text("disk-patch".into())]),
            )
            .expect("row should be inserted");
        storage
            .create_index_storage(TxnId::default(), &index_desc)
            .expect("index should be created");
        storage
            .checkpoint()
            .expect("checkpoint should materialize disk index state");
    }

    let relation_id = RelationId::new(disk_ordered_relation_id(index_id));
    let mut writer = WalWriter::open(config.clone()).unwrap();
    writer
        .append(&WalRecord::PagePatch {
            relation_id,
            page_number: 0,
            segments: vec![(20, 999u64.to_le_bytes().to_vec())],
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
    assert_eq!(stats.page_count, 999);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_page_patch_batch_into_disk_index_pages() {
    use aiondb_storage_api::StorageTxnParticipant;

    let dir = test_dir("recover_disk_index_page_patch_batch");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let index_id = IndexId::new(994);
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
            ivf_flat_options: None,
    };

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        storage
            .create_table_storage(TxnId::default(), &desc)
            .expect("table should be created");
        for value in 0..200 {
            storage
                .insert(
                    TxnId::default(),
                    desc.table_id,
                    Row::new(vec![
                        Value::Int(value),
                        Value::Text("disk-patch-batch".into()),
                    ]),
                )
                .expect("row should be inserted");
        }
        storage
            .create_index_storage(TxnId::default(), &index_desc)
            .expect("index should be created");
        storage
            .checkpoint()
            .expect("checkpoint should materialize disk index state");
    }

    let relation_id = RelationId::new(disk_ordered_relation_id(index_id));
    let mut writer = WalWriter::open(config.clone()).unwrap();
    writer
        .append(&WalRecord::PagePatchBatch {
            relation_id,
            patches: vec![
                (0, vec![(20, 444u64.to_le_bytes().to_vec())]),
                (1, vec![(20, 555u64.to_le_bytes().to_vec())]),
            ],
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
    assert_eq!(stats.page_count, 444);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_page_set_u64_batch_into_disk_index_pages() {
    use aiondb_storage_api::StorageTxnParticipant;

    let dir = test_dir("recover_disk_index_page_set_u64_batch");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let index_id = IndexId::new(995);
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
            ivf_flat_options: None,
    };

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        storage
            .create_table_storage(TxnId::default(), &desc)
            .expect("table should be created");
        for value in 0..200 {
            storage
                .insert(
                    TxnId::default(),
                    desc.table_id,
                    Row::new(vec![
                        Value::Int(value),
                        Value::Text("disk-u64-batch".into()),
                    ]),
                )
                .expect("row should be inserted");
        }
        storage
            .create_index_storage(TxnId::default(), &index_desc)
            .expect("index should be created");
        storage
            .checkpoint()
            .expect("checkpoint should materialize disk index state");
    }

    let relation_id = RelationId::new(disk_ordered_relation_id(index_id));
    let mut writer = WalWriter::open(config.clone()).unwrap();
    writer
        .append(&WalRecord::PageSetU64Batch {
            relation_id,
            updates: vec![(0, 20, 777), (1, 20, 888)],
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
    assert_eq!(stats.page_count, 777);

    let _ = std::fs::remove_dir_all(&dir);
}
