use super::*;
use aiondb_wal::replication::ReplicaRegistry;

fn assert_segments_not_pruned(
    segments_before: &[aiondb_wal::segment::SegmentId],
    segments_after: &[aiondb_wal::segment::SegmentId],
    context: &str,
) {
    assert!(
        segments_after.len() >= segments_before.len(),
        "{context}: WAL segment count shrank unexpectedly"
    );
    assert_eq!(
        &segments_after[..segments_before.len()],
        segments_before,
        "{context}: checkpoint removed or rewrote existing WAL segments before it was allowed"
    );
}

// ── Storage Metrics Tests ──────────────────────────────────────────

#[test]
fn metrics_empty_storage_returns_zeros() {
    let storage = InMemoryStorage::new_without_wal();
    let m = storage.metrics().expect("metrics");
    assert_eq!(m.table_count, 0);
    assert_eq!(m.index_count, 0);
    assert_eq!(m.hnsw_index_count, 0);
    assert_eq!(m.total_row_count, 0);
    assert_eq!(m.total_dead_row_count, 0);
    assert_eq!(m.active_transaction_count, 0);
    assert_eq!(m.estimated_memory_bytes, 0);
    assert_eq!(m.node_label_count, 0);
    assert_eq!(m.edge_label_count, 0);
}

#[test]
fn metrics_tracks_table_and_row_counts() {
    let storage = InMemoryStorage::new_without_wal();
    let t1 = RelationId::new(100);
    let t2 = RelationId::new(101);
    create_table(&storage, t1);
    create_table(&storage, t2);

    let m = storage.metrics().expect("metrics after create");
    assert_eq!(m.table_count, 2);
    assert_eq!(m.total_row_count, 0);

    insert_row(&storage, TxnId::default(), t1, 1, "alice");
    insert_row(&storage, TxnId::default(), t1, 2, "bob");
    insert_row(&storage, TxnId::default(), t2, 10, "carol");

    let m = storage.metrics().expect("metrics after inserts");
    assert_eq!(m.table_count, 2);
    assert_eq!(m.total_row_count, 3);
    assert_eq!(m.total_dead_row_count, 0);
    assert!(m.estimated_memory_bytes > 0);
}

#[test]
fn metrics_tracks_dead_rows_after_delete() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(200);
    create_table(&storage, table_id);

    let tid1 = insert_row(&storage, TxnId::default(), table_id, 1, "alice");
    insert_row(&storage, TxnId::default(), table_id, 2, "bob");

    storage
        .delete(TxnId::default(), table_id, tid1)
        .expect("delete row");

    let m = storage.metrics().expect("metrics after delete");
    assert_eq!(m.total_row_count, 1);
    assert!(m.total_dead_row_count >= 1);
}

#[test]
fn metrics_dead_rows_decrease_after_vacuum() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(201);
    create_table(&storage, table_id);

    let tid1 = insert_row(&storage, TxnId::default(), table_id, 1, "alice");
    insert_row(&storage, TxnId::default(), table_id, 2, "bob");

    storage
        .delete(TxnId::default(), table_id, tid1)
        .expect("delete");

    let before = storage.metrics().expect("before vacuum");
    assert!(before.total_dead_row_count >= 1);

    storage.vacuum_table(table_id).expect("vacuum");

    let after = storage.metrics().expect("after vacuum");
    assert_eq!(after.total_dead_row_count, 0);
    assert_eq!(after.total_row_count, 1);
}

#[test]
fn autovacuum_cleans_dead_versions_after_threshold() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(202);
    create_table(&storage, table_id);

    let tuple_id = insert_row(&storage, TxnId::default(), table_id, 1, "seed");
    for version in 0..=AUTOVACUUM_MIN_DEAD_ROWS {
        storage
            .update(
                TxnId::default(),
                table_id,
                tuple_id,
                Row::new(vec![
                    Value::Int(1),
                    Value::Text(format!("version_{version}")),
                ]),
            )
            .expect("update");
    }

    let metrics = storage.metrics().expect("metrics after autovacuum");
    assert_eq!(metrics.total_row_count, 1);
    assert!(
        metrics.total_dead_row_count < AUTOVACUUM_MIN_DEAD_ROWS,
        "autovacuum should have cleaned accumulated dead versions"
    );
}

#[test]
fn autovacuum_waits_for_active_transactions() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(203);
    create_table(&storage, table_id);

    let tuple_id = insert_row(&storage, TxnId::default(), table_id, 1, "seed");
    let blocker_txn = TxnId::new(9_001);
    storage
        .begin_txn(blocker_txn, IsolationLevel::ReadCommitted)
        .expect("begin blocker");

    for version in 0..=AUTOVACUUM_MIN_DEAD_ROWS {
        storage
            .update(
                TxnId::default(),
                table_id,
                tuple_id,
                Row::new(vec![
                    Value::Int(1),
                    Value::Text(format!("blocked_{version}")),
                ]),
            )
            .expect("update while blocker is active");
    }

    let blocked = storage.metrics().expect("metrics with blocker");
    assert!(
        blocked.total_dead_row_count >= AUTOVACUUM_MIN_DEAD_ROWS,
        "autovacuum must defer while another transaction can still need old versions"
    );

    storage.rollback_txn(blocker_txn).expect("rollback blocker");
    storage
        .update(
            TxnId::default(),
            table_id,
            tuple_id,
            Row::new(vec![Value::Int(1), Value::Text("after_blocker".to_owned())]),
        )
        .expect("update after blocker rollback");

    let after = storage.metrics().expect("metrics after blocker rollback");
    assert!(
        after.total_dead_row_count < blocked.total_dead_row_count,
        "autovacuum should resume once conflicting transactions are gone"
    );
}

#[test]
fn metrics_tracks_index_count() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(300);
    let idx1 = IndexId::new(301);
    let idx2 = IndexId::new(302);
    create_table(&storage, table_id);

    let m0 = storage.metrics().expect("no indexes");
    assert_eq!(m0.index_count, 0);

    create_index(&storage, table_id, idx1);
    let m1 = storage.metrics().expect("one index");
    assert_eq!(m1.index_count, 1);

    create_index(&storage, table_id, idx2);
    let m2 = storage.metrics().expect("two indexes");
    assert_eq!(m2.index_count, 2);
}

#[test]
fn metrics_tracks_active_transactions() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(400);
    create_table(&storage, table_id);

    let m0 = storage.metrics().expect("no active txns");
    assert_eq!(m0.active_transaction_count, 0);

    let txn1 = TxnId::new(401);
    let txn2 = TxnId::new(402);
    storage
        .begin_txn(txn1, IsolationLevel::ReadCommitted)
        .expect("begin txn1");
    storage
        .begin_txn(txn2, IsolationLevel::ReadCommitted)
        .expect("begin txn2");

    let m1 = storage.metrics().expect("two active txns");
    assert_eq!(m1.active_transaction_count, 2);

    storage.commit_txn(txn1, 1).expect("commit txn1");

    let m2 = storage.metrics().expect("one active txn");
    assert_eq!(m2.active_transaction_count, 1);

    storage.rollback_txn(txn2).expect("rollback txn2");

    let m3 = storage.metrics().expect("no active txns again");
    assert_eq!(m3.active_transaction_count, 0);
}

#[test]
fn metrics_memory_grows_with_data() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(500);
    create_table(&storage, table_id);

    let before = storage
        .metrics()
        .expect("empty table")
        .estimated_memory_bytes;

    for i in 0..100 {
        insert_row(&storage, TxnId::default(), table_id, i, "data");
    }

    let after = storage.metrics().expect("100 rows").estimated_memory_bytes;
    assert!(after > before, "memory should grow after inserts");
}

#[test]
fn metrics_memory_includes_indexes() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(600);
    let index_id = IndexId::new(601);
    create_table(&storage, table_id);

    for i in 0..10 {
        insert_row(&storage, TxnId::default(), table_id, i, "x");
    }

    let without_idx = storage.metrics().expect("no index").estimated_memory_bytes;

    create_index(&storage, table_id, index_id);

    let with_idx = storage
        .metrics()
        .expect("with index")
        .estimated_memory_bytes;
    assert!(
        with_idx > without_idx,
        "memory should grow after creating index"
    );
}

#[test]
fn checkpoint_saves_snapshot_and_cleans_wal_segments() {
    use aiondb_wal::WalConfig;

    let dir = wal_test_dir("checkpoint_cleanup");

    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 100,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: aiondb_wal::WalCompression::None,
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
    };

    let storage = InMemoryStorage::new(StorageOptions::durable(config)).unwrap();

    let table_id = RelationId::new(1);
    let txn = TxnId::new(1);

    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .unwrap();
    storage
        .create_table_storage(txn, &test_table_descriptor(table_id))
        .unwrap();
    storage.commit_txn(txn, 1).unwrap();

    for i in 2..30u64 {
        let txn = TxnId::new(i);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage
            .insert(
                txn,
                table_id,
                Row::new(vec![Value::Int(i as i32), Value::Null]),
            )
            .unwrap();
        storage.commit_txn(txn, i).unwrap();
    }

    let segments_before = aiondb_wal::segment::list_segments(&dir).unwrap();
    assert!(
        segments_before.len() >= 3,
        "Expected at least 3 WAL segments, got {}",
        segments_before.len()
    );

    let info = storage.checkpoint().unwrap();
    assert!(info.checkpoint_lsn > 0);
    assert!(info.dirty_pages_flushed > 0, "should report flushed rows");

    let segments_after = aiondb_wal::segment::list_segments(&dir).unwrap();
    assert!(
        segments_after.len() < segments_before.len(),
        "checkpoint should clean up old WAL segments: before={}, after={}",
        segments_before.len(),
        segments_after.len()
    );

    assert!(
        dir.join("base.snapshot").exists(),
        "base snapshot file should exist after checkpoint"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn checkpoint_does_not_prune_segments_needed_by_fresh_replica() {
    use aiondb_wal::WalConfig;
    use std::sync::Arc;

    let dir = wal_test_dir("checkpoint_replica_retention");

    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 100,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: aiondb_wal::WalCompression::None,
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
    };

    let mut storage = InMemoryStorage::new(StorageOptions::durable(config)).unwrap();
    let replica_registry = Arc::new(ReplicaRegistry::new());
    storage.set_replica_registry(Arc::clone(&replica_registry));
    replica_registry
        .register(aiondb_wal::Lsn::new(1))
        .expect("register replica");

    let table_id = RelationId::new(1);
    let txn = TxnId::new(1);

    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .unwrap();
    storage
        .create_table_storage(txn, &test_table_descriptor(table_id))
        .unwrap();
    storage.commit_txn(txn, 1).unwrap();

    for i in 2..30u64 {
        let txn = TxnId::new(i);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage
            .insert(
                txn,
                table_id,
                Row::new(vec![Value::Int(i as i32), Value::Null]),
            )
            .unwrap();
        storage.commit_txn(txn, i).unwrap();
    }

    let segments_before = aiondb_wal::segment::list_segments(&dir).unwrap();
    assert!(
        segments_before.len() >= 3,
        "Expected at least 3 WAL segments, got {}",
        segments_before.len()
    );

    let info = storage.checkpoint().unwrap();
    assert!(info.checkpoint_lsn > 0);

    let segments_after = aiondb_wal::segment::list_segments(&dir).unwrap();
    assert!(
        segments_before
            .iter()
            .all(|segment_id| segments_after.contains(segment_id)),
        "checkpoint pruned WAL segments still needed by a fresh replica: before={segments_before:?} after={segments_after:?}",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn checkpoint_does_not_prune_segments_pinned_by_physical_slot() {
    use aiondb_wal::{Lsn, WalConfig};
    use std::sync::Arc;

    let dir = wal_test_dir("checkpoint_slot_retention");

    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 100,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: aiondb_wal::WalCompression::None,
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
    };

    let mut storage = InMemoryStorage::new(StorageOptions::durable(config)).unwrap();
    let slot_registry = Arc::new(
        ReplicaRegistry::open(dir.join("replication_slots"))
            .expect("open persistent slot registry"),
    );
    slot_registry
        .create_physical_slot("slot_a", Lsn::new(1), true, 1)
        .expect("create slot");
    storage.set_replica_registry(Arc::clone(&slot_registry));

    let table_id = RelationId::new(1);
    let txn = TxnId::new(1);

    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .unwrap();
    storage
        .create_table_storage(txn, &test_table_descriptor(table_id))
        .unwrap();
    storage.commit_txn(txn, 1).unwrap();

    for i in 2..30u64 {
        let txn = TxnId::new(i);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage
            .insert(
                txn,
                table_id,
                Row::new(vec![Value::Int(i as i32), Value::Null]),
            )
            .unwrap();
        storage.commit_txn(txn, i).unwrap();
    }

    let segments_before = aiondb_wal::segment::list_segments(&dir).unwrap();
    assert!(
        segments_before.len() >= 3,
        "Expected at least 3 WAL segments, got {}",
        segments_before.len()
    );

    let info = storage.checkpoint().unwrap();
    assert!(info.checkpoint_lsn > 0);

    let segments_after = aiondb_wal::segment::list_segments(&dir).unwrap();
    assert_segments_not_pruned(
        &segments_before,
        &segments_after,
        "checkpoint pruned WAL segments still pinned by a physical replication slot",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn checkpoint_retains_minimum_wal_segment_floor_without_replicas() {
    use aiondb_wal::WalConfig;

    let dir = wal_test_dir("checkpoint_keep_floor");

    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 100,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: aiondb_wal::WalCompression::None,
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
    };

    let mut options = StorageOptions::durable(config);
    options.min_wal_keep_segments = 2;
    let storage = InMemoryStorage::new(options).unwrap();

    let table_id = RelationId::new(1);
    let txn = TxnId::new(1);

    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .unwrap();
    storage
        .create_table_storage(txn, &test_table_descriptor(table_id))
        .unwrap();
    storage.commit_txn(txn, 1).unwrap();

    for i in 2..40u64 {
        let txn = TxnId::new(i);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage
            .insert(
                txn,
                table_id,
                Row::new(vec![Value::Int(i as i32), Value::Null]),
            )
            .unwrap();
        storage.commit_txn(txn, i).unwrap();
    }

    let segments_before = aiondb_wal::segment::list_segments(&dir).unwrap();
    assert!(
        segments_before.len() >= 3,
        "Expected at least 3 WAL segments, got {}",
        segments_before.len()
    );

    let info = storage.checkpoint().unwrap();
    assert!(info.checkpoint_lsn > 0);

    let segments_after = aiondb_wal::segment::list_segments(&dir).unwrap();
    assert_eq!(
        segments_after.len(),
        segments_before.len().min(2),
        "checkpoint cleanup must preserve the configured newest WAL segment floor"
    );
    let oldest_allowed = segments_before[segments_before.len() - 2];
    let newest_before = *segments_before.last().expect("segments before checkpoint");
    assert!(
        segments_after[0] >= oldest_allowed,
        "checkpoint cleanup kept a segment older than the configured newest floor"
    );
    assert!(
        segments_after[0] <= newest_before.next(),
        "checkpoint cleanup retained an unexpected oldest WAL segment"
    );
    assert_eq!(
        segments_after[1],
        segments_after[0].next(),
        "checkpoint cleanup should keep a contiguous newest WAL tail"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn checkpoint_rejects_active_transactions() {
    let (storage, dir) = storage_with_wal("checkpoint_active_transactions");

    storage
        .begin_txn(TxnId::new(7000), IsolationLevel::ReadCommitted)
        .expect("begin explicit transaction");

    let err = storage
        .checkpoint()
        .expect_err("checkpoint with active txns must fail");
    assert!(format!("{err}").contains("active"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn checkpoint_failure_does_not_cleanup_wal_before_snapshot_is_durable() {
    use aiondb_wal::WalConfig;

    let dir = wal_test_dir("checkpoint_snapshot_dir_sync_failure");
    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 100,
        sync_on_flush: true,
        group_commit_delay_micros: 0,
        wal_compression: aiondb_wal::WalCompression::None,
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
    };

    let storage = InMemoryStorage::new(StorageOptions::durable(config.clone())).unwrap();

    let table_id = RelationId::new(77);
    create_table(&storage, table_id);
    for i in 0..24 {
        insert_row(&storage, TxnId::default(), table_id, i, "durable");
    }

    let segments_before = aiondb_wal::segment::list_segments(&dir).unwrap();
    assert!(
        segments_before.len() >= 2,
        "expected multiple WAL segments before checkpoint"
    );

    super::snapshot::inject_dir_sync_failure();
    let err = storage
        .checkpoint()
        .expect_err("checkpoint must fail if snapshot directory sync fails");
    assert!(format!("{err}").contains("snapshot directory"));

    let segments_after = aiondb_wal::segment::list_segments(&dir).unwrap();
    assert_segments_not_pruned(
        &segments_before,
        &segments_after,
        "WAL cleanup must not run until the snapshot is durably published",
    );

    drop(storage);

    let (recovered, _) = InMemoryStorage::open_with_recovery(config).unwrap();
    let rows = collect_stream(
        recovered
            .scan_table(TxnId::default(), &snapshot(), table_id, None)
            .expect("scan recovered rows"),
    );
    assert_eq!(rows.len(), 24);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn checkpoint_rename_failure_does_not_cleanup_wal_before_snapshot_is_durable() {
    use aiondb_wal::WalConfig;

    let dir = wal_test_dir("checkpoint_snapshot_rename_failure");
    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 100,
        sync_on_flush: true,
        group_commit_delay_micros: 0,
        wal_compression: aiondb_wal::WalCompression::None,
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
    };

    let storage = InMemoryStorage::new(StorageOptions::durable(config.clone())).unwrap();

    let table_id = RelationId::new(78);
    create_table(&storage, table_id);
    for i in 0..24 {
        insert_row(&storage, TxnId::default(), table_id, i, "durable");
    }

    let segments_before = aiondb_wal::segment::list_segments(&dir).unwrap();
    assert!(
        segments_before.len() >= 2,
        "expected multiple WAL segments before checkpoint"
    );

    super::snapshot::inject_rename_failure();
    let err = storage
        .checkpoint()
        .expect_err("checkpoint must fail if snapshot rename fails");
    assert!(format!("{err}").contains("rename failure"));

    let segments_after = aiondb_wal::segment::list_segments(&dir).unwrap();
    assert_segments_not_pruned(
        &segments_before,
        &segments_after,
        "WAL cleanup must not run until the snapshot is durably published",
    );

    drop(storage);

    let (recovered, _) = InMemoryStorage::open_with_recovery(config).unwrap();
    let rows = collect_stream(
        recovered
            .scan_table(TxnId::default(), &snapshot(), table_id, None)
            .expect("scan recovered rows"),
    );
    assert_eq!(rows.len(), 24);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn checkpoint_paged_snapshot_publish_failure_does_not_cleanup_wal() {
    use aiondb_wal::WalConfig;

    let dir = wal_test_dir("checkpoint_paged_snapshot_publish_failure");
    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 100,
        sync_on_flush: true,
        group_commit_delay_micros: 0,
        wal_compression: aiondb_wal::WalCompression::None,
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
    };

    let storage = InMemoryStorage::new(StorageOptions::durable(config.clone())).unwrap();

    let table_id = RelationId::new(79);
    create_table(&storage, table_id);
    for i in 0..24 {
        insert_row(&storage, TxnId::default(), table_id, i, "durable");
    }

    let segments_before = aiondb_wal::segment::list_segments(&dir).unwrap();
    assert!(
        segments_before.len() >= 2,
        "expected multiple WAL segments before checkpoint"
    );

    super::paged_snapshot::inject_publish_failure();
    let err = storage
        .checkpoint()
        .expect_err("checkpoint must fail if paged snapshot publish fails");
    assert!(format!("{err}").contains("publish failure"));

    let segments_after = aiondb_wal::segment::list_segments(&dir).unwrap();
    assert_segments_not_pruned(
        &segments_before,
        &segments_after,
        "WAL cleanup must not run until the paged snapshot is durably published",
    );

    drop(storage);

    let (recovered, _) = InMemoryStorage::open_with_recovery(config).unwrap();
    let rows = collect_stream(
        recovered
            .scan_table(TxnId::default(), &snapshot(), table_id, None)
            .expect("scan recovered rows"),
    );
    assert_eq!(rows.len(), 24);

    let _ = std::fs::remove_dir_all(&dir);
}
