use super::*;

#[test]
fn lsm_backend_handle_reports_kind() {
    let data_dir = unique_temp_path("lsm");
    let backend = StorageBackendHandle::open(StorageBackendSpec::Lsm {
        config: LsmBackendConfig::new(&data_dir),
    })
    .expect("lsm backend should open");

    assert_eq!(backend.kind(), StorageBackendKind::Lsm);
    assert!(backend.supports_durability());

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn lsm_backend_creates_manifest_and_layout_dirs() {
    let data_dir = unique_temp_path("lsm-layout");
    StorageBackendHandle::open(StorageBackendSpec::Lsm {
        config: LsmBackendConfig::new(&data_dir),
    })
    .expect("lsm backend should create its layout");

    assert!(data_dir.join("manifest.json").is_file());
    assert!(data_dir.join("wal").is_dir());
    assert!(data_dir.join("levels").is_dir());
    assert!(data_dir.join("levels").join("level-0").is_dir());
    assert!(data_dir.join("levels").join("level-1").is_dir());

    let manifest = std::fs::read_to_string(data_dir.join("manifest.json"))
        .expect("lsm manifest should be readable");
    let manifest: serde_json::Value =
        serde_json::from_str(&manifest).expect("lsm manifest should be valid json");
    assert_eq!(manifest["backend"], "lsm");
    assert_eq!(manifest["memtable_flush_bytes"], 4 * 1024 * 1024);
    assert_eq!(
        manifest["level_one_runs"],
        serde_json::Value::Array(Vec::new())
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn lsm_backend_checkpoint_creates_level_zero_run() {
    let data_dir = unique_temp_path("lsm-checkpoint");
    let backend = StorageBackendHandle::open(StorageBackendSpec::Lsm {
        config: LsmBackendConfig::new(&data_dir),
    })
    .expect("lsm backend should open");

    let checkpoint = backend
        .checkpoint()
        .expect("lsm backend checkpoint should succeed");

    let run_dir = data_dir.join("levels").join("level-0");
    let mut run_files: Vec<PathBuf> = std::fs::read_dir(&run_dir)
        .expect("lsm run directory should be readable")
        .map(|entry| entry.expect("run entry should be readable").path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("sst"))
        .collect();
    run_files.sort();

    assert_eq!(run_files.len(), 1);

    let manifest = std::fs::read_to_string(data_dir.join("manifest.json"))
        .expect("lsm manifest should be readable");
    let manifest: serde_json::Value =
        serde_json::from_str(&manifest).expect("lsm manifest should be valid json");
    assert_eq!(manifest["next_sstable_id"], 2);
    assert_eq!(manifest["last_checkpoint_lsn"], checkpoint.checkpoint_lsn);
    assert_eq!(
        manifest["level_zero_runs"][0]["checkpoint_lsn"],
        checkpoint.checkpoint_lsn
    );
    assert_eq!(
        manifest["level_zero_runs"][0]["dirty_pages_flushed"],
        checkpoint.dirty_pages_flushed
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn lsm_backend_checkpoint_compacts_level_zero_runs_into_level_one() {
    let data_dir = unique_temp_path("lsm-compaction");
    let backend = StorageBackendHandle::open(StorageBackendSpec::Lsm {
        config: LsmBackendConfig::new(&data_dir),
    })
    .expect("lsm backend should open");

    let mut last_checkpoint = None;
    for _ in 0..4 {
        last_checkpoint = Some(
            backend
                .checkpoint()
                .expect("lsm backend checkpoint should succeed"),
        );
    }
    let last_checkpoint = last_checkpoint.expect("checkpoint loop should run");

    let level_zero_run_count = std::fs::read_dir(data_dir.join("levels").join("level-0"))
        .expect("level-0 directory should be readable")
        .count();
    let level_one_run_count = std::fs::read_dir(data_dir.join("levels").join("level-1"))
        .expect("level-1 directory should be readable")
        .map(|entry| entry.expect("level-1 entry should be readable").path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("sst"))
        .count();
    assert_eq!(level_zero_run_count, 0);
    assert_eq!(level_one_run_count, 1);

    let manifest = std::fs::read_to_string(data_dir.join("manifest.json"))
        .expect("lsm manifest should be readable");
    let manifest: serde_json::Value =
        serde_json::from_str(&manifest).expect("lsm manifest should be valid json");
    assert_eq!(
        manifest["level_zero_runs"],
        serde_json::Value::Array(Vec::new())
    );
    assert_eq!(
        manifest["level_one_runs"][0]["checkpoint_lsn"],
        last_checkpoint.checkpoint_lsn
    );
    assert_eq!(
        manifest["level_one_runs"][0]["dirty_pages_flushed"],
        last_checkpoint.dirty_pages_flushed
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn lsm_backend_recovers_from_level_zero_run_when_base_snapshot_is_missing() {
    let data_dir = unique_temp_path("lsm-recover-level-zero");
    let table = test_table_descriptor(RelationId::new(41));
    let backend = StorageBackendHandle::open(StorageBackendSpec::Lsm {
        config: LsmBackendConfig::new(&data_dir),
    })
    .expect("lsm backend should open");

    backend
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(7), Value::Text("level-zero".into())]),
        )
        .expect("row should be inserted");
    backend
        .checkpoint()
        .expect("checkpoint should create a recoverable level-0 run");
    drop(backend);

    let snapshot_path = data_dir.join("wal").join("base.snapshot");
    std::fs::remove_file(&snapshot_path).expect("file snapshot fixture should be removable");

    let reopened = StorageBackendHandle::open(StorageBackendSpec::Lsm {
        config: LsmBackendConfig::new(&data_dir),
    })
    .expect("lsm backend should reopen from level-0 run snapshot");

    assert!(snapshot_path.is_file());
    let rows = collect_stream(
        reopened
            .scan_table(TxnId::default(), &visible_snapshot(), table.table_id, None)
            .expect("reopened backend should scan recovered table"),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row.values[0], Value::Int(7));
    assert_eq!(rows[0].row.values[1], Value::Text("level-zero".into()));

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn lsm_backend_recovers_from_level_one_run_when_base_snapshot_is_missing() {
    let data_dir = unique_temp_path("lsm-recover-level-one");
    let table = test_table_descriptor(RelationId::new(42));
    let backend = StorageBackendHandle::open(StorageBackendSpec::Lsm {
        config: LsmBackendConfig::new(&data_dir),
    })
    .expect("lsm backend should open");

    backend
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(9), Value::Text("level-one".into())]),
        )
        .expect("row should be inserted");
    for _ in 0..4 {
        backend
            .checkpoint()
            .expect("checkpoint should eventually compact into level-1");
    }
    drop(backend);

    let snapshot_path = data_dir.join("wal").join("base.snapshot");
    std::fs::remove_file(&snapshot_path).expect("file snapshot fixture should be removable");

    let reopened = StorageBackendHandle::open(StorageBackendSpec::Lsm {
        config: LsmBackendConfig::new(&data_dir),
    })
    .expect("lsm backend should reopen from compacted level-1 run snapshot");

    assert!(snapshot_path.is_file());
    let level_one_run_count = std::fs::read_dir(data_dir.join("levels").join("level-1"))
        .expect("level-1 directory should be readable")
        .map(|entry| entry.expect("level-1 entry should be readable").path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("sst"))
        .count();
    assert_eq!(level_one_run_count, 1);

    let rows = collect_stream(
        reopened
            .scan_table(TxnId::default(), &visible_snapshot(), table.table_id, None)
            .expect("reopened backend should scan recovered table"),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row.values[0], Value::Int(9));
    assert_eq!(rows[0].row.values[1], Value::Text("level-one".into()));

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn lsm_backend_rejects_invalid_manifest_backend() {
    let data_dir = unique_temp_path("lsm-invalid-manifest");
    std::fs::create_dir_all(&data_dir).expect("base dir should be creatable");
    std::fs::write(
        data_dir.join("manifest.json"),
        format!(
            "{{\"version\":1,\"backend\":\"disk\",\"memtable_flush_bytes\":4194304,\"block_size_bytes\":{},\"wal_dir\":\"{}\",\"levels_dir\":\"{}\"}}",
            aiondb_buffer_pool::PAGE_SIZE,
            data_dir.join("wal").display(),
            data_dir.join("levels").display()
        ),
    )
    .expect("manifest fixture should be writable");

    let error = StorageBackendHandle::open(StorageBackendSpec::Lsm {
        config: LsmBackendConfig::new(&data_dir),
    })
    .expect_err("invalid lsm manifest should be rejected");

    assert!(error
        .to_string()
        .contains("backend must be 'lsm', got 'disk'"));

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn lsm_backend_replication_seed_round_trip_recovers_levels_and_wal() {
    let data_dir = unique_temp_path("lsm-seed-source");
    let seed_dir = unique_temp_path("lsm-seed-export");
    let replica_dir = unique_temp_path("lsm-seed-replica");
    let table = test_table_descriptor(RelationId::new(431));
    let backend = StorageBackendHandle::open(StorageBackendSpec::Lsm {
        config: LsmBackendConfig::new(&data_dir),
    })
    .expect("lsm backend should open");

    backend
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(1), Value::Text("level".into())]),
        )
        .expect("checkpoint seed row should be inserted");
    backend
        .checkpoint()
        .expect("lsm backend checkpoint should succeed");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(2), Value::Text("wal".into())]),
        )
        .expect("post-checkpoint wal row should be inserted");

    let manifest = backend
        .export_replication_seed(&seed_dir)
        .expect("lsm replication seed export should succeed");
    assert_eq!(manifest.backend, "lsm");
    assert!(seed_dir
        .join("state")
        .join("levels")
        .join("level-0")
        .is_dir());
    assert!(seed_dir.join("state").join("wal").is_dir());

    install_replication_seed(&seed_dir, &replica_dir)
        .expect("lsm replication seed install should succeed");
    std::fs::remove_file(replica_dir.join("wal").join("base.snapshot"))
        .expect("replica lsm file snapshot should be removable");

    let reopened = StorageBackendHandle::open(StorageBackendSpec::Lsm {
        config: LsmBackendConfig::new(&replica_dir),
    })
    .expect("replica lsm backend should reopen");
    let rows = collect_stream(
        reopened
            .scan_table(TxnId::default(), &visible_snapshot(), table.table_id, None)
            .expect("replica lsm backend should scan replicated table"),
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].row.values[0], Value::Int(1));
    assert_eq!(rows[0].row.values[1], Value::Text("level".into()));
    assert_eq!(rows[1].row.values[0], Value::Int(2));
    assert_eq!(rows[1].row.values[1], Value::Text("wal".into()));

    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&seed_dir);
    let _ = std::fs::remove_dir_all(&replica_dir);
}

#[test]
fn lsm_backend_rejects_custom_memtable_flush_until_wired() {
    let data_dir = unique_temp_path("lsm-memtable");
    let error = StorageBackendHandle::open(StorageBackendSpec::Lsm {
        config: LsmBackendConfig {
            memtable_flush_bytes: 8 * 1024 * 1024,
            ..LsmBackendConfig::new(&data_dir)
        },
    })
    .expect_err("custom lsm memtable size should not be accepted yet");

    assert!(error
        .to_string()
        .contains("lsm memtable_flush_bytes is not configurable yet"));
}

#[test]
fn lsm_backend_rejects_unsupported_block_size() {
    let data_dir = unique_temp_path("lsm-block-size");
    let error = StorageBackendHandle::open(StorageBackendSpec::Lsm {
        config: LsmBackendConfig {
            block_size_bytes: 4096,
            ..LsmBackendConfig::new(&data_dir)
        },
    })
    .expect_err("lsm should reject unsupported block sizes");

    assert!(error
        .to_string()
        .contains("lsm currently requires block_size_bytes"));
}
