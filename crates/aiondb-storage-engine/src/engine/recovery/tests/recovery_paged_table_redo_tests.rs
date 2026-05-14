use super::*;

#[test]
fn recover_replays_page_patch_into_paged_tables() {
    use aiondb_storage_api::StorageTxnParticipant;

    let dir = test_dir("recover_paged_table_page_patch");
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
                Row::new(vec![Value::Int(1), Value::Text("table-patch".into())]),
            )
            .expect("row should be inserted");
        storage
            .checkpoint()
            .expect("checkpoint should materialize paged table state");
    }

    let mut writer = WalWriter::open(config.clone()).unwrap();
    writer
        .append(&WalRecord::PagePatch {
            relation_id: desc.table_id,
            page_number: 0,
            segments: vec![(8, 123u64.to_le_bytes().to_vec())],
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
    assert!(bytes.len() >= 16);
    assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 123);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_page_set_u64_batch_into_paged_tables() {
    use aiondb_storage_api::StorageTxnParticipant;

    let dir = test_dir("recover_paged_table_page_set_u64_batch");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        storage
            .create_table_storage(TxnId::default(), &desc)
            .expect("table should be created");
        for value in 0..64 {
            storage
                .insert(
                    TxnId::default(),
                    desc.table_id,
                    Row::new(vec![
                        Value::Int(value),
                        Value::Text("table-u64-batch".into()),
                    ]),
                )
                .expect("row should be inserted");
        }
        storage
            .checkpoint()
            .expect("checkpoint should materialize paged table state");
    }

    let mut writer = WalWriter::open(config.clone()).unwrap();
    writer
        .append(&WalRecord::PageSetU64Batch {
            relation_id: desc.table_id,
            updates: vec![(0, 8, 321), (1, 8, 654)],
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
    assert!(bytes.len() >= 16 + aiondb_buffer_pool::PAGE_SIZE);
    assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 321);
    assert_eq!(
        u64::from_le_bytes(
            bytes[aiondb_buffer_pool::PAGE_SIZE + 8..aiondb_buffer_pool::PAGE_SIZE + 16]
                .try_into()
                .unwrap()
        ),
        654
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_replays_page_patch_batch_into_paged_tables() {
    use aiondb_storage_api::StorageTxnParticipant;

    let dir = test_dir("recover_paged_table_page_patch_batch");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        storage
            .create_table_storage(TxnId::default(), &desc)
            .expect("table should be created");
        for value in 0..64 {
            storage
                .insert(
                    TxnId::default(),
                    desc.table_id,
                    Row::new(vec![
                        Value::Int(value),
                        Value::Text("table-patch-batch".into()),
                    ]),
                )
                .expect("row should be inserted");
        }
        storage
            .checkpoint()
            .expect("checkpoint should materialize paged table state");
    }

    let mut writer = WalWriter::open(config.clone()).unwrap();
    writer
        .append(&WalRecord::PagePatchBatch {
            relation_id: desc.table_id,
            patches: vec![
                (0, vec![(8, 321u64.to_le_bytes().to_vec())]),
                (1, vec![(8, 654u64.to_le_bytes().to_vec())]),
            ],
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
    assert!(bytes.len() >= 16 + aiondb_buffer_pool::PAGE_SIZE);
    assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 321);
    assert_eq!(
        u64::from_le_bytes(
            bytes[aiondb_buffer_pool::PAGE_SIZE + 8..aiondb_buffer_pool::PAGE_SIZE + 16]
                .try_into()
                .unwrap()
        ),
        654
    );

    let _ = std::fs::remove_dir_all(&dir);
}
