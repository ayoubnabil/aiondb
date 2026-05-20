use super::*;

#[path = "advanced_recovery_snapshot_based.rs"]
mod snapshot_based;

#[test]
fn recover_repeated_crash_recovery_cycles() {
    let dir = test_dir("recover_repeated_cycles");
    let desc = sample_table_desc();
    let t = desc.table_id;
    let txn1 = TxnId::new(1000);
    // Round 1: create table + insert, commit, recover.
    write_wal(
        &dir,
        &[
            begin(txn1),
            WalRecord::CreateTable {
                txn_id: txn1,
                descriptor: desc.clone(),
            },
            insert(txn1, t, 1, row2(10, "round1")),
            commit(txn1, 1),
        ],
    );
    let (s1, r1) = InMemoryStorage::open_with_recovery(test_config(dir.clone())).unwrap();
    assert_eq!(r1.recovered_transactions, 1);
    drop(s1);
    // Round 2: append more records, recover again.
    let txn2 = TxnId::new(1001);
    let mut writer = WalWriter::open(test_config(dir.clone())).unwrap();
    for rec in &[
        begin(txn2),
        insert(txn2, t, 2, row2(20, "round2")),
        commit(txn2, 2),
    ] {
        writer.append(rec).unwrap();
    }
    writer.flush().unwrap();
    drop(writer);
    let (storage, r2) = InMemoryStorage::open_with_recovery(test_config(dir.clone())).unwrap();
    assert_eq!(r2.recovered_transactions, 2);
    let v1 = fetch_row(&storage, t, 1).unwrap();
    assert_eq!(
        v1.values,
        vec![Value::Int(10), Value::Text("round1".into())]
    );
    let v2 = fetch_row(&storage, t, 2).unwrap();
    assert_eq!(
        v2.values,
        vec![Value::Int(20), Value::Text("round2".into())]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_after_checkpoint() {
    use aiondb_wal::Lsn;

    let dir = test_dir("recover_checkpoint");
    let desc = sample_table_desc();
    let t = desc.table_id;
    let (txn1, txn2) = (TxnId::new(1100), TxnId::new(1101));
    write_wal(
        &dir,
        &[
            begin(txn1),
            WalRecord::CreateTable {
                txn_id: txn1,
                descriptor: desc.clone(),
            },
            insert(txn1, t, 1, row2(1, "before_cp")),
            commit(txn1, 1),
            WalRecord::Checkpoint {
                last_committed_lsn: Lsn::new(4),
            },
            begin(txn2),
            insert(txn2, t, 2, row2(2, "after_cp")),
            commit(txn2, 2),
        ],
    );
    let (storage, report) = InMemoryStorage::open_with_recovery(test_config(dir.clone())).unwrap();
    assert_eq!(report.recovered_transactions, 2);
    let v1 = fetch_row(&storage, t, 1).unwrap();
    assert_eq!(
        v1.values,
        vec![Value::Int(1), Value::Text("before_cp".into())]
    );
    let v2 = fetch_row(&storage, t, 2).unwrap();
    assert_eq!(
        v2.values,
        vec![Value::Int(2), Value::Text("after_cp".into())]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_crash_mid_transaction_after_committed() {
    let dir = test_dir("recover_crash_mid_txn");
    let desc = sample_table_desc();
    let t = desc.table_id;
    let (txn_s, txn_g, txn_c) = (TxnId::new(1200), TxnId::new(1201), TxnId::new(1202));
    write_wal(
        &dir,
        &[
            begin(txn_s),
            WalRecord::CreateTable {
                txn_id: txn_s,
                descriptor: desc.clone(),
            },
            commit(txn_s, 1),
            begin(txn_g),
            insert(txn_g, t, 1, row2(42, "committed")),
            commit(txn_g, 2),
            // Crash mid-transaction: begin + insert but NO commit.
            begin(txn_c),
            insert(txn_c, t, 2, row2(99, "crashed")),
        ],
    );
    let (storage, report) = InMemoryStorage::open_with_recovery(test_config(dir.clone())).unwrap();
    assert_eq!(report.recovered_transactions, 2); // setup + good
    assert_eq!(fetch_row(&storage, t, 1).unwrap().values[0], Value::Int(42));
    assert!(fetch_row(&storage, t, 2).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_large_overflow_row_from_wal() {
    let dir = test_dir("recover_large_overflow_row_from_wal");
    let desc = sample_table_desc();
    let t = desc.table_id;
    let txn = TxnId::new(1250);
    let payload = "overflow payload ".repeat(800);
    let row = Row::new(vec![Value::Int(7), Value::Text(payload.clone())]);
    write_wal(
        &dir,
        &[
            begin(txn),
            WalRecord::CreateTable {
                txn_id: txn,
                descriptor: desc.clone(),
            },
            WalRecord::InsertRow {
                txn_id: txn,
                table_id: t,
                tuple_id: TupleId::new(1),
                row: row.clone(),
            },
            commit(txn, 1),
        ],
    );

    let (storage, report) = InMemoryStorage::open_with_recovery(test_config(dir.clone())).unwrap();
    assert_eq!(report.recovered_transactions, 1);
    assert_eq!(fetch_row(&storage, t, 1).unwrap(), row);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_index_rebuild() {
    use aiondb_core::IndexId;
    use aiondb_storage_api::{IndexKeyColumn, IndexStorageDescriptor, KeyRange};

    let dir = test_dir("recover_index_rebuild");
    let desc = sample_table_desc();
    let t = desc.table_id;
    let txn = TxnId::new(1300);
    let idx = IndexStorageDescriptor {
        index_id: IndexId::new(1),
        table_id: t,
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
    write_wal(
        &dir,
        &[
            begin(txn),
            WalRecord::CreateTable {
                txn_id: txn,
                descriptor: desc.clone(),
            },
            insert(txn, t, 1, row2(10, "a")),
            insert(txn, t, 2, row2(20, "b")),
            WalRecord::CreateIndex {
                txn_id: txn,
                descriptor: idx.clone(),
            },
            commit(txn, 1),
        ],
    );
    let (storage, report) = InMemoryStorage::open_with_recovery(test_config(dir.clone())).unwrap();
    assert_eq!(report.recovered_transactions, 1);
    let state = storage.state.read();
    let index = state
        .indexes
        .get(&IndexId::new(1))
        .expect("index should exist");
    assert_eq!(index.descriptor, idx);
    let candidates = index.candidate_tuple_ids(&KeyRange::full()).unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(candidates.contains(&TupleId::new(1)));
    assert!(candidates.contains(&TupleId::new(2)));
    drop(state);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_idempotency() {
    let dir = test_dir("recover_idempotent");
    let desc = sample_table_desc();
    let t = desc.table_id;
    let txn = TxnId::new(1400);
    write_wal(
        &dir,
        &[
            begin(txn),
            WalRecord::CreateTable {
                txn_id: txn,
                descriptor: desc.clone(),
            },
            insert(txn, t, 1, row2(7, "idem")),
            insert(txn, t, 2, Row::new(vec![Value::Int(8), Value::Null])),
            commit(txn, 1),
        ],
    );
    // Two recoveries from the same WAL (no writes between).
    let (s1, rpt1) = InMemoryStorage::open_with_recovery(test_config(dir.clone())).unwrap();
    let (s2, rpt2) = InMemoryStorage::open_with_recovery(test_config(dir.clone())).unwrap();
    assert_eq!(rpt1.recovered_transactions, 1);
    assert_eq!(rpt1.recovered_transactions, rpt2.recovered_transactions);
    for tid in [1u64, 2] {
        assert_eq!(
            fetch_row(&s1, t, tid).unwrap().values,
            fetch_row(&s2, t, tid).unwrap().values
        );
    }
    let st1 = s1.state.read();
    let st2 = s2.state.read();
    assert_eq!(
        st1.tables.get(&t).unwrap().tuple_ids().count(),
        st2.tables.get(&t).unwrap().tuple_ids().count(),
    );
    drop(st1);
    drop(st2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_interleaved_transactions() {
    let dir = test_dir("recover_interleaved");
    let desc = sample_table_desc();
    let t = desc.table_id;
    let (txn_s, txn1, txn2) = (TxnId::new(1500), TxnId::new(1501), TxnId::new(1502));
    write_wal(
        &dir,
        &[
            begin(txn_s),
            WalRecord::CreateTable {
                txn_id: txn_s,
                descriptor: desc.clone(),
            },
            commit(txn_s, 1),
            // Interleaved: t1 begin, t2 begin, t1 insert, t2 insert, t1 insert, t2 commit, t1 commit.
            begin(txn1),
            begin(txn2),
            insert(txn1, t, 1, row2(100, "t1_first")),
            insert(txn2, t, 2, row2(200, "t2_only")),
            insert(txn1, t, 3, row2(300, "t1_second")),
            commit(txn2, 2),
            commit(txn1, 3),
        ],
    );
    let (storage, report) = InMemoryStorage::open_with_recovery(test_config(dir.clone())).unwrap();
    assert_eq!(report.recovered_transactions, 3); // setup + txn1 + txn2
    assert_eq!(
        fetch_row(&storage, t, 1).unwrap().values,
        vec![Value::Int(100), Value::Text("t1_first".into())]
    );
    assert_eq!(
        fetch_row(&storage, t, 2).unwrap().values,
        vec![Value::Int(200), Value::Text("t2_only".into())]
    );
    assert_eq!(
        fetch_row(&storage, t, 3).unwrap().values,
        vec![Value::Int(300), Value::Text("t1_second".into())]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_uses_commit_lsn_order_not_transaction_id() {
    let dir = test_dir("recover_commit_lsn_order");
    let desc = sample_table_desc();
    let t = desc.table_id;
    let setup = TxnId::new(1599);
    let delete_txn = TxnId::new(1600);
    let update_txn = TxnId::new(1601);
    write_wal(
        &dir,
        &[
            begin(setup),
            WalRecord::CreateTable {
                txn_id: setup,
                descriptor: desc.clone(),
            },
            insert(setup, t, 1, row2(1, "base")),
            commit(setup, 1),
            begin(delete_txn),
            begin(update_txn),
            WalRecord::DeleteRow {
                txn_id: delete_txn,
                table_id: t,
                tuple_id: TupleId::new(1),
            },
            WalRecord::UpdateRow {
                txn_id: update_txn,
                table_id: t,
                old_tuple_id: TupleId::new(1),
                new_tuple_id: TupleId::new(1),
                row: row2(2, "updated"),
            },
            commit(update_txn, 2),
            commit(delete_txn, 3),
        ],
    );
    let (storage, report) = InMemoryStorage::open_with_recovery(test_config(dir.clone())).unwrap();
    assert_eq!(report.recovered_transactions, 3);
    assert!(
        fetch_row(&storage, t, 1).is_none(),
        "delete committed after update must win during recovery"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_rebuilds_hnsw_indexes() {
    use aiondb_core::IndexId;

    let dir = test_dir("recover_hnsw_index");
    let desc = sample_vector_table_desc();
    let t = desc.table_id;
    let txn = TxnId::new(1700);
    let idx = IndexStorageDescriptor {
        index_id: IndexId::new(20),
        table_id: t,
        unique: false,
        nulls_not_distinct: false,
        gin: false,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(2),
            descending: false,
            nulls_first: false,
        }],
        include_columns: vec![],
        hnsw_options: None,
            ivf_flat_options: None,
    };
    write_wal(
        &dir,
        &[
            begin(txn),
            WalRecord::CreateTable {
                txn_id: txn,
                descriptor: desc.clone(),
            },
            insert(
                txn,
                t,
                1,
                Row::new(vec![
                    Value::Int(1),
                    Value::Vector(aiondb_core::VectorValue::new(2, vec![1.0, 0.0])),
                ]),
            ),
            insert(
                txn,
                t,
                2,
                Row::new(vec![
                    Value::Int(2),
                    Value::Vector(aiondb_core::VectorValue::new(2, vec![0.0, 1.0])),
                ]),
            ),
            WalRecord::CreateIndex {
                txn_id: txn,
                descriptor: idx.clone(),
            },
            commit(txn, 1),
        ],
    );

    let (storage, _) = InMemoryStorage::open_with_recovery(test_config(dir.clone())).unwrap();
    let mut stream = storage
        .vector_search(
            TxnId::default(),
            &all_visible_snapshot(),
            idx.index_id,
            &[1.0, 0.0],
            1,
            8,
            None,
            None,
            None,
        )
        .unwrap();
    let first = stream.next().unwrap().expect("nearest row");
    assert_eq!(first.row.values[0], Value::Int(1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_rebuilds_gin_indexes() {
    use aiondb_core::IndexId;

    let dir = test_dir("recover_gin_index");
    let desc = sample_json_table_desc();
    let t = desc.table_id;
    let txn = TxnId::new(1800);
    let idx = IndexStorageDescriptor {
        index_id: IndexId::new(30),
        table_id: t,
        unique: false,
        nulls_not_distinct: false,
        gin: true,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(2),
            descending: false,
            nulls_first: false,
        }],
        include_columns: vec![],
        hnsw_options: None,
            ivf_flat_options: None,
    };
    write_wal(
        &dir,
        &[
            begin(txn),
            WalRecord::CreateTable {
                txn_id: txn,
                descriptor: desc.clone(),
            },
            insert(
                txn,
                t,
                1,
                Row::new(vec![
                    Value::Int(1),
                    Value::Jsonb(serde_json::json!({"kind":"user","active":true})),
                ]),
            ),
            insert(
                txn,
                t,
                2,
                Row::new(vec![
                    Value::Int(2),
                    Value::Jsonb(serde_json::json!({"kind":"admin","active":true})),
                ]),
            ),
            WalRecord::CreateIndex {
                txn_id: txn,
                descriptor: idx.clone(),
            },
            commit(txn, 1),
        ],
    );

    let (storage, _) = InMemoryStorage::open_with_recovery(test_config(dir.clone())).unwrap();
    let mut stream = storage
        .gin_containment_search(
            TxnId::default(),
            &all_visible_snapshot(),
            idx.index_id,
            &serde_json::json!({"kind":"admin"}),
        )
        .unwrap();
    let first = stream.next().unwrap().expect("matching row");
    assert_eq!(first.row.values[0], Value::Int(2));
    let _ = std::fs::remove_dir_all(&dir);
}
