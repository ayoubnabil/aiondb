use super::*;

#[test]
fn inserts_and_scans_rows() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(1);
    create_table(&storage, table_id);

    insert_row(&storage, TxnId::default(), table_id, 7, "alice");

    let records = collect_stream(
        storage
            .scan_table(TxnId::default(), &snapshot(), table_id, None)
            .expect("scan table"),
    );
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].row,
        Row::new(vec![Value::Int(7), Value::Text("alice".to_owned())])
    );
}

#[test]
fn updates_and_deletes_rows() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2);
    create_table(&storage, table_id);

    let tuple_id = insert_row(&storage, TxnId::default(), table_id, 7, "alice");
    storage
        .update(
            TxnId::default(),
            table_id,
            tuple_id,
            Row::new(vec![Value::Int(8), Value::Text("bob".to_owned())]),
        )
        .expect("update row");

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch updated row")
        .expect("row exists");
    assert_eq!(
        updated,
        Row::new(vec![Value::Int(8), Value::Text("bob".to_owned())])
    );

    storage
        .delete(TxnId::default(), table_id, tuple_id)
        .expect("delete row");
    assert!(storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch deleted row")
        .is_none());
}

#[test]
fn index_scan_honors_exact_key_range() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(3);
    let index_id = IndexId::new(3);
    create_table(&storage, table_id);
    create_index(&storage, table_id, index_id);

    insert_row(&storage, TxnId::default(), table_id, 1, "alice");
    insert_row(&storage, TxnId::default(), table_id, 2, "bob");

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(2)]),
                    upper: Bound::Included(vec![Value::Int(2)]),
                },
                None,
            )
            .expect("scan index"),
    );
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].row,
        Row::new(vec![Value::Int(2), Value::Text("bob".to_owned())])
    );
}

#[test]
fn disk_ordered_index_serves_safe_latest_int_scan() {
    let (storage, _dir) = storage_with_wal("disk_ordered_index_serves_safe_latest_int_scan");
    let table_id = RelationId::new(30);
    let index_id = IndexId::new(30);
    create_table(&storage, table_id);
    create_unique_index(&storage, table_id, index_id);

    insert_row(&storage, TxnId::default(), table_id, 10, "ten");
    insert_row(&storage, TxnId::default(), table_id, 20, "twenty");

    // Keep the descriptor but remove in-memory candidates. A successful
    // latest INT range scan below proves candidate IDs came from the
    // page-backed disk mirror rather than IndexData.
    {
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(20)]),
                    upper: Bound::Included(vec![Value::Int(20)]),
                },
                None,
            )
            .expect("scan disk ordered index"),
    );
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].row,
        Row::new(vec![Value::Int(20), Value::Text("twenty".to_owned())])
    );
}

#[test]
fn disk_ordered_unique_index_serves_latest_exact_scan_without_ram_candidates() {
    let (storage, _dir) = storage_with_wal(
        "disk_ordered_unique_index_serves_latest_exact_scan_without_ram_candidates",
    );
    let table_id = RelationId::new(229);
    let index_id = IndexId::new(229);
    create_table(&storage, table_id);
    create_unique_index(&storage, table_id, index_id);

    insert_row(&storage, TxnId::default(), table_id, 10, "ten");
    insert_row(&storage, TxnId::default(), table_id, 20, "twenty");

    {
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(20)]),
                    upper: Bound::Included(vec![Value::Int(20)]),
                },
                None,
            )
            .expect("scan unique disk ordered index"),
    );
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].row,
        Row::new(vec![Value::Int(20), Value::Text("twenty".to_owned())])
    );
}

#[test]
fn disk_ordered_unique_preflight_rejects_duplicate_without_ram_candidates() {
    let (storage, _dir) =
        storage_with_wal("disk_ordered_unique_preflight_rejects_duplicate_without_ram_candidates");
    let table_id = RelationId::new(228);
    let index_id = IndexId::new(228);
    create_table(&storage, table_id);
    create_unique_index(&storage, table_id, index_id);

    insert_row(&storage, TxnId::default(), table_id, 20, "twenty");

    {
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let error = storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![Value::Int(20), Value::Text("duplicate".to_owned())]),
        )
        .expect_err("duplicate insert must fail");
    assert_eq!(error.sqlstate(), SqlState::UniqueViolation);
}

#[test]
fn disk_ordered_index_serves_latest_int_scan_with_overlay() {
    let (storage, _dir) =
        storage_with_wal("disk_ordered_index_serves_latest_int_scan_with_overlay");
    let table_id = RelationId::new(230);
    let index_id = IndexId::new(230);
    let txn = TxnId::new(230);
    create_table(&storage, table_id);
    create_index(&storage, table_id, index_id);

    insert_row(&storage, TxnId::default(), table_id, 10, "ten");
    let gone = insert_row(&storage, TxnId::default(), table_id, 20, "twenty");
    let moved = insert_row(&storage, TxnId::default(), table_id, 30, "thirty");

    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");
    storage
        .delete(txn, table_id, gone)
        .expect("delete row in overlay");
    storage
        .update(
            txn,
            table_id,
            moved,
            Row::new(vec![Value::Int(25), Value::Text("twenty-five".to_owned())]),
        )
        .expect("update row in overlay");
    insert_row(&storage, txn, table_id, 25, "overlay");

    // Remove in-memory candidates. A successful transactional scan below proves
    // the latest committed base candidates came from the disk mirror and were
    // then merged with overlay insert/update/delete state.
    {
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                txn,
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(25)]),
                    upper: Bound::Included(vec![Value::Int(25)]),
                },
                None,
            )
            .expect("scan disk ordered index with overlay"),
    );
    assert_eq!(records.len(), 2);
    assert_eq!(
        records
            .iter()
            .map(|record| record.row.values[1].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Text("twenty-five".to_owned()),
            Value::Text("overlay".to_owned())
        ]
    );

    let removed = collect_stream(
        storage
            .scan_index(
                txn,
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(20)]),
                    upper: Bound::Included(vec![Value::Int(20)]),
                },
                None,
            )
            .expect("scan deleted key through overlay"),
    );
    assert!(removed.is_empty());

    let untouched = collect_stream(
        storage
            .scan_index(
                txn,
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(10)]),
                    upper: Bound::Included(vec![Value::Int(10)]),
                },
                None,
            )
            .expect("scan untouched key through overlay"),
    );
    assert_eq!(untouched.len(), 1);
    assert_eq!(untouched[0].row.values[1], Value::Text("ten".to_owned()));
    storage.rollback_txn(txn).expect("rollback txn");
}

#[test]
fn disk_ordered_index_rebuilds_missing_committed_mirror_on_demand() {
    let (storage, _dir) =
        storage_with_wal("disk_ordered_index_rebuilds_missing_committed_mirror_on_demand");
    let table_id = RelationId::new(219);
    let index_id = IndexId::new(219);
    create_table(&storage, table_id);
    create_unique_index(&storage, table_id, index_id);
    insert_row(&storage, TxnId::default(), table_id, 7, "alice");
    insert_row(&storage, TxnId::default(), table_id, 8, "bob");

    {
        storage.disk_ordered_indexes.write().remove(&index_id);
        storage.disk_var_exact_indexes.write().remove(&index_id);
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("committed index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(8)]),
                    upper: Bound::Included(vec![Value::Int(8)]),
                },
                None,
            )
            .expect("scan committed index after mirror rebuild"),
    );
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].row,
        Row::new(vec![Value::Int(8), Value::Text("bob".to_owned())])
    );
}

#[test]
fn visible_index_row_count_uses_latest_disk_path_with_overlay() {
    let (storage, _dir) =
        storage_with_wal("visible_index_row_count_uses_latest_disk_path_with_overlay");
    let table_id = RelationId::new(231);
    let index_id = IndexId::new(231);
    let txn = TxnId::new(231);
    create_table(&storage, table_id);
    create_index(&storage, table_id, index_id);

    let gone = insert_row(&storage, TxnId::default(), table_id, 20, "twenty");
    let moved = insert_row(&storage, TxnId::default(), table_id, 30, "thirty");

    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");
    storage
        .delete(txn, table_id, gone)
        .expect("delete row in overlay");
    storage
        .update(
            txn,
            table_id,
            moved,
            Row::new(vec![Value::Int(25), Value::Text("twenty-five".to_owned())]),
        )
        .expect("update row in overlay");
    insert_row(&storage, txn, table_id, 25, "overlay");

    {
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let count = storage
        .visible_index_row_count(
            txn,
            &snapshot(),
            index_id,
            KeyRange {
                lower: Bound::Included(vec![Value::Int(25)]),
                upper: Bound::Included(vec![Value::Int(25)]),
            },
        )
        .expect("visible index row count");
    assert_eq!(count, 2);

    storage.rollback_txn(txn).expect("rollback txn");
}

#[test]
fn disk_ordered_index_serves_latest_text_exact_scan_with_recheck() {
    let (storage, _dir) =
        storage_with_wal("disk_ordered_index_serves_latest_text_exact_scan_with_recheck");
    let table_id = RelationId::new(31);
    let index_id = IndexId::new(31);
    create_table(&storage, table_id);
    create_unique_text_index(&storage, table_id, index_id);

    insert_row(&storage, TxnId::default(), table_id, 10, "alice");
    insert_row(&storage, TxnId::default(), table_id, 20, "bob");
    insert_row(&storage, TxnId::default(), table_id, 30, "carol");

    {
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Text("bob".to_owned())]),
                    upper: Bound::Included(vec![Value::Text("bob".to_owned())]),
                },
                None,
            )
            .expect("scan disk ordered text index"),
    );
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].row.values[1], Value::Text("bob".to_owned()));
}

#[test]
fn disk_var_exact_registry_tracks_text_index_mutations() {
    let (storage, _dir) = storage_with_wal("disk_var_exact_registry_tracks_text_index_mutations");
    let table_id = RelationId::new(131);
    let index_id = IndexId::new(131);
    create_table(&storage, table_id);
    create_text_index(&storage, table_id, index_id);

    insert_row(&storage, TxnId::default(), table_id, 10, "alice");
    insert_row(&storage, TxnId::default(), table_id, 20, "bob");
    insert_row(&storage, TxnId::default(), table_id, 30, "bob");

    {
        let disk_indexes = storage.disk_var_exact_indexes.read();
        let disk_index = disk_indexes
            .get(&index_id)
            .expect("disk var exact index exists");
        assert_eq!(
            disk_index
                .exact_values([&Value::Text("bob".to_owned())])
                .expect("exact values"),
            vec![aiondb_core::TupleId::new(2), aiondb_core::TupleId::new(3)]
        );
    }

    storage
        .delete(TxnId::default(), table_id, aiondb_core::TupleId::new(2))
        .expect("delete tuple");

    {
        let disk_indexes = storage.disk_var_exact_indexes.read();
        let disk_index = disk_indexes
            .get(&index_id)
            .expect("disk var exact index still exists");
        assert_eq!(
            disk_index
                .exact_values([&Value::Text("bob".to_owned())])
                .expect("exact values after delete"),
            vec![aiondb_core::TupleId::new(3)]
        );
    }
}

#[test]
fn disk_var_exact_index_serves_latest_text_exact_scan_with_recheck() {
    let (storage, _dir) =
        storage_with_wal("disk_var_exact_index_serves_latest_text_exact_scan_with_recheck");
    let table_id = RelationId::new(132);
    let index_id = IndexId::new(132);
    create_table(&storage, table_id);
    create_text_index(&storage, table_id, index_id);

    insert_row(&storage, TxnId::default(), table_id, 10, "alice");
    insert_row(&storage, TxnId::default(), table_id, 20, "bob");
    insert_row(&storage, TxnId::default(), table_id, 30, "bob");

    {
        storage.disk_ordered_indexes.write().remove(&index_id);
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Text("bob".to_owned())]),
                    upper: Bound::Included(vec![Value::Text("bob".to_owned())]),
                },
                None,
            )
            .expect("scan disk var exact text index"),
    );
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].row.values[1], Value::Text("bob".to_owned()));
    assert_eq!(records[1].row.values[1], Value::Text("bob".to_owned()));
}

#[test]
fn disk_var_exact_index_serves_latest_composite_exact_scan_with_recheck() {
    let (storage, _dir) =
        storage_with_wal("disk_var_exact_index_serves_latest_composite_exact_scan_with_recheck");
    let table_id = RelationId::new(133);
    let index_id = IndexId::new(133);
    create_table(&storage, table_id);
    create_composite_index(&storage, table_id, index_id);

    insert_row(&storage, TxnId::default(), table_id, 7, "alice");
    insert_row(&storage, TxnId::default(), table_id, 7, "bob");
    insert_row(&storage, TxnId::default(), table_id, 8, "bob");

    {
        storage.disk_ordered_indexes.write().remove(&index_id);
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(7), Value::Text("bob".to_owned())]),
                    upper: Bound::Included(vec![Value::Int(7), Value::Text("bob".to_owned())]),
                },
                None,
            )
            .expect("scan disk var exact composite index"),
    );
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].row,
        Row::new(vec![Value::Int(7), Value::Text("bob".to_owned())])
    );
}

#[test]
fn disk_var_exact_index_serves_latest_text_range_scan_with_recheck() {
    let (storage, _dir) =
        storage_with_wal("disk_var_exact_index_serves_latest_text_range_scan_with_recheck");
    let table_id = RelationId::new(134);
    let index_id = IndexId::new(134);
    create_table(&storage, table_id);
    create_text_index(&storage, table_id, index_id);

    insert_row(&storage, TxnId::default(), table_id, 10, "a");
    insert_row(&storage, TxnId::default(), table_id, 20, "aa");
    insert_row(&storage, TxnId::default(), table_id, 30, "b");
    insert_row(&storage, TxnId::default(), table_id, 40, "ba");
    insert_row(&storage, TxnId::default(), table_id, 50, "c");

    {
        storage.disk_ordered_indexes.write().remove(&index_id);
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Text("aa".to_owned())]),
                    upper: Bound::Included(vec![Value::Text("ba".to_owned())]),
                },
                None,
            )
            .expect("scan disk var exact text range index"),
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record.row.values[1].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Text("aa".to_owned()),
            Value::Text("b".to_owned()),
            Value::Text("ba".to_owned())
        ]
    );
}

#[test]
fn disk_var_exact_index_serves_latest_uuid_range_scan_with_recheck() {
    let (storage, _dir) =
        storage_with_wal("disk_var_exact_index_serves_latest_uuid_range_scan_with_recheck");
    let table_id = RelationId::new(135);
    let index_id = IndexId::new(135);
    create_uuid_table(&storage, table_id);
    create_index(&storage, table_id, index_id);

    let low = [0x10; 16];
    let mid = [0x20; 16];
    let high = [0x30; 16];
    insert_uuid_row(&storage, table_id, low, "low");
    insert_uuid_row(&storage, table_id, mid, "mid");
    insert_uuid_row(&storage, table_id, high, "high");

    {
        storage.disk_ordered_indexes.write().remove(&index_id);
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Uuid(low)]),
                    upper: Bound::Included(vec![Value::Uuid(mid)]),
                },
                None,
            )
            .expect("scan disk var exact uuid range index"),
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record.row.values[1].clone())
            .collect::<Vec<_>>(),
        vec![Value::Text("low".to_owned()), Value::Text("mid".to_owned())]
    );
}

#[test]
fn disk_var_exact_index_serves_latest_composite_range_scan_with_recheck() {
    let (storage, _dir) =
        storage_with_wal("disk_var_exact_index_serves_latest_composite_range_scan_with_recheck");
    let table_id = RelationId::new(136);
    let index_id = IndexId::new(136);
    create_table(&storage, table_id);
    create_composite_index(&storage, table_id, index_id);

    insert_row(&storage, TxnId::default(), table_id, 7, "alice");
    insert_row(&storage, TxnId::default(), table_id, 7, "bob");
    insert_row(&storage, TxnId::default(), table_id, 8, "anna");
    insert_row(&storage, TxnId::default(), table_id, 8, "bob");

    {
        storage.disk_ordered_indexes.write().remove(&index_id);
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(7), Value::Text("bob".to_owned())]),
                    upper: Bound::Included(vec![Value::Int(8), Value::Text("anna".to_owned())]),
                },
                None,
            )
            .expect("scan disk var exact composite range index"),
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record.row.clone())
            .collect::<Vec<_>>(),
        vec![
            Row::new(vec![Value::Int(7), Value::Text("bob".to_owned())]),
            Row::new(vec![Value::Int(8), Value::Text("anna".to_owned())])
        ]
    );
}

#[test]
fn disk_var_exact_index_serves_latest_composite_prefix_range_scan_with_recheck() {
    let (storage, _dir) = storage_with_wal(
        "disk_var_exact_index_serves_latest_composite_prefix_range_scan_with_recheck",
    );
    let table_id = RelationId::new(137);
    let index_id = IndexId::new(137);
    create_table(&storage, table_id);
    create_composite_index(&storage, table_id, index_id);

    insert_row(&storage, TxnId::default(), table_id, 6, "z");
    insert_row(&storage, TxnId::default(), table_id, 7, "alice");
    insert_row(&storage, TxnId::default(), table_id, 7, "bob");
    insert_row(&storage, TxnId::default(), table_id, 8, "anna");
    insert_row(&storage, TxnId::default(), table_id, 8, "bob");
    insert_row(&storage, TxnId::default(), table_id, 9, "a");

    {
        storage.disk_ordered_indexes.write().remove(&index_id);
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(7)]),
                    upper: Bound::Included(vec![Value::Int(8)]),
                },
                None,
            )
            .expect("scan disk var exact composite prefix range index"),
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record.row.clone())
            .collect::<Vec<_>>(),
        vec![
            Row::new(vec![Value::Int(7), Value::Text("alice".to_owned())]),
            Row::new(vec![Value::Int(7), Value::Text("bob".to_owned())]),
            Row::new(vec![Value::Int(8), Value::Text("anna".to_owned())]),
            Row::new(vec![Value::Int(8), Value::Text("bob".to_owned())])
        ]
    );
}

#[test]
fn vacuum_rebuild_compacts_disk_ordered_index_pages() {
    let (storage, _dir) = storage_with_wal("vacuum_rebuild_compacts_disk_ordered_index_pages");
    let table_id = RelationId::new(138);
    let index_id = IndexId::new(138);
    create_table(&storage, table_id);
    create_unique_index(&storage, table_id, index_id);

    let tuple_ids = (0..600)
        .map(|idx| insert_row(&storage, TxnId::default(), table_id, idx, "row"))
        .collect::<Vec<_>>();
    for (idx, tuple_id) in tuple_ids.iter().enumerate().take(520) {
        storage
            .delete(TxnId::default(), table_id, *tuple_id)
            .unwrap_or_else(|err| panic!("delete tuple {idx} failed: {err}"));
    }

    let before_rebuild = {
        let disk_indexes = storage.disk_ordered_indexes.read();
        disk_indexes
            .get(&index_id)
            .expect("disk ordered index exists")
            .stats()
            .expect("disk ordered stats before rebuild")
    };

    {
        let mut state = storage.write_state().expect("write state");
        storage
            .rebuild_base_btree_indexes_after_vacuum(&mut state, table_id)
            .expect("rebuild base btree indexes after vacuum");
    }

    let after_rebuild = {
        let disk_indexes = storage.disk_ordered_indexes.read();
        disk_indexes
            .get(&index_id)
            .expect("disk ordered index rebuilt")
            .stats()
            .expect("disk ordered stats after rebuild")
    };

    assert!(after_rebuild.page_count <= before_rebuild.page_count);
}

#[test]
fn disk_ordered_index_serves_latest_text_range_scan_with_recheck() {
    let (storage, _dir) =
        storage_with_wal("disk_ordered_index_serves_latest_text_range_scan_with_recheck");
    let table_id = RelationId::new(33);
    let index_id = IndexId::new(33);
    create_table(&storage, table_id);
    create_unique_text_index(&storage, table_id, index_id);

    insert_row(&storage, TxnId::default(), table_id, 10, "alpha");
    insert_row(&storage, TxnId::default(), table_id, 20, "beta");
    insert_row(&storage, TxnId::default(), table_id, 30, "betamax");
    insert_row(&storage, TxnId::default(), table_id, 35, "betazzz");
    insert_row(&storage, TxnId::default(), table_id, 40, "zeta");

    {
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Text("beta".to_owned())]),
                    upper: Bound::Included(vec![Value::Text("betaz".to_owned())]),
                },
                None,
            )
            .expect("scan disk ordered text range index"),
    );
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].row.values[1], Value::Text("beta".to_owned()));
    assert_eq!(records[1].row.values[1], Value::Text("betamax".to_owned()));
}

#[test]
fn disk_ordered_index_serves_latest_composite_exact_scan() {
    let (storage, _dir) = storage_with_wal("disk_ordered_index_serves_latest_composite_exact_scan");
    let table_id = RelationId::new(32);
    let index_id = IndexId::new(32);
    create_table(&storage, table_id);
    create_unique_composite_index(&storage, table_id, index_id);

    insert_row(&storage, TxnId::default(), table_id, 7, "alice");
    insert_row(&storage, TxnId::default(), table_id, 7, "bob");
    insert_row(&storage, TxnId::default(), table_id, 8, "bob");

    {
        let state = storage.read_state().expect("read state");
        let index = state.indexes.get(&index_id).expect("index exists");
        let table = state.tables.get(&table_id).expect("table exists");
        assert!(super::disk_ordered_index::supports_var_exact_descriptor(
            &index.descriptor,
            &table.descriptor
        ));
    }

    {
        let disk_indexes = storage.disk_var_exact_indexes.read();
        let disk_index = disk_indexes.get(&index_id).expect("disk var index exists");
        let candidates = disk_index
            .exact_values(
                [&Value::Int(7), &Value::Text("bob".to_owned())]
                    .iter()
                    .copied(),
            )
            .expect("disk composite candidates");
        assert_eq!(candidates.len(), 1);
    }

    {
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(7), Value::Text("bob".to_owned())]),
                    upper: Bound::Included(vec![Value::Int(7), Value::Text("bob".to_owned())]),
                },
                None,
            )
            .expect("scan disk ordered composite index"),
    );
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].row,
        Row::new(vec![Value::Int(7), Value::Text("bob".to_owned())])
    );
}

#[test]
fn disk_ordered_index_serves_latest_bigint_range_scan() {
    let (storage, _dir) = storage_with_wal("disk_ordered_index_serves_latest_bigint_range_scan");
    let table_id = RelationId::new(33);
    let index_id = IndexId::new(33);
    create_bigint_table(&storage, table_id);
    create_unique_index(&storage, table_id, index_id);

    insert_bigint_row(&storage, table_id, -9_000_000_000, "low");
    insert_bigint_row(&storage, table_id, -1, "minus-one");
    insert_bigint_row(&storage, table_id, 0, "zero");
    insert_bigint_row(&storage, table_id, 9_000_000_000, "high");

    {
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::BigInt(-10)]),
                    upper: Bound::Included(vec![Value::BigInt(10)]),
                },
                None,
            )
            .expect("scan disk ordered bigint index"),
    );
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].row.values[0], Value::BigInt(-1));
    assert_eq!(records[1].row.values[0], Value::BigInt(0));
}

#[test]
fn disk_ordered_index_serves_latest_bool_range_scan() {
    let (storage, _dir) = storage_with_wal("disk_ordered_index_serves_latest_bool_range_scan");
    let table_id = RelationId::new(34);
    let index_id = IndexId::new(34);
    create_bool_table(&storage, table_id);
    create_unique_index(&storage, table_id, index_id);

    insert_bool_row(&storage, table_id, false, "disabled-a");
    insert_bool_row(&storage, table_id, true, "enabled");

    {
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Boolean(false)]),
                    upper: Bound::Included(vec![Value::Boolean(true)]),
                },
                None,
            )
            .expect("scan disk ordered bool index"),
    );
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].row.values[0], Value::Boolean(false));
    assert_eq!(records[1].row.values[0], Value::Boolean(true));
}

#[test]
fn disk_ordered_index_serves_latest_uuid_range_scan_with_recheck() {
    let (storage, _dir) =
        storage_with_wal("disk_ordered_index_serves_latest_uuid_range_scan_with_recheck");
    let table_id = RelationId::new(35);
    let index_id = IndexId::new(35);
    create_uuid_table(&storage, table_id);
    create_unique_index(&storage, table_id, index_id);

    fn uuid(first_four: [u8; 4], tail: u8) -> [u8; 16] {
        let mut bytes = [tail; 16];
        bytes[..4].copy_from_slice(&first_four);
        bytes
    }

    insert_uuid_row(
        &storage,
        table_id,
        uuid([0x00, 0x00, 0x00, 0x01], 0),
        "before",
    );
    insert_uuid_row(
        &storage,
        table_id,
        uuid([0x10, 0x00, 0x00, 0x00], 0),
        "lower",
    );
    insert_uuid_row(
        &storage,
        table_id,
        uuid([0x10, 0x00, 0x00, 0x01], 0),
        "middle",
    );
    insert_uuid_row(
        &storage,
        table_id,
        uuid([0x10, 0x00, 0x00, 0x01], 0xFF),
        "false-positive",
    );
    insert_uuid_row(
        &storage,
        table_id,
        uuid([0x20, 0x00, 0x00, 0x00], 0),
        "after",
    );

    {
        let mut state = storage.write_state().expect("write state");
        let descriptor = state
            .indexes
            .get(&index_id)
            .expect("index exists")
            .descriptor
            .clone();
        state.indexes.insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Uuid(uuid([0x10, 0x00, 0x00, 0x00], 0))]),
                    upper: Bound::Included(vec![Value::Uuid(uuid([0x10, 0x00, 0x00, 0x01], 0x7F))]),
                },
                None,
            )
            .expect("scan disk ordered uuid range index"),
    );
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].row.values[1], Value::Text("lower".to_owned()));
    assert_eq!(records[1].row.values[1], Value::Text("middle".to_owned()));
}

#[test]
fn updates_with_btree_and_gin_indexes_keep_document_matches_after_autocommit_update() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2000);
    let btree_index_id = IndexId::new(2000);
    let gin_index_id = IndexId::new(2001);
    create_jsonb_table(&storage, table_id);
    create_index(&storage, table_id, btree_index_id);
    create_gin_json_index(&storage, table_id, gin_index_id);

    let tuple_id = storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![
                Value::Int(1),
                Value::Jsonb(serde_json::json!({"tag": "blue", "kind": "node"})),
            ]),
        )
        .expect("insert baseline row");

    storage
        .update(
            TxnId::default(),
            table_id,
            tuple_id,
            Row::new(vec![
                Value::Int(2),
                Value::Jsonb(serde_json::json!({"tag": "red", "kind": "leaf"})),
            ]),
        )
        .expect("update both indexed columns");

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch updated row")
        .expect("row exists");
    assert_eq!(
        updated,
        Row::new(vec![
            Value::Int(2),
            Value::Jsonb(serde_json::json!({"tag": "red", "kind": "leaf"})),
        ])
    );

    let red_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "red"}),
            )
            .expect("search red token"),
    );
    assert_eq!(red_matches.len(), 1);
    assert_eq!(red_matches[0].row, updated);

    let blue_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "blue"}),
            )
            .expect("search old blue token"),
    );
    assert_eq!(blue_matches.len(), 0);
}

#[test]
fn updates_with_btree_and_gin_indexes_keep_document_matches_in_split_phase_update() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2002);
    let btree_index_id = IndexId::new(2002);
    let gin_index_id = IndexId::new(2003);
    create_jsonb_table(&storage, table_id);
    create_index(&storage, table_id, btree_index_id);
    create_gin_json_index(&storage, table_id, gin_index_id);

    let tuple_id = storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![
                Value::Int(3),
                Value::Jsonb(serde_json::json!({"tag": "green", "kind": "node"})),
            ]),
        )
        .expect("insert baseline row");

    let txn = TxnId::new(9001);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin split-phase txn");

    let precheck = storage
        .split_phase_update_precheck(
            txn,
            table_id,
            tuple_id,
            &Row::new(vec![
                Value::Int(4),
                Value::Jsonb(serde_json::json!({"tag": "yellow", "kind": "leaf"})),
            ]),
        )
        .expect("split-phase update precheck");
    storage
        .split_phase_update_lock(txn, &precheck)
        .expect("split-phase update lock");
    storage
        .split_phase_update_apply(
            txn,
            &precheck,
            Row::new(vec![
                Value::Int(4),
                Value::Jsonb(serde_json::json!({"tag": "yellow", "kind": "leaf"})),
            ]),
        )
        .expect("split-phase update apply");
    storage
        .commit_txn(txn, txn.get())
        .expect("commit split-phase txn");

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch updated split-phase row")
        .expect("row exists");
    assert_eq!(
        updated,
        Row::new(vec![
            Value::Int(4),
            Value::Jsonb(serde_json::json!({"tag": "yellow", "kind": "leaf"})),
        ])
    );

    let yellow_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "yellow"}),
            )
            .expect("search yellow token"),
    );
    assert_eq!(yellow_matches.len(), 1);
    assert_eq!(yellow_matches[0].row, updated);

    let green_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "green"}),
            )
            .expect("search old green token"),
    );
    assert_eq!(green_matches.len(), 0);
}

fn create_jsonb_table(storage: &InMemoryStorage, table_id: RelationId) {
    let descriptor = TableStorageDescriptor {
        table_id,
        columns: vec![
            StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Int,
                nullable: false,
            },
            StorageColumn {
                column_id: ColumnId::new(2),
                data_type: DataType::Jsonb,
                nullable: true,
            },
        ],
        primary_key: None,
        shard_config: None,
    };
    storage
        .create_table_storage(TxnId::default(), &descriptor)
        .expect("create jsonb table");
}

fn create_vector_payload_table(storage: &InMemoryStorage, table_id: RelationId) {
    let descriptor = TableStorageDescriptor {
        table_id,
        columns: vec![
            StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Int,
                nullable: false,
            },
            StorageColumn {
                column_id: ColumnId::new(2),
                data_type: DataType::Vector {
                    dims: 3,
                    element_type: aiondb_core::VectorElementType::Float32,
                },
                nullable: false,
            },
        ],
        primary_key: None,
        shard_config: None,
    };
    storage
        .create_table_storage(TxnId::default(), &descriptor)
        .expect("create vector payload table");
}

fn create_hnsw_index(storage: &InMemoryStorage, table_id: RelationId, index_id: IndexId) {
    let mut descriptor = test_index_descriptor(index_id, table_id);
    descriptor.key_columns = vec![IndexKeyColumn {
        column_id: ColumnId::new(2),
        descending: false,
        nulls_first: false,
    }];
    storage
        .create_index_storage(TxnId::default(), &descriptor)
        .expect("create hnsw index");
}

fn create_gin_json_index(storage: &InMemoryStorage, table_id: RelationId, index_id: IndexId) {
    let mut descriptor = test_index_descriptor(index_id, table_id);
    descriptor.gin = true;
    descriptor.key_columns = vec![IndexKeyColumn {
        column_id: ColumnId::new(2),
        descending: false,
        nulls_first: false,
    }];
    storage
        .create_index_storage(TxnId::default(), &descriptor)
        .expect("create gin index");
}

#[test]
fn updates_of_non_indexed_columns_do_not_change_btree_or_gin_membership() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2004);
    let btree_index_id = IndexId::new(2004);
    let gin_index_id = IndexId::new(2005);
    create_jsonb_payload_table(&storage, table_id);
    create_index(&storage, table_id, btree_index_id);
    create_gin_json_index(&storage, table_id, gin_index_id);

    let tuple_id = storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![
                Value::Int(10),
                Value::Jsonb(serde_json::json!({"tag": "red", "kind": "root"})),
                Value::Text("payload-0".to_owned()),
            ]),
        )
        .expect("insert row");

    storage
        .update(
            TxnId::default(),
            table_id,
            tuple_id,
            Row::new(vec![
                Value::Int(10),
                Value::Jsonb(serde_json::json!({"tag": "red", "kind": "root"})),
                Value::Text("payload-1".to_owned()),
            ]),
        )
        .expect("update unindexed column only");

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch row after update")
        .expect("row exists");
    assert_eq!(
        updated,
        Row::new(vec![
            Value::Int(10),
            Value::Jsonb(serde_json::json!({"tag": "red", "kind": "root"})),
            Value::Text("payload-1".to_owned())
        ])
    );

    let index_rows = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                btree_index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(10)]),
                    upper: Bound::Included(vec![Value::Int(10)]),
                },
                None,
            )
            .expect("scan btree after update"),
    );
    assert_eq!(index_rows.len(), 1);
    assert_eq!(index_rows[0].row, updated);

    let red_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "red"}),
            )
            .expect("search red token"),
    );
    assert_eq!(red_matches.len(), 1);
    assert_eq!(red_matches[0].row, updated);
}

#[test]
fn split_phase_update_of_non_indexed_columns_keeps_index_membership_stable() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2006);
    let btree_index_id = IndexId::new(2006);
    let gin_index_id = IndexId::new(2007);
    create_jsonb_payload_table(&storage, table_id);
    create_index(&storage, table_id, btree_index_id);
    create_gin_json_index(&storage, table_id, gin_index_id);

    let tuple_id = insert_jsonb_payload_row(
        &storage,
        table_id,
        11,
        serde_json::json!({"tag":"blue","kind":"leaf"}),
        "alpha",
    );

    let txn = TxnId::new(9002);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin split-phase txn");

    let precheck = storage
        .split_phase_update_precheck(
            txn,
            table_id,
            tuple_id,
            &Row::new(vec![
                Value::Int(11),
                Value::Jsonb(serde_json::json!({"tag":"blue","kind":"leaf"})),
                Value::Text("beta".to_owned()),
            ]),
        )
        .expect("split-phase precheck non-indexed update");
    storage
        .split_phase_update_lock(txn, &precheck)
        .expect("split-phase lock non-indexed update");
    storage
        .split_phase_update_apply(
            txn,
            &precheck,
            Row::new(vec![
                Value::Int(11),
                Value::Jsonb(serde_json::json!({"tag":"blue","kind":"leaf"})),
                Value::Text("beta".to_owned()),
            ]),
        )
        .expect("split-phase apply non-indexed update");
    storage
        .commit_txn(txn, txn.get())
        .expect("commit split-phase txn");

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch updated row")
        .expect("row exists");
    assert_eq!(
        updated,
        Row::new(vec![
            Value::Int(11),
            Value::Jsonb(serde_json::json!({"tag":"blue","kind":"leaf"})),
            Value::Text("beta".to_owned())
        ])
    );

    let index_rows = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                btree_index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(11)]),
                    upper: Bound::Included(vec![Value::Int(11)]),
                },
                None,
            )
            .expect("scan btree after split-phase non-indexed update"),
    );
    assert_eq!(index_rows.len(), 1);
    assert_eq!(index_rows[0].row, updated);

    let blue_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "blue"}),
            )
            .expect("search blue token after split-phase non-indexed update"),
    );
    assert_eq!(blue_matches.len(), 1);
    assert_eq!(blue_matches[0].row, updated);
}

#[test]
fn split_phase_multiple_updates_keep_indexes_consistent() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2009);
    let btree_index_id = IndexId::new(2010);
    let gin_index_id = IndexId::new(2011);
    create_jsonb_payload_table(&storage, table_id);
    create_index(&storage, table_id, btree_index_id);
    create_gin_json_index(&storage, table_id, gin_index_id);

    let tuple_id = insert_jsonb_payload_row(
        &storage,
        table_id,
        30,
        serde_json::json!({"tag": "green", "kind": "node"}),
        "payload-0",
    );

    let txn = TxnId::new(9003);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin split-phase txn");

    let first_precheck = storage
        .split_phase_update_precheck(
            txn,
            table_id,
            tuple_id,
            &Row::new(vec![
                Value::Int(31),
                Value::Jsonb(serde_json::json!({"tag": "red", "kind": "leaf"})),
                Value::Text("payload-0".to_owned()),
            ]),
        )
        .expect("split-phase indexed precheck");
    storage
        .split_phase_update_lock(txn, &first_precheck)
        .expect("split-phase indexed update lock");
    storage
        .split_phase_update_apply(
            txn,
            &first_precheck,
            Row::new(vec![
                Value::Int(31),
                Value::Jsonb(serde_json::json!({"tag": "red", "kind": "leaf"})),
                Value::Text("payload-0".to_owned()),
            ]),
        )
        .expect("split-phase indexed update apply");

    let second_precheck = storage
        .split_phase_update_precheck(
            txn,
            table_id,
            tuple_id,
            &Row::new(vec![
                Value::Int(31),
                Value::Jsonb(serde_json::json!({"tag": "red", "kind": "leaf"})),
                Value::Text("payload-1".to_owned()),
            ]),
        )
        .expect("split-phase non-indexed precheck");
    storage
        .split_phase_update_lock(txn, &second_precheck)
        .expect("split-phase non-indexed update lock");
    storage
        .split_phase_update_apply(
            txn,
            &second_precheck,
            Row::new(vec![
                Value::Int(31),
                Value::Jsonb(serde_json::json!({"tag": "red", "kind": "leaf"})),
                Value::Text("payload-1".to_owned()),
            ]),
        )
        .expect("split-phase non-indexed update apply");
    storage
        .commit_txn(txn, txn.get())
        .expect("commit split-phase txn");

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch updated split-phase row")
        .expect("row exists");
    assert_eq!(
        updated,
        Row::new(vec![
            Value::Int(31),
            Value::Jsonb(serde_json::json!({"tag": "red", "kind": "leaf"})),
            Value::Text("payload-1".to_owned())
        ])
    );

    let red_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "red"}),
            )
            .expect("search red token"),
    );
    assert_eq!(red_matches.len(), 1);
    assert_eq!(red_matches[0].row, updated);

    let old_green_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "green"}),
            )
            .expect("search old green token"),
    );
    assert_eq!(old_green_matches.len(), 0);

    let index_rows = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                btree_index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(31)]),
                    upper: Bound::Included(vec![Value::Int(31)]),
                },
                None,
            )
            .expect("scan btree after split-phase multi-update"),
    );
    assert_eq!(index_rows.len(), 1);
    assert_eq!(index_rows[0].row, updated);
}

#[test]
fn split_phase_multiple_updates_indexed_after_nonindexed() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2012);
    let btree_index_id = IndexId::new(2013);
    let gin_index_id = IndexId::new(2014);
    create_jsonb_payload_table(&storage, table_id);
    create_index(&storage, table_id, btree_index_id);
    create_gin_json_index(&storage, table_id, gin_index_id);

    let tuple_id = insert_jsonb_payload_row(
        &storage,
        table_id,
        40,
        serde_json::json!({"tag": "blue", "kind": "node"}),
        "payload-0",
    );

    let txn = TxnId::new(9004);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin split-phase txn");

    let first_precheck = storage
        .split_phase_update_precheck(
            txn,
            table_id,
            tuple_id,
            &Row::new(vec![
                Value::Int(40),
                Value::Jsonb(serde_json::json!({"tag": "blue", "kind": "node"})),
                Value::Text("payload-1".to_owned()),
            ]),
        )
        .expect("split-phase non-indexed precheck");
    storage
        .split_phase_update_lock(txn, &first_precheck)
        .expect("split-phase non-indexed update lock");
    storage
        .split_phase_update_apply(
            txn,
            &first_precheck,
            Row::new(vec![
                Value::Int(40),
                Value::Jsonb(serde_json::json!({"tag": "blue", "kind": "node"})),
                Value::Text("payload-1".to_owned()),
            ]),
        )
        .expect("split-phase non-indexed apply");

    let second_precheck = storage
        .split_phase_update_precheck(
            txn,
            table_id,
            tuple_id,
            &Row::new(vec![
                Value::Int(41),
                Value::Jsonb(serde_json::json!({"tag": "yellow", "kind": "leaf"})),
                Value::Text("payload-1".to_owned()),
            ]),
        )
        .expect("split-phase indexed precheck");
    storage
        .split_phase_update_lock(txn, &second_precheck)
        .expect("split-phase indexed update lock");
    storage
        .split_phase_update_apply(
            txn,
            &second_precheck,
            Row::new(vec![
                Value::Int(41),
                Value::Jsonb(serde_json::json!({"tag": "yellow", "kind": "leaf"})),
                Value::Text("payload-1".to_owned()),
            ]),
        )
        .expect("split-phase indexed apply");
    storage
        .commit_txn(txn, txn.get())
        .expect("commit split-phase txn");

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch updated split-phase row")
        .expect("row exists");
    assert_eq!(
        updated,
        Row::new(vec![
            Value::Int(41),
            Value::Jsonb(serde_json::json!({"tag": "yellow", "kind": "leaf"})),
            Value::Text("payload-1".to_owned())
        ])
    );

    let yellow_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "yellow"}),
            )
            .expect("search yellow token"),
    );
    assert_eq!(yellow_matches.len(), 1);
    assert_eq!(yellow_matches[0].row, updated);

    let index_rows = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                btree_index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(41)]),
                    upper: Bound::Included(vec![Value::Int(41)]),
                },
                None,
            )
            .expect("scan btree after split-phase index change second"),
    );
    assert_eq!(index_rows.len(), 1);
    assert_eq!(index_rows[0].row, updated);
}

#[test]
fn split_phase_multiple_updates_with_stress_pattern_keeps_indexes_consistent() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2018);
    let btree_index_id = IndexId::new(2019);
    let gin_index_id = IndexId::new(2020);
    create_jsonb_payload_table(&storage, table_id);
    create_index(&storage, table_id, btree_index_id);
    create_gin_json_index(&storage, table_id, gin_index_id);

    let tuple_id = insert_jsonb_payload_row(
        &storage,
        table_id,
        50,
        serde_json::json!({"tag": "blue", "kind": "node"}),
        "payload-0",
    );

    let txn = TxnId::new(9005);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin split-phase txn");

    let mut expected_id = 50;
    let mut expected_tag = "blue".to_owned();
    for i in 0..7 {
        let update_id = if i % 2 == 0 {
            expected_id += 1;
            expected_id
        } else {
            expected_id
        };

        let expected_kind = if i % 3 == 0 { "leaf" } else { "node" };
        expected_tag = if i == 1 {
            "yellow".to_owned()
        } else if i > 1 && i % 2 == 1 {
            "red".to_owned()
        } else {
            "blue".to_owned()
        };

        let payload = format!("payload-{i}");
        let precheck = storage
            .split_phase_update_precheck(
                txn,
                table_id,
                tuple_id,
                &Row::new(vec![
                    Value::Int(update_id),
                    Value::Jsonb(serde_json::json!({"tag": expected_tag, "kind": expected_kind})),
                    Value::Text(payload.clone()),
                ]),
            )
            .expect("split-phase stress precheck");
        storage
            .split_phase_update_lock(txn, &precheck)
            .expect("split-phase stress lock");
        storage
            .split_phase_update_apply(
                txn,
                &precheck,
                Row::new(vec![
                    Value::Int(update_id),
                    Value::Jsonb(serde_json::json!({"tag": expected_tag, "kind": expected_kind})),
                    Value::Text(payload),
                ]),
            )
            .expect("split-phase stress apply");
    }
    storage
        .commit_txn(txn, txn.get())
        .expect("commit split-phase stress txn");

    let expected = Row::new(vec![
        Value::Int(expected_id),
        Value::Jsonb(
            serde_json::json!({"tag": expected_tag.clone(), "kind": if 6 % 3 == 0 { "leaf" } else { "node" }}),
        ),
        Value::Text("payload-6".to_owned()),
    ]);
    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch updated row after stress updates")
        .expect("row exists");
    assert_eq!(updated, expected);

    let index_rows = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                btree_index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(expected_id)]),
                    upper: Bound::Included(vec![Value::Int(expected_id)]),
                },
                None,
            )
            .expect("scan btree after stress updates"),
    );
    assert_eq!(index_rows.len(), 1);
    assert_eq!(index_rows[0].row, expected);

    let final_token_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": expected_tag}),
            )
            .expect("search final token after stress updates"),
    );
    assert_eq!(final_token_matches.len(), 1);
    assert_eq!(final_token_matches[0].row, expected);

    let yellow_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "yellow"}),
            )
            .expect("search yellow token"),
    );
    assert_eq!(yellow_matches.len(), 0);

    let red_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "red"}),
            )
            .expect("search red token"),
    );
    assert_eq!(red_matches.len(), 0);
}

#[test]
fn split_phase_stress_updates_alternating_indexed_and_non_indexed_columns() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2021);
    let btree_index_id = IndexId::new(2022);
    let gin_index_id = IndexId::new(2023);
    create_jsonb_payload_table(&storage, table_id);
    create_index(&storage, table_id, btree_index_id);
    create_gin_json_index(&storage, table_id, gin_index_id);

    let tuple_id = insert_jsonb_payload_row(
        &storage,
        table_id,
        10,
        serde_json::json!({"tag": "blue", "kind": "leaf"}),
        "payload-0",
    );

    let txn = TxnId::new(9012);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin split-phase txn");

    let mut expected_id = 10;
    let mut expected_tag = "blue".to_owned();
    let mut expected_kind = "leaf".to_owned();
    for step in 0..18 {
        let even_step = step % 2 == 0;
        let new_id = if even_step {
            expected_id += 1;
            expected_id
        } else {
            expected_id
        };

        if even_step {
            expected_tag = "blue".to_owned();
        } else {
            expected_tag = if step % 3 == 0 { "amber" } else { "red" }.to_owned();
        }
        expected_kind = if step % 5 == 0 { "root" } else { "leaf" }.to_owned();
        let payload = format!("payload-{step}");
        let row = Row::new(vec![
            Value::Int(new_id),
            Value::Jsonb(serde_json::json!({"tag": expected_tag, "kind": expected_kind})),
            Value::Text(payload.clone()),
        ]);

        let precheck = storage
            .split_phase_update_precheck(txn, table_id, tuple_id, &row)
            .expect("split-phase alternation precheck");
        storage
            .split_phase_update_lock(txn, &precheck)
            .expect("split-phase alternation lock");
        storage
            .split_phase_update_apply(txn, &precheck, row)
            .expect("split-phase alternation apply");
    }

    storage
        .commit_txn(txn, txn.get())
        .expect("commit split-phase alternating txn");

    let expected = Row::new(vec![
        Value::Int(expected_id),
        Value::Jsonb(serde_json::json!({"tag": expected_tag, "kind": expected_kind})),
        Value::Text("payload-17".to_owned()),
    ]);

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch final row after alternating updates")
        .expect("row exists");
    assert_eq!(updated, expected);

    let btree_matches = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                btree_index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(expected_id)]),
                    upper: Bound::Included(vec![Value::Int(expected_id)]),
                },
                None,
            )
            .expect("scan btree after alternating updates"),
    );
    assert_eq!(btree_matches.len(), 1);
    assert_eq!(btree_matches[0].row, expected);

    let matches_tag = |tag: &str| {
        collect_stream(
            storage
                .gin_containment_search(
                    TxnId::default(),
                    &snapshot(),
                    gin_index_id,
                    &serde_json::json!({"tag": tag}),
                )
                .expect("gin alternation search"),
        )
    };

    let final_tag_matches = matches_tag(&expected_tag);
    assert_eq!(final_tag_matches.len(), 1);
    assert_eq!(final_tag_matches[0].row, expected);

    let blue_matches = matches_tag("blue");
    assert_eq!(blue_matches.len(), usize::from(expected_tag == "blue"));

    let amber_matches = matches_tag("amber");
    assert_eq!(amber_matches.len(), usize::from(expected_tag == "amber"));

    let red_matches = matches_tag("red");
    assert_eq!(red_matches.len(), usize::from(expected_tag == "red"));
}

#[test]
fn split_phase_stress_updates_non_indexed_columns_only() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2024);
    let btree_index_id = IndexId::new(2025);
    let gin_index_id = IndexId::new(2026);
    create_jsonb_payload_table(&storage, table_id);
    create_index(&storage, table_id, btree_index_id);
    create_gin_json_index(&storage, table_id, gin_index_id);

    let tuple_id = insert_jsonb_payload_row(
        &storage,
        table_id,
        31,
        serde_json::json!({"tag":"green","kind":"leaf"}),
        "payload-0",
    );

    let txn = TxnId::new(9014);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin split-phase non-indexed txn");

    for step in 0..25 {
        let row = Row::new(vec![
            Value::Int(31),
            Value::Jsonb(serde_json::json!({"tag":"green","kind":"leaf"})),
            Value::Text(format!("payload-{step}")),
        ]);
        let precheck = storage
            .split_phase_update_precheck(txn, table_id, tuple_id, &row)
            .expect("split-phase non-indexed precheck");
        storage
            .split_phase_update_lock(txn, &precheck)
            .expect("split-phase non-indexed lock");
        storage
            .split_phase_update_apply(txn, &precheck, row)
            .expect("split-phase non-indexed apply");
    }

    storage
        .commit_txn(txn, txn.get())
        .expect("commit split-phase non-indexed txn");

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch final row after non-indexed stress updates")
        .expect("row exists");
    assert_eq!(
        updated,
        Row::new(vec![
            Value::Int(31),
            Value::Jsonb(serde_json::json!({"tag":"green","kind":"leaf"})),
            Value::Text("payload-24".to_owned())
        ])
    );

    let btree_matches = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                btree_index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(31)]),
                    upper: Bound::Included(vec![Value::Int(31)]),
                },
                None,
            )
            .expect("scan btree after non-indexed stress updates"),
    );
    assert_eq!(btree_matches.len(), 1);
    assert_eq!(btree_matches[0].row, updated);

    let green_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "green"}),
            )
            .expect("gin non-indexed stress search"),
    );
    assert_eq!(green_matches.len(), 1);
    assert_eq!(green_matches[0].row, updated);
}

#[test]
fn split_phase_repeated_updates_on_indexless_table_keep_precheck_light() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2040);
    create_jsonb_payload_table(&storage, table_id);

    let tuple_id = insert_jsonb_payload_row(
        &storage,
        table_id,
        77,
        serde_json::json!({"tag":"base","kind":"leaf"}),
        "payload-0",
    );

    let txn = TxnId::new(9020);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin split-phase indexless stress txn");

    for step in 0..16 {
        let row = Row::new(vec![
            Value::Int(77),
            Value::Jsonb(serde_json::json!({"tag":"base","kind":"leaf"})),
            Value::Text(format!("payload-{step}")),
        ]);
        let precheck = storage
            .split_phase_update_precheck(txn, table_id, tuple_id, &row)
            .expect("split-phase indexless precheck");
        assert!(precheck.split_phase_index_update_set.is_none());
        assert!(precheck.split_phase_hnsw_index_ids.is_none());
        assert!(!precheck.pending_indexed_columns_changed);
        storage
            .split_phase_update_lock(txn, &precheck)
            .expect("split-phase indexless lock");
        storage
            .split_phase_update_apply(txn, &precheck, row)
            .expect("split-phase indexless apply");
    }

    storage
        .commit_txn(txn, txn.get())
        .expect("commit split-phase indexless stress txn");

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch final row for indexless stress")
        .expect("row exists");
    assert_eq!(
        updated,
        Row::new(vec![
            Value::Int(77),
            Value::Jsonb(serde_json::json!({"tag":"base","kind":"leaf"})),
            Value::Text("payload-15".to_owned()),
        ])
    );
}

#[test]
fn split_phase_many_non_indexed_updates_skip_index_plan_on_indexed_table() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2041);
    let btree_index_id = IndexId::new(2042);
    let gin_index_id = IndexId::new(2043);
    create_jsonb_payload_table(&storage, table_id);
    create_index(&storage, table_id, btree_index_id);
    create_gin_json_index(&storage, table_id, gin_index_id);

    let tuple_id = insert_jsonb_payload_row(
        &storage,
        table_id,
        99,
        serde_json::json!({"tag":"base","kind":"leaf"}),
        "payload-0",
    );

    let txn = TxnId::new(9021);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin split-phase indexed table stress txn");

    for step in 0..20 {
        let row = Row::new(vec![
            Value::Int(99),
            Value::Jsonb(serde_json::json!({"tag":"base","kind":"leaf"})),
            Value::Text(format!("payload-{step}")),
        ]);
        let precheck = storage
            .split_phase_update_precheck(txn, table_id, tuple_id, &row)
            .expect("split-phase indexed table precheck");
        assert!(precheck.split_phase_index_update_set.is_none());
        assert!(precheck.split_phase_hnsw_index_ids.is_none());
        assert!(!precheck.pending_indexed_columns_changed);
        storage
            .split_phase_update_lock(txn, &precheck)
            .expect("split-phase indexed table lock");
        storage
            .split_phase_update_apply(txn, &precheck, row)
            .expect("split-phase indexed table apply");
    }

    storage
        .commit_txn(txn, txn.get())
        .expect("commit split-phase indexed table stress txn");

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch final row after indexed table non-indexed stress")
        .expect("row exists");
    assert_eq!(
        updated,
        Row::new(vec![
            Value::Int(99),
            Value::Jsonb(serde_json::json!({"tag":"base","kind":"leaf"})),
            Value::Text("payload-19".to_owned()),
        ])
    );

    let btree_matches = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                btree_index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(99)]),
                    upper: Bound::Included(vec![Value::Int(99)]),
                },
                None,
            )
            .expect("scan btree after indexed table stress non-indexed updates"),
    );
    assert_eq!(btree_matches.len(), 1);
    assert_eq!(btree_matches[0].row, updated);

    let base_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "base"}),
            )
            .expect("search base tag after indexed table stress"),
    );
    assert_eq!(base_matches.len(), 1);
    assert_eq!(base_matches[0].row, updated);
}

#[test]
fn split_phase_index_update_on_hnsw_table_targets_only_changed_hnsw_indexes() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2042);
    let hnsw_index_id = IndexId::new(2043);
    create_vector_payload_table(&storage, table_id);
    create_hnsw_index(&storage, table_id, hnsw_index_id);

    let tuple_id = storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![
                Value::Int(10),
                Value::Vector(aiondb_core::VectorValue {
                    dims: 3,
                    values: vec![0.0, 1.0, 0.0],
                }),
            ]),
        )
        .expect("insert baseline vector row");

    let txn = TxnId::new(9022);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin split-phase vector txn");

    let non_hnsw_update = Row::new(vec![
        Value::Int(11),
        Value::Vector(aiondb_core::VectorValue {
            dims: 3,
            values: vec![0.0, 1.0, 0.0],
        }),
    ]);
    let precheck = storage
        .split_phase_update_precheck(txn, table_id, tuple_id, &non_hnsw_update)
        .expect("split-phase non-hnsw-column precheck");
    assert!(precheck.split_phase_hnsw_index_ids.is_none());
    assert!(precheck.split_phase_index_update_set.is_none());
    assert!(!precheck.pending_indexed_columns_changed);
    storage
        .split_phase_update_lock(txn, &precheck)
        .expect("split-phase non-hnsw-column lock");
    storage
        .split_phase_update_apply(txn, &precheck, non_hnsw_update)
        .expect("split-phase non-hnsw-column apply");

    let hnsw_update = Row::new(vec![
        Value::Int(11),
        Value::Vector(aiondb_core::VectorValue {
            dims: 3,
            values: vec![0.1, 0.9, 0.1],
        }),
    ]);
    let precheck = storage
        .split_phase_update_precheck(txn, table_id, tuple_id, &hnsw_update)
        .expect("split-phase hnsw-column precheck");
    assert_eq!(
        precheck.split_phase_hnsw_index_ids,
        Some(vec![hnsw_index_id]),
        "precheck should include only changed committed HNSW index"
    );
    storage
        .split_phase_update_lock(txn, &precheck)
        .expect("split-phase hnsw-column lock");
    storage
        .split_phase_update_apply(txn, &precheck, hnsw_update)
        .expect("split-phase hnsw-column apply");

    storage
        .commit_txn(txn, txn.get())
        .expect("commit split-phase vector txn");

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch vector row after split-phase updates")
        .expect("row exists");
    assert_eq!(
        updated,
        Row::new(vec![
            Value::Int(11),
            Value::Vector(aiondb_core::VectorValue {
                dims: 3,
                values: vec![0.1, 0.9, 0.1],
            }),
        ]),
    );
}

#[test]
fn split_phase_update_on_hnsw_and_btree_targeted_tables_does_not_touch_hnsw_for_btree_only_change()
{
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2044);
    let btree_index_id = IndexId::new(2045);
    let hnsw_index_id = IndexId::new(2046);

    create_vector_payload_table(&storage, table_id);
    create_index(&storage, table_id, btree_index_id);
    create_hnsw_index(&storage, table_id, hnsw_index_id);

    let tuple_id = storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![
                Value::Int(10),
                Value::Vector(aiondb_core::VectorValue {
                    dims: 3,
                    values: vec![0.0, 1.0, 0.0],
                }),
            ]),
        )
        .expect("insert baseline vector row");

    let txn = TxnId::new(9023);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin split-phase vector txn");

    let btree_only_update = Row::new(vec![
        Value::Int(11),
        Value::Vector(aiondb_core::VectorValue {
            dims: 3,
            values: vec![0.0, 1.0, 0.0],
        }),
    ]);
    let precheck = storage
        .split_phase_update_precheck(txn, table_id, tuple_id, &btree_only_update)
        .expect("split-phase btree-only precheck");
    let Some(update_set) = precheck.split_phase_index_update_set.as_ref() else {
        panic!("split-phase update set should be present for indexed btree change");
    };
    assert!(update_set.btree_index_ids.contains(&btree_index_id));
    assert!(!update_set.hnsw_index_ids.contains(&hnsw_index_id));
    assert!(precheck.split_phase_hnsw_index_ids.is_none());

    storage
        .split_phase_update_lock(txn, &precheck)
        .expect("split-phase btree-only lock");
    storage
        .split_phase_update_apply(txn, &precheck, btree_only_update)
        .expect("split-phase btree-only apply");

    storage
        .commit_txn(txn, txn.get())
        .expect("commit split-phase vector txn");

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch vector row after split-phase btree-only update")
        .expect("row exists");
    assert_eq!(
        updated,
        Row::new(vec![
            Value::Int(11),
            Value::Vector(aiondb_core::VectorValue {
                dims: 3,
                values: vec![0.0, 1.0, 0.0],
            }),
        ]),
    );
}

#[test]
fn split_phase_precheck_targets_only_pending_indexes_on_changed_columns_and_table() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(3060);
    let other_table_id = RelationId::new(3061);
    let btree_index_id = IndexId::new(3062);
    let hnsw_index_id = IndexId::new(3063);
    let other_index_id = IndexId::new(3064);

    create_vector_payload_table(&storage, table_id);
    create_table(&storage, other_table_id);

    let tuple_id = storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![
                Value::Int(10),
                Value::Vector(aiondb_core::VectorValue {
                    dims: 3,
                    values: vec![0.0, 1.0, 0.0],
                }),
            ]),
        )
        .expect("insert vector row");

    let txn = TxnId::new(9028);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin split-phase pending-index txn");

    let mut hnsw_descriptor = test_index_descriptor(hnsw_index_id, table_id);
    hnsw_descriptor.key_columns = vec![IndexKeyColumn {
        column_id: ColumnId::new(2),
        descending: false,
        nulls_first: false,
    }];
    storage
        .create_index_storage(txn, &test_index_descriptor(btree_index_id, table_id))
        .expect("create pending btree index in txn");
    storage
        .create_index_storage(txn, &hnsw_descriptor)
        .expect("create pending hnsw index in txn");
    storage
        .create_index_storage(txn, &test_index_descriptor(other_index_id, other_table_id))
        .expect("create pending unrelated btree index in txn");

    let btree_only_update = Row::new(vec![
        Value::Int(11),
        Value::Vector(aiondb_core::VectorValue {
            dims: 3,
            values: vec![0.0, 1.0, 0.0],
        }),
    ]);
    let precheck_btree_only = storage
        .split_phase_update_precheck(txn, table_id, tuple_id, &btree_only_update)
        .expect("split-phase btree-only precheck with pending indexes");
    assert_eq!(
        precheck_btree_only.split_phase_pending_btree_index_ids,
        vec![btree_index_id],
    );
    assert!(precheck_btree_only
        .split_phase_pending_hnsw_index_ids
        .is_empty());
    assert!(precheck_btree_only
        .split_phase_pending_gin_index_ids
        .is_empty());
    assert!(precheck_btree_only.pending_indexed_columns_changed);

    storage
        .split_phase_update_lock(txn, &precheck_btree_only)
        .expect("split-phase btree-only lock");
    storage
        .split_phase_update_apply(txn, &precheck_btree_only, btree_only_update)
        .expect("split-phase btree-only apply");

    let hnsw_only_update = Row::new(vec![
        Value::Int(11),
        Value::Vector(aiondb_core::VectorValue {
            dims: 3,
            values: vec![0.0, 0.9, 0.1],
        }),
    ]);
    let precheck_hnsw_only = storage
        .split_phase_update_precheck(txn, table_id, tuple_id, &hnsw_only_update)
        .expect("split-phase hnsw-only precheck with pending indexes");
    assert!(precheck_hnsw_only
        .split_phase_pending_btree_index_ids
        .is_empty());
    assert_eq!(
        precheck_hnsw_only.split_phase_pending_hnsw_index_ids,
        vec![hnsw_index_id],
    );
    assert!(precheck_hnsw_only
        .split_phase_pending_gin_index_ids
        .is_empty());
    assert!(precheck_hnsw_only.pending_indexed_columns_changed);

    storage
        .split_phase_update_lock(txn, &precheck_hnsw_only)
        .expect("split-phase hnsw-only lock");
    storage
        .split_phase_update_apply(txn, &precheck_hnsw_only, hnsw_only_update)
        .expect("split-phase hnsw-only apply");

    storage
        .commit_txn(txn, txn.get())
        .expect("commit split-phase pending-index txn");

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch vector row after pending-index split-phase updates")
        .expect("row exists");
    assert_eq!(
        updated,
        Row::new(vec![
            Value::Int(11),
            Value::Vector(aiondb_core::VectorValue {
                dims: 3,
                values: vec![0.0, 0.9, 0.1],
            }),
        ])
    );
}

#[test]
fn split_phase_insert_precheck_targets_only_pending_indexes_on_table() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(3070);
    let other_table_id = RelationId::new(3071);
    let btree_index_id = IndexId::new(3072);
    let gin_index_id = IndexId::new(3073);
    let other_index_id = IndexId::new(3074);

    create_jsonb_payload_table(&storage, table_id);
    create_jsonb_payload_table(&storage, other_table_id);

    let txn = TxnId::new(9030);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin split-phase pending insert txn");

    storage
        .create_index_storage(txn, &test_index_descriptor(btree_index_id, table_id))
        .expect("create pending btree index in txn");
    let mut gin_descriptor = test_index_descriptor(gin_index_id, table_id);
    gin_descriptor.gin = true;
    gin_descriptor.key_columns = vec![IndexKeyColumn {
        column_id: ColumnId::new(2),
        descending: false,
        nulls_first: false,
    }];
    storage
        .create_index_storage(txn, &gin_descriptor)
        .expect("create pending gin index in txn");
    storage
        .create_index_storage(txn, &test_index_descriptor(other_index_id, other_table_id))
        .expect("create pending unrelated btree index in txn");

    let row = Row::new(vec![
        Value::Int(77),
        Value::Jsonb(serde_json::json!({"k":"v"})),
        Value::Text("payload".to_owned()),
    ]);
    let precheck = storage
        .split_phase_insert_precheck(txn, table_id, &row)
        .expect("split-phase insert precheck with pending indexes");
    assert_eq!(
        precheck.split_phase_pending_btree_index_ids,
        vec![btree_index_id],
    );
    assert!(precheck.split_phase_pending_hnsw_index_ids.is_empty());
    assert_eq!(
        precheck.split_phase_pending_gin_index_ids,
        vec![gin_index_id],
    );

    storage
        .split_phase_insert_lock(txn, &precheck)
        .expect("split-phase insert lock");
    let tuple_id = storage
        .split_phase_insert_apply(txn, &precheck, row.clone())
        .expect("split-phase insert apply");
    storage
        .commit_txn(txn, txn.get())
        .expect("commit split-phase pending insert txn");

    let fetched = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch inserted row")
        .expect("row exists");
    assert_eq!(fetched, row);
}

#[test]
fn split_phase_delete_precheck_targets_only_pending_indexes_on_table() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(3075);
    let other_table_id = RelationId::new(3076);
    let btree_index_id = IndexId::new(3077);
    let gin_index_id = IndexId::new(3078);
    let other_index_id = IndexId::new(3079);

    create_jsonb_payload_table(&storage, table_id);
    create_jsonb_payload_table(&storage, other_table_id);

    let tuple_id = storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![
                Value::Int(88),
                Value::Jsonb(serde_json::json!({"k":"v2"})),
                Value::Text("before-delete".to_owned()),
            ]),
        )
        .expect("insert row");

    let txn = TxnId::new(9031);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin split-phase pending delete txn");

    storage
        .create_index_storage(txn, &test_index_descriptor(btree_index_id, table_id))
        .expect("create pending btree index in txn");
    let mut gin_descriptor = test_index_descriptor(gin_index_id, table_id);
    gin_descriptor.gin = true;
    gin_descriptor.key_columns = vec![IndexKeyColumn {
        column_id: ColumnId::new(2),
        descending: false,
        nulls_first: false,
    }];
    storage
        .create_index_storage(txn, &gin_descriptor)
        .expect("create pending gin index in txn");
    storage
        .create_index_storage(txn, &test_index_descriptor(other_index_id, other_table_id))
        .expect("create pending unrelated btree index in txn");

    let precheck = storage
        .split_phase_delete_precheck(txn, table_id, tuple_id)
        .expect("split-phase delete precheck with pending indexes");
    assert_eq!(
        precheck.split_phase_pending_btree_index_ids,
        vec![btree_index_id],
    );
    assert!(precheck.split_phase_pending_hnsw_index_ids.is_empty());
    assert_eq!(
        precheck.split_phase_pending_gin_index_ids,
        vec![gin_index_id],
    );

    storage
        .split_phase_delete_lock(txn, &precheck)
        .expect("split-phase delete lock");
    storage
        .split_phase_delete_apply(txn, &precheck)
        .expect("split-phase delete apply");
    storage
        .commit_txn(txn, txn.get())
        .expect("commit split-phase pending delete txn");

    let fetched = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch deleted row");
    assert!(fetched.is_none());
}

#[test]
fn split_phase_preserves_index_plan_when_final_updates_are_non_indexed() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2029);
    let btree_index_id = IndexId::new(2030);
    let gin_index_id = IndexId::new(2031);
    create_jsonb_payload_table(&storage, table_id);
    create_index(&storage, table_id, btree_index_id);
    create_gin_json_index(&storage, table_id, gin_index_id);

    let tuple_id = insert_jsonb_payload_row(
        &storage,
        table_id,
        41,
        serde_json::json!({"tag":"blue","kind":"leaf"}),
        "payload-0",
    );

    let txn = TxnId::new(9015);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin split-phase plan reuse txn");

    let indexed_row = Row::new(vec![
        Value::Int(42),
        Value::Jsonb(serde_json::json!({"tag":"amber","kind":"root"})),
        Value::Text("payload-1".to_owned()),
    ]);
    let precheck = storage
        .split_phase_update_precheck(txn, table_id, tuple_id, &indexed_row)
        .expect("split-phase indexed precheck");
    storage
        .split_phase_update_lock(txn, &precheck)
        .expect("split-phase indexed lock");
    storage
        .split_phase_update_apply(txn, &precheck, indexed_row)
        .expect("split-phase indexed apply");

    for step in 0..25 {
        let row = Row::new(vec![
            Value::Int(42),
            Value::Jsonb(serde_json::json!({"tag":"amber","kind":"root"})),
            Value::Text(format!("payload-final-{step}")),
        ]);
        let precheck = storage
            .split_phase_update_precheck(txn, table_id, tuple_id, &row)
            .expect("split-phase terminal non-indexed precheck");
        storage
            .split_phase_update_lock(txn, &precheck)
            .expect("split-phase terminal non-indexed lock");
        storage
            .split_phase_update_apply(txn, &precheck, row)
            .expect("split-phase terminal non-indexed apply");
    }

    storage
        .commit_txn(txn, 9015)
        .expect("commit split-phase plan reuse txn");

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch split-phase plan reuse final row")
        .expect("row exists");
    assert_eq!(
        updated,
        Row::new(vec![
            Value::Int(42),
            Value::Jsonb(serde_json::json!({"tag":"amber","kind":"root"})),
            Value::Text("payload-final-24".to_owned()),
        ])
    );

    let btree_matches = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                btree_index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(42)]),
                    upper: Bound::Included(vec![Value::Int(42)]),
                },
                None,
            )
            .expect("scan btree final key"),
    );
    assert_eq!(btree_matches.len(), 1);
    assert_eq!(btree_matches[0].row, updated);

    let old_btree_matches = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                btree_index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(41)]),
                    upper: Bound::Included(vec![Value::Int(41)]),
                },
                None,
            )
            .expect("scan btree old key"),
    );
    assert_eq!(old_btree_matches.len(), 0);

    let amber_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "amber"}),
            )
            .expect("gin amber search"),
    );
    assert_eq!(amber_matches.len(), 1);
    assert_eq!(amber_matches[0].row, updated);
}

#[test]
fn split_phase_repeated_txn_cycles_keep_index_membership_under_non_indexed_tail() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2032);
    let btree_index_id = IndexId::new(2033);
    let gin_index_id = IndexId::new(2034);
    create_jsonb_payload_table(&storage, table_id);
    create_index(&storage, table_id, btree_index_id);
    create_gin_json_index(&storage, table_id, gin_index_id);

    let tuple_id = insert_jsonb_payload_row(
        &storage,
        table_id,
        500,
        serde_json::json!({"tag":"red","kind":"leaf"}),
        "payload-start",
    );

    for cycle in 0..4 {
        let txn = TxnId::new(9100 + cycle as u64);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .expect("begin split-phase repeat txn");

        let indexed_row = Row::new(vec![
            Value::Int(500 + cycle + 1),
            Value::Jsonb(serde_json::json!({"tag":"cycle","kind":"leaf"})),
            Value::Text(format!("payload-{cycle}-indexed")),
        ]);
        let precheck = storage
            .split_phase_update_precheck(txn, table_id, tuple_id, &indexed_row)
            .expect("split-phase repeat indexed precheck");
        storage
            .split_phase_update_lock(txn, &precheck)
            .expect("split-phase repeat indexed lock");
        storage
            .split_phase_update_apply(txn, &precheck, indexed_row)
            .expect("split-phase repeat indexed apply");

        for step in 0..8 {
            let row = Row::new(vec![
                Value::Int(500 + cycle + 1),
                Value::Jsonb(serde_json::json!({"tag":"cycle","kind":"leaf"})),
                Value::Text(format!("payload-{cycle}-{step}")),
            ]);
            let precheck = storage
                .split_phase_update_precheck(txn, table_id, tuple_id, &row)
                .expect("split-phase repeat non-indexed precheck");
            storage
                .split_phase_update_lock(txn, &precheck)
                .expect("split-phase repeat non-indexed lock");
            storage
                .split_phase_update_apply(txn, &precheck, row)
                .expect("split-phase repeat non-indexed apply");
        }

        storage
            .commit_txn(txn, 9100 + cycle as u64)
            .expect("commit split-phase repeat txn");
    }

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch repeated cycle final row")
        .expect("row exists");
    let expected = Row::new(vec![
        Value::Int(504),
        Value::Jsonb(serde_json::json!({"tag":"cycle","kind":"leaf"})),
        Value::Text("payload-3-7".to_owned()),
    ]);
    assert_eq!(updated, expected);

    let index_rows = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                btree_index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(504)]),
                    upper: Bound::Included(vec![Value::Int(504)]),
                },
                None,
            )
            .expect("scan btree after repeated cycles"),
    );
    assert_eq!(index_rows.len(), 1);
    assert_eq!(index_rows[0].row, expected);

    let cycle_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "cycle"}),
            )
            .expect("search cycle tag"),
    );
    assert_eq!(cycle_matches.len(), 1);
    assert_eq!(cycle_matches[0].row, expected);
}

#[test]
fn tx_update_of_non_indexed_columns_keeps_index_membership_after_commit() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2008);
    let btree_index_id = IndexId::new(2008);
    let gin_index_id = IndexId::new(2009);
    create_jsonb_payload_table(&storage, table_id);
    create_index(&storage, table_id, btree_index_id);
    create_gin_json_index(&storage, table_id, gin_index_id);

    let tuple_id = insert_jsonb_payload_row(
        &storage,
        table_id,
        21,
        serde_json::json!({"tag":"yellow","kind":"node"}),
        "payload-0",
    );

    let txn = TxnId::new(9010);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");
    storage
        .update(
            txn,
            table_id,
            tuple_id,
            Row::new(vec![
                Value::Int(21),
                Value::Jsonb(serde_json::json!({"tag":"yellow","kind":"node"})),
                Value::Text("payload-1".to_owned()),
            ]),
        )
        .expect("update non-indexed column in txn");
    storage.commit_txn(txn, txn.get()).expect("commit txn");

    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch row after committed txn update")
        .expect("row exists");
    assert_eq!(
        updated,
        Row::new(vec![
            Value::Int(21),
            Value::Jsonb(serde_json::json!({"tag":"yellow","kind":"node"})),
            Value::Text("payload-1".to_owned())
        ])
    );

    let index_rows = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                btree_index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(21)]),
                    upper: Bound::Included(vec![Value::Int(21)]),
                },
                None,
            )
            .expect("scan btree after committed txn update"),
    );
    assert_eq!(index_rows.len(), 1);
    assert_eq!(index_rows[0].row, updated);

    let yellow_matches = collect_stream(
        storage
            .gin_containment_search(
                TxnId::default(),
                &snapshot(),
                gin_index_id,
                &serde_json::json!({"tag": "yellow"}),
            )
            .expect("search yellow token after committed txn update"),
    );
    assert_eq!(yellow_matches.len(), 1);
    assert_eq!(yellow_matches[0].row, updated);
}

#[test]
fn tx_insert_into_committed_table_populates_committed_indexes_on_commit() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2010);
    let index_id = IndexId::new(2010);
    create_table(&storage, table_id);
    create_index(&storage, table_id, index_id);

    let txn = TxnId::new(9011);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin insert txn");
    storage
        .insert(
            txn,
            table_id,
            Row::new(vec![Value::Int(42), Value::Text("indexed".to_owned())]),
        )
        .expect("insert indexed row in txn");
    storage
        .commit_txn(txn, txn.get())
        .expect("commit insert txn");

    let index_rows = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(42)]),
                    upper: Bound::Included(vec![Value::Int(42)]),
                },
                None,
            )
            .expect("scan committed index after transactional insert"),
    );
    assert_eq!(index_rows.len(), 1);
    assert_eq!(
        index_rows[0].row,
        Row::new(vec![Value::Int(42), Value::Text("indexed".to_owned())])
    );
}

fn create_jsonb_payload_table(storage: &InMemoryStorage, table_id: RelationId) {
    let descriptor = TableStorageDescriptor {
        table_id,
        columns: vec![
            StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Int,
                nullable: false,
            },
            StorageColumn {
                column_id: ColumnId::new(2),
                data_type: DataType::Jsonb,
                nullable: false,
            },
            StorageColumn {
                column_id: ColumnId::new(3),
                data_type: DataType::Text,
                nullable: false,
            },
        ],
        primary_key: None,
        shard_config: None,
    };
    storage
        .create_table_storage(TxnId::default(), &descriptor)
        .expect("create jsonb payload table");
}

fn insert_jsonb_payload_row(
    storage: &InMemoryStorage,
    table_id: RelationId,
    id: i32,
    json: serde_json::Value,
    payload: &str,
) -> TupleId {
    storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![
                Value::Int(id),
                Value::Jsonb(json),
                Value::Text(payload.to_owned()),
            ]),
        )
        .expect("insert jsonb payload row")
}

fn create_hnsw_index_with_quantization(
    storage: &InMemoryStorage,
    table_id: RelationId,
    index_id: IndexId,
    quantization: aiondb_storage_api::StoredQuantizationKind,
) {
    let mut descriptor = test_index_descriptor(index_id, table_id);
    descriptor.key_columns = vec![IndexKeyColumn {
        column_id: ColumnId::new(2),
        descending: false,
        nulls_first: false,
    }];
    descriptor.hnsw_options = Some(aiondb_storage_api::HnswStorageOptions {
        m: 8,
        ef_construction: 32,
        distance_metric: aiondb_storage_api::StoredVectorMetric::L2,
        quantization,
        prenormalised: false,
    });
    storage
        .create_index_storage(TxnId::default(), &descriptor)
        .expect("create quantized hnsw index");
}

#[test]
fn reindex_vector_index_trains_scalar_codebook_for_undersized_indexes() {
    // SQ index created on an empty table accumulates raw f32 vectors below
    // the lazy-training threshold. `reindex_vector_index` should retrain
    // the codebook from the current rows so subsequent searches run on the
    // quantized hot path.
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(2199);
    let index_id = IndexId::new(2200);
    create_vector_payload_table(&storage, table_id);
    create_hnsw_index_with_quantization(
        &storage,
        table_id,
        index_id,
        aiondb_storage_api::StoredQuantizationKind::Scalar,
    );

    for i in 1..=50i32 {
        let v = i as f32;
        storage
            .insert(
                TxnId::default(),
                table_id,
                Row::new(vec![
                    Value::Int(i),
                    Value::Vector(aiondb_core::VectorValue {
                        dims: 3,
                        values: vec![v, v + 1.0, v + 2.0],
                    }),
                ]),
            )
            .expect("insert vector row");
    }

    let pre = storage
        .vector_index_stats(index_id)
        .expect("read stats")
        .expect("hnsw index exists");
    assert_eq!(
        pre.quantization,
        aiondb_storage_api::StoredQuantizationKind::Scalar,
    );
    assert!(
        !pre.codebook_ready,
        "SQ codebook should be cold before REINDEX (only 50 rows)"
    );
    assert_eq!(pre.total_vectors, 50);

    storage
        .reindex_vector_index(index_id)
        .expect("reindex vector index");

    let post = storage
        .vector_index_stats(index_id)
        .expect("read stats post-reindex")
        .expect("hnsw index still registered");
    assert_eq!(
        post.quantization,
        aiondb_storage_api::StoredQuantizationKind::Scalar,
    );
    assert!(
        post.codebook_ready,
        "REINDEX VECTOR must train the SQ codebook"
    );
    assert_eq!(post.total_vectors, 50);
}

#[test]
fn reindex_vector_index_errors_for_unknown_index() {
    let storage = InMemoryStorage::new_without_wal();
    let err = storage
        .reindex_vector_index(IndexId::new(9999))
        .expect_err("missing index must surface an error");
    assert!(err.to_string().contains("REINDEX VECTOR"));
}
