use super::*;
use crate::install_replication_seed;
use aiondb_core::{checksum::compute_crc32c, DataType, Value};
use aiondb_storage_api::{
    IndexKeyColumn, IndexStorageDescriptor, StorageColumn, StorageDDL, StorageDML, TupleRecord,
};
use aiondb_tx::Snapshot;

#[path = "page_engine_backend_tests.rs"]
mod page_engine_tests;

#[path = "backend_tests_lsm.rs"]
mod lsm_tests;

fn unique_temp_path(name: &str) -> PathBuf {
    crate::test_support::unique_temp_path("backend-test", name)
}

fn read_disk_checkpoint_manifest_json(path: &std::path::Path) -> serde_json::Value {
    const MAGIC: &[u8; 8] = b"AIONCKP1";

    let bytes = std::fs::read(path).expect("disk checkpoint manifest should be readable");
    let payload = if bytes.starts_with(MAGIC) {
        let checksum_offset = bytes.len() - 4;
        let stored = u32::from_le_bytes(bytes[checksum_offset..].try_into().unwrap());
        assert_eq!(stored, compute_crc32c(&bytes[..checksum_offset]));
        let payload_len =
            u64::from_le_bytes(bytes[MAGIC.len()..MAGIC.len() + 8].try_into().unwrap()) as usize;
        let payload_start = MAGIC.len() + 8;
        bytes[payload_start..payload_start + payload_len].to_vec()
    } else {
        bytes
    };

    serde_json::from_slice(&payload).expect("disk checkpoint manifest should be valid json")
}

fn test_table_descriptor(table_id: RelationId) -> TableStorageDescriptor {
    TableStorageDescriptor {
        table_id,
        columns: vec![
            StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Int,
                nullable: false,
            },
            StorageColumn {
                column_id: ColumnId::new(2),
                data_type: DataType::Text,
                nullable: true,
            },
        ],
        primary_key: None,
        shard_config: None,
    }
}

fn test_edge_table_descriptor(table_id: RelationId) -> TableStorageDescriptor {
    TableStorageDescriptor {
        table_id,
        columns: vec![
            StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Int,
                nullable: false,
            },
            StorageColumn {
                column_id: ColumnId::new(2),
                data_type: DataType::Int,
                nullable: false,
            },
        ],
        primary_key: None,
        shard_config: None,
    }
}

fn visible_snapshot() -> Snapshot {
    Snapshot::new(TxnId::default(), TxnId::default(), Vec::new())
}

fn test_gin_text_index_descriptor(
    index_id: IndexId,
    table_id: RelationId,
) -> IndexStorageDescriptor {
    IndexStorageDescriptor {
        index_id,
        table_id,
        unique: false,
        nulls_not_distinct: false,
        gin: true,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(2),
            descending: false,
            nulls_first: false,
        }],
        include_columns: Vec::new(),
        hnsw_options: None,
    }
}

fn collect_stream(mut stream: Box<dyn TupleStream>) -> Vec<TupleRecord> {
    let mut records = Vec::new();
    while let Some(record) = stream.next().expect("tuple stream next should succeed") {
        records.push(record);
    }
    records
}

fn remove_log_files(dir: &std::path::Path) {
    for entry in std::fs::read_dir(dir).expect("log dir should be enumerable") {
        let entry = entry.expect("log dir entry should be readable");
        let path = entry.path();
        if path.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    std::path::Path::new(name)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("log"))
                })
        {
            std::fs::remove_file(&path).expect("log segment should be removable");
        }
    }
}

#[test]
fn in_memory_backend_handle_reports_kind() {
    let backend = StorageBackendHandle::open_in_memory(None);
    assert_eq!(backend.kind(), StorageBackendKind::InMemory);
    assert!(!backend.supports_durability());
}

#[test]
fn durable_backend_handle_reports_kind() {
    let data_dir = unique_temp_path("durable");
    let wal_dir = data_dir.join("wal");
    let backend = StorageBackendHandle::open(StorageBackendSpec::durable(StorageOptions::durable(
        aiondb_wal::WalConfig {
            dir: wal_dir,
            wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
            ..aiondb_wal::WalConfig::default()
        },
    )))
    .expect("durable backend should open");

    assert_eq!(backend.kind(), StorageBackendKind::Durable);
    assert!(backend.supports_durability());

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn durable_backend_handle_forwards_limited_gin_search() {
    let data_dir = unique_temp_path("durable-limited-gin");
    let wal_dir = data_dir.join("wal");
    let backend = StorageBackendHandle::open(StorageBackendSpec::durable(StorageOptions::durable(
        aiondb_wal::WalConfig {
            dir: wal_dir,
            wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
            ..aiondb_wal::WalConfig::default()
        },
    )))
    .expect("durable backend should open");
    let table = test_table_descriptor(RelationId::new(77));
    let index = test_gin_text_index_descriptor(IndexId::new(78), table.table_id);

    backend
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    for id in 1..=5 {
        backend
            .insert(
                TxnId::default(),
                table.table_id,
                Row::new(vec![Value::Int(id), Value::Text("body text".into())]),
            )
            .expect("row should be inserted");
    }
    backend
        .create_index_storage(TxnId::default(), &index)
        .expect("GIN index should be created");

    let rows = collect_stream(
        backend
            .gin_containment_search_limited(
                TxnId::default(),
                &visible_snapshot(),
                index.index_id,
                &serde_json::json!({"body": {}, "text": {}}),
                2,
            )
            .expect("limited GIN search should succeed"),
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].row.values[0], Value::Int(1));
    assert_eq!(rows[1].row.values[0], Value::Int(2));

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn durable_backend_handle_delegates_edge_table_registration() {
    let data_dir = unique_temp_path("durable-adjacency");
    let wal_dir = data_dir.join("wal");
    let backend = StorageBackendHandle::open(StorageBackendSpec::durable(StorageOptions::durable(
        aiondb_wal::WalConfig {
            dir: wal_dir,
            wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
            ..aiondb_wal::WalConfig::default()
        },
    )))
    .expect("durable backend should open");

    let table_id = RelationId::new(42);
    backend
        .create_table_storage(TxnId::default(), &test_edge_table_descriptor(table_id))
        .expect("create edge table storage");
    backend.register_edge_table(table_id, 0, 1);
    backend
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![Value::Int(1), Value::Int(2)]),
        )
        .expect("insert edge row");

    let neighbors = backend
        .adjacency_neighbors(
            TxnId::default(),
            &visible_snapshot(),
            table_id,
            &Value::Int(1),
            true,
        )
        .expect("adjacency neighbors should be delegated to durable inner storage");
    assert_eq!(neighbors, vec![Value::Int(2)]);

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn disk_backend_handle_reports_kind() {
    let data_dir = unique_temp_path("disk");
    let backend = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should open");

    assert_eq!(backend.kind(), StorageBackendKind::Disk);
    assert!(backend.supports_durability());

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn disk_backend_creates_distinct_layout_dirs() {
    let data_dir = unique_temp_path("disk-layout");
    StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should open");

    assert!(data_dir.join("wal").is_dir());
    assert!(data_dir.join("checkpoints").is_dir());

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn disk_backend_recovers_from_checkpoint_mirror_when_wal_snapshot_is_missing() {
    let data_dir = unique_temp_path("disk-recover-checkpoint");
    let table = test_table_descriptor(RelationId::new(43));
    let backend = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should open");

    backend
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(11), Value::Text("disk-checkpoint".into())]),
        )
        .expect("row should be inserted");
    backend
        .checkpoint()
        .expect("disk backend checkpoint should succeed");
    drop(backend);

    let wal_snapshot_path = data_dir.join("wal").join("base.snapshot");
    let checkpoint_snapshot_path = data_dir.join("checkpoints").join("base.snapshot");
    assert!(checkpoint_snapshot_path.is_file());
    std::fs::remove_file(&wal_snapshot_path)
        .expect("disk backend wal snapshot fixture should be removable");

    let reopened = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should restore wal snapshot from checkpoint mirror");

    assert!(wal_snapshot_path.is_file());
    let rows = collect_stream(
        reopened
            .scan_table(TxnId::default(), &visible_snapshot(), table.table_id, None)
            .expect("reopened disk backend should scan recovered table"),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row.values[0], Value::Int(11));
    assert_eq!(rows[0].row.values[1], Value::Text("disk-checkpoint".into()));

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn disk_backend_restores_paged_checkpoint_artifacts_when_wal_state_is_missing() {
    let data_dir = unique_temp_path("disk-recover-paged-checkpoint");
    let table = test_table_descriptor(RelationId::new(44));
    let backend = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should open");

    backend
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(22), Value::Text("disk-paged".into())]),
        )
        .expect("row should be inserted");
    backend
        .checkpoint()
        .expect("disk backend checkpoint should succeed");
    drop(backend);

    let wal_dir = data_dir.join("wal");
    let wal_pages_dir = wal_dir.join("pages");
    let wal_table_pages_dir = wal_dir.join("table_pages");
    let checkpoint_pages_dir = data_dir.join("checkpoints").join("pages");
    let checkpoint_table_pages_dir = data_dir.join("checkpoints").join("table_pages");
    assert!(checkpoint_pages_dir.is_dir());
    assert!(checkpoint_table_pages_dir.is_dir());
    assert!(!wal_pages_dir.exists());
    assert!(!wal_table_pages_dir.exists());

    std::fs::remove_file(wal_dir.join("base.snapshot"))
        .expect("disk backend wal snapshot fixture should be removable");
    remove_log_files(&wal_dir);

    let reopened = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should restore paged checkpoint artifacts from mirror");

    assert!(wal_dir.join("base.snapshot").is_file());
    assert!(checkpoint_pages_dir.is_dir());
    assert!(checkpoint_table_pages_dir.is_dir());
    assert!(!wal_pages_dir.exists());
    assert!(!wal_table_pages_dir.exists());
    let rows = collect_stream(
        reopened
            .scan_table(TxnId::default(), &visible_snapshot(), table.table_id, None)
            .expect("reopened disk backend should scan recovered paged table"),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row.values[0], Value::Int(22));
    assert_eq!(rows[0].row.values[1], Value::Text("disk-paged".into()));

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn disk_backend_persists_snapshot_mirror_without_explicit_checkpoint() {
    let data_dir = unique_temp_path("disk-autocommit-snapshot-mirror");
    let table = test_table_descriptor(RelationId::new(441));
    let backend = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should open");

    backend
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(55), Value::Text("disk-autocommit".into())]),
        )
        .expect("row should be inserted");
    drop(backend);

    let wal_dir = data_dir.join("wal");
    let wal_snapshot_path = wal_dir.join("base.snapshot");
    let checkpoint_snapshot_path = data_dir.join("checkpoints").join("base.snapshot");
    assert!(checkpoint_snapshot_path.is_file());
    assert!(!wal_snapshot_path.exists());

    remove_log_files(&wal_dir);

    let reopened = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should recover from snapshot mirror written on commit");

    assert!(wal_snapshot_path.is_file());
    let rows = collect_stream(
        reopened
            .scan_table(TxnId::default(), &visible_snapshot(), table.table_id, None)
            .expect("reopened disk backend should scan mirrored table"),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row.values[0], Value::Int(55));
    assert_eq!(rows[0].row.values[1], Value::Text("disk-autocommit".into()));

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn disk_backend_publishes_checkpoint_manifest_on_commit() {
    let data_dir = unique_temp_path("disk-checkpoint-manifest");
    let table = test_table_descriptor(RelationId::new(443));
    let backend = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should open");

    backend
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(57), Value::Text("disk-manifest".into())]),
        )
        .expect("row should be inserted");
    drop(backend);

    let manifest_path = data_dir.join("checkpoints").join("manifest.json");
    let manifest = read_disk_checkpoint_manifest_json(&manifest_path);

    assert_eq!(manifest["backend"], "disk");
    assert_eq!(manifest["file_snapshot_present"], true);
    assert_eq!(manifest["paged_snapshot_present"], true);
    assert!(manifest["checkpoint_lsn"].as_u64().unwrap_or_default() >= 1);
    assert_eq!(
        manifest["paged_tables_checkpoint_lsn"],
        manifest["checkpoint_lsn"]
    );
    let generations = manifest["generations"]
        .as_array()
        .expect("disk checkpoint manifest generations should be an array");
    assert!(!generations.is_empty());
    assert_eq!(generations[0]["checkpoint_lsn"], manifest["checkpoint_lsn"]);

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn disk_backend_manifest_retains_recent_checkpoint_generations() {
    let data_dir = unique_temp_path("disk-checkpoint-generation-retention");
    let table = test_table_descriptor(RelationId::new(445));
    let backend = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should open");

    backend
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    for value in 0..4 {
        backend
            .insert(
                TxnId::default(),
                table.table_id,
                Row::new(vec![
                    Value::Int(80 + value),
                    Value::Text(format!("generation-{value}")),
                ]),
            )
            .expect("row should be inserted");
    }
    drop(backend);

    let manifest_path = data_dir.join("checkpoints").join("manifest.json");
    let manifest = read_disk_checkpoint_manifest_json(&manifest_path);
    let generations = manifest["generations"]
        .as_array()
        .expect("disk checkpoint manifest generations should be an array");

    assert_eq!(generations.len(), 3);
    assert_eq!(generations[0]["checkpoint_lsn"], manifest["checkpoint_lsn"]);

    let mut manifest_dirs: Vec<String> = generations
        .iter()
        .map(|generation| {
            generation["snapshot_dir"]
                .as_str()
                .expect("generation snapshot_dir should be a string")
                .to_string()
        })
        .collect();
    manifest_dirs.sort();

    let generations_dir = data_dir.join("checkpoints").join("generations");
    let mut on_disk_dirs: Vec<String> = std::fs::read_dir(&generations_dir)
        .expect("disk checkpoint generations dir should be enumerable")
        .map(|entry| entry.expect("generation dir entry should be readable"))
        .filter(|entry| {
            entry
                .file_type()
                .expect("generation dir entry should have a file type")
                .is_dir()
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    on_disk_dirs.sort();

    assert_eq!(on_disk_dirs, manifest_dirs);
    for manifest_dir in &manifest_dirs {
        let generation_dir = generations_dir.join(manifest_dir);
        assert!(generation_dir.join("base.snapshot").is_file());
        assert!(generation_dir.join("pages").is_dir());
        assert!(generation_dir.join("table_pages").is_dir());
    }

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn disk_backend_manifest_prefers_matching_paged_snapshot_over_stale_file_snapshot() {
    let data_dir = unique_temp_path("disk-manifest-stale-file-snapshot");
    let table = test_table_descriptor(RelationId::new(444));
    let backend = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should open");

    backend
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(71), Value::Text("old-snapshot".into())]),
        )
        .expect("first row should be inserted");
    let stale_snapshot_bytes = std::fs::read(data_dir.join("checkpoints").join("base.snapshot"))
        .expect("stale checkpoint snapshot fixture should be readable");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(72), Value::Text("newer-paged".into())]),
        )
        .expect("second row should be inserted");
    drop(backend);

    let wal_dir = data_dir.join("wal");
    let checkpoint_snapshot_path = data_dir.join("checkpoints").join("base.snapshot");
    std::fs::write(&checkpoint_snapshot_path, &stale_snapshot_bytes)
        .expect("stale checkpoint snapshot fixture should be writable");
    remove_log_files(&wal_dir);

    let reopened = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should recover from manifest-selected paged snapshot");

    let rows = collect_stream(
        reopened
            .scan_table(TxnId::default(), &visible_snapshot(), table.table_id, None)
            .expect("reopened disk backend should scan recovered table"),
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].row.values[0], Value::Int(71));
    assert_eq!(rows[1].row.values[0], Value::Int(72));

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn disk_backend_rejects_older_checkpoint_generation_when_current_is_corrupted() {
    let data_dir = unique_temp_path("disk-manifest-generation-fallback");
    let table = test_table_descriptor(RelationId::new(446));
    let backend = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should open");

    backend
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(81), Value::Text("older-generation".into())]),
        )
        .expect("first row should be inserted");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![
                Value::Int(82),
                Value::Text("corrupted-current".into()),
            ]),
        )
        .expect("second row should be inserted");
    drop(backend);

    let manifest =
        read_disk_checkpoint_manifest_json(&data_dir.join("checkpoints").join("manifest.json"));
    let generations = manifest["generations"]
        .as_array()
        .expect("disk checkpoint manifest generations should be an array");
    assert!(generations.len() >= 2);
    let current_generation_dir = generations[0]["snapshot_dir"]
        .as_str()
        .expect("current generation snapshot_dir should be a string");
    let current_generation_snapshot = data_dir
        .join("checkpoints")
        .join("generations")
        .join(current_generation_dir)
        .join("base.snapshot");
    std::fs::write(&current_generation_snapshot, b"corrupted-generation")
        .expect("current checkpoint generation snapshot should be writable");

    let checkpoint_snapshot_path = data_dir.join("checkpoints").join("base.snapshot");
    if checkpoint_snapshot_path.exists() {
        std::fs::remove_file(&checkpoint_snapshot_path)
            .expect("checkpoint snapshot mirror should be removable");
    }

    let wal_dir = data_dir.join("wal");
    let wal_snapshot_path = wal_dir.join("base.snapshot");
    let checkpoint_pages_dir = data_dir.join("checkpoints").join("pages");
    let checkpoint_table_pages_dir = data_dir.join("checkpoints").join("table_pages");
    if wal_snapshot_path.exists() {
        std::fs::remove_file(&wal_snapshot_path)
            .expect("wal snapshot should be removable to force manifest recovery");
    }
    if checkpoint_pages_dir.exists() {
        std::fs::remove_dir_all(&checkpoint_pages_dir)
            .expect("current paged snapshot should be removable to force failure");
    }
    if checkpoint_table_pages_dir.exists() {
        std::fs::remove_dir_all(&checkpoint_table_pages_dir)
            .expect("current paged tables should be removable to force failure");
    }
    remove_log_files(&wal_dir);

    let error = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect_err("disk backend must fail instead of silently falling back to an older checkpoint");
    assert!(
        error.to_string().contains("snapshot")
            || error.to_string().contains("checkpoint")
            || error.to_string().contains("decode")
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn disk_backend_recovers_from_paged_snapshot_when_checkpoint_snapshot_mirror_is_missing() {
    let data_dir = unique_temp_path("disk-paged-snapshot-fallback");
    let table = test_table_descriptor(RelationId::new(442));
    let backend = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should open");

    backend
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![
                Value::Int(66),
                Value::Text("disk-paged-snapshot".into()),
            ]),
        )
        .expect("row should be inserted");
    drop(backend);

    let wal_dir = data_dir.join("wal");
    let wal_snapshot_path = wal_dir.join("base.snapshot");
    let checkpoint_snapshot_path = data_dir.join("checkpoints").join("base.snapshot");
    assert!(checkpoint_snapshot_path.is_file());
    std::fs::remove_file(&checkpoint_snapshot_path)
        .expect("checkpoint snapshot mirror fixture should be removable");
    assert!(!wal_snapshot_path.exists());

    remove_log_files(&wal_dir);

    let reopened = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should fall back to paged snapshot recovery");

    assert!(wal_snapshot_path.is_file());
    let rows = collect_stream(
        reopened
            .scan_table(TxnId::default(), &visible_snapshot(), table.table_id, None)
            .expect("reopened disk backend should scan paged snapshot recovered table"),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row.values[0], Value::Int(66));
    assert_eq!(
        rows[0].row.values[1],
        Value::Text("disk-paged-snapshot".into())
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn disk_backend_migrates_legacy_paged_state_into_checkpoint_root() {
    let data_dir = unique_temp_path("disk-legacy-paged-migration");
    let table = test_table_descriptor(RelationId::new(45));
    let legacy = InMemoryStorage::new(StorageOptions::durable(aiondb_wal::WalConfig {
        dir: data_dir.join("wal"),
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
        ..aiondb_wal::WalConfig::default()
    }))
    .expect("legacy durable storage should open");

    legacy
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    legacy
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(33), Value::Text("legacy-disk".into())]),
        )
        .expect("row should be inserted");
    legacy
        .checkpoint()
        .expect("legacy durable checkpoint should succeed");
    drop(legacy);

    let legacy_pages_dir = data_dir.join("wal").join("pages");
    let legacy_table_pages_dir = data_dir.join("wal").join("table_pages");
    let checkpoint_pages_dir = data_dir.join("checkpoints").join("pages");
    let checkpoint_table_pages_dir = data_dir.join("checkpoints").join("table_pages");
    assert!(legacy_pages_dir.is_dir());
    assert!(legacy_table_pages_dir.is_dir());
    assert!(!checkpoint_pages_dir.exists());
    assert!(!checkpoint_table_pages_dir.exists());

    let disk = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should migrate legacy paged state");

    assert!(checkpoint_pages_dir.is_dir());
    assert!(checkpoint_table_pages_dir.is_dir());
    assert!(checkpoint_pages_dir.join(".legacy_migrated").is_file());
    assert!(checkpoint_table_pages_dir
        .join(".legacy_migrated")
        .is_file());
    let rows = collect_stream(
        disk.scan_table(TxnId::default(), &visible_snapshot(), table.table_id, None)
            .expect("disk backend should scan migrated table"),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row.values[0], Value::Int(33));
    assert_eq!(rows[0].row.values[1], Value::Text("legacy-disk".into()));

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn disk_backend_repairs_interrupted_legacy_paged_migration() {
    let data_dir = unique_temp_path("disk-legacy-paged-migration-repair");
    let table = test_table_descriptor(RelationId::new(451));
    let legacy = InMemoryStorage::new(StorageOptions::durable(aiondb_wal::WalConfig {
        dir: data_dir.join("wal"),
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
        ..aiondb_wal::WalConfig::default()
    }))
    .expect("legacy durable storage should open");

    legacy
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    legacy
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(34), Value::Text("legacy-repair".into())]),
        )
        .expect("row should be inserted");
    legacy
        .checkpoint()
        .expect("legacy durable checkpoint should succeed");
    drop(legacy);

    let checkpoint_pages_dir = data_dir.join("checkpoints").join("pages");
    let checkpoint_table_pages_dir = data_dir.join("checkpoints").join("table_pages");
    std::fs::create_dir_all(&checkpoint_pages_dir)
        .expect("interrupted pages migration fixture should be creatable");
    std::fs::create_dir_all(&checkpoint_table_pages_dir)
        .expect("interrupted table pages migration fixture should be creatable");
    std::fs::write(checkpoint_pages_dir.join("stale.tmp"), b"incomplete")
        .expect("stale pages migration fixture should be writable");
    std::fs::write(checkpoint_table_pages_dir.join("stale.tmp"), b"incomplete")
        .expect("stale table pages migration fixture should be writable");

    let disk = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should repair interrupted migration");

    assert!(checkpoint_pages_dir.join(".legacy_migrated").is_file());
    assert!(checkpoint_table_pages_dir
        .join(".legacy_migrated")
        .is_file());
    assert!(!checkpoint_pages_dir.join("stale.tmp").exists());
    assert!(!checkpoint_table_pages_dir.join("stale.tmp").exists());
    let migrated_table_versions = std::fs::read_dir(&checkpoint_table_pages_dir)
        .expect("checkpoint table pages dir should be enumerable")
        .map(|entry| entry.expect("dir entry should be readable").path())
        .filter(|path| path.is_dir())
        .count();
    assert!(migrated_table_versions >= 1);
    let rows = collect_stream(
        disk.scan_table(TxnId::default(), &visible_snapshot(), table.table_id, None)
            .expect("disk backend should scan repaired migrated table"),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row.values[0], Value::Int(34));
    assert_eq!(rows[0].row.values[1], Value::Text("legacy-repair".into()));

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn disk_backend_rejects_custom_index_shards_until_wired() {
    let data_dir = unique_temp_path("disk-index-shards");
    let error = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig {
            index_shards: 64,
            ..DiskBackendConfig::new(&data_dir)
        },
    })
    .expect_err("custom disk index_shards should not be accepted yet");

    assert!(error
        .to_string()
        .contains("disk backend index_shards is not configurable yet"));
}

#[test]
fn disk_backend_accepts_batched_sync_policy() {
    let data_dir = unique_temp_path("disk-sync-policy");
    let table = test_table_descriptor(RelationId::new(46));
    let backend = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig {
            sync_policy: DiskSyncPolicy::Every(8),
            ..DiskBackendConfig::new(&data_dir)
        },
    })
    .expect("batched disk sync policy should be accepted");

    backend
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(77), Value::Text("disk-every".into())]),
        )
        .expect("row should be inserted");
    drop(backend);

    let reopened = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig {
            sync_policy: DiskSyncPolicy::Every(8),
            ..DiskBackendConfig::new(&data_dir)
        },
    })
    .expect("disk backend with batched sync policy should reopen");

    let rows = collect_stream(
        reopened
            .scan_table(TxnId::default(), &visible_snapshot(), table.table_id, None)
            .expect("reopened disk backend should scan table"),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row.values[0], Value::Int(77));
    assert_eq!(rows[0].row.values[1], Value::Text("disk-every".into()));

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn disk_backend_rejects_every_zero_sync_policy() {
    let data_dir = unique_temp_path("disk-sync-policy-zero");
    let error = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig {
            sync_policy: DiskSyncPolicy::Every(0),
            ..DiskBackendConfig::new(&data_dir)
        },
    })
    .expect_err("Every(0) should be rejected");

    assert!(error
        .to_string()
        .contains("disk backend sync policy Every(0) requires interval >= 1"));
}

#[test]
fn disk_backend_rejects_zero_max_open_files() {
    let data_dir = unique_temp_path("disk-zero-max-open-files");
    let error = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig {
            max_open_files: 0,
            ..DiskBackendConfig::new(&data_dir)
        },
    })
    .expect_err("disk backend should reject zero max_open_files");

    assert!(error
        .to_string()
        .contains("disk backend max_open_files must be >= 1"));
}

#[test]
fn disk_backend_rejects_zero_snapshot_pool_frames() {
    let data_dir = unique_temp_path("disk-zero-snapshot-frames");
    let error = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig {
            buffer_pool: StorageBufferPoolConfig {
                snapshot_frames: 0,
                ..StorageBufferPoolConfig::default()
            },
            ..DiskBackendConfig::new(&data_dir)
        },
    })
    .expect_err("disk backend should reject zero snapshot frames");

    assert!(error
        .to_string()
        .contains("disk backend snapshot buffer pool must be >= 1 frame"));
}

#[test]
fn disk_backend_replication_seed_round_trip_recovers_snapshot_and_wal() {
    let data_dir = unique_temp_path("disk-seed-source");
    let seed_dir = unique_temp_path("disk-seed-export");
    let replica_dir = unique_temp_path("disk-seed-replica");
    let table = test_table_descriptor(RelationId::new(430));
    let backend = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&data_dir),
    })
    .expect("disk backend should open");

    backend
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(1), Value::Text("snapshot".into())]),
        )
        .expect("checkpoint seed row should be inserted");
    backend
        .checkpoint()
        .expect("disk backend checkpoint should succeed");
    backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![Value::Int(2), Value::Text("wal".into())]),
        )
        .expect("post-checkpoint wal row should be inserted");

    let manifest = backend
        .export_replication_seed(&seed_dir)
        .expect("disk replication seed export should succeed");
    assert_eq!(manifest.backend, "disk");
    assert!(seed_dir.join("state").join("wal").is_dir());
    assert!(seed_dir.join("state").join("checkpoints").is_dir());

    install_replication_seed(&seed_dir, &replica_dir)
        .expect("disk replication seed install should succeed");
    std::fs::remove_file(replica_dir.join("wal").join("base.snapshot"))
        .expect("replica disk file snapshot should be removable");

    let reopened = StorageBackendHandle::open(StorageBackendSpec::Disk {
        config: DiskBackendConfig::new(&replica_dir),
    })
    .expect("replica disk backend should reopen");
    let rows = collect_stream(
        reopened
            .scan_table(TxnId::default(), &visible_snapshot(), table.table_id, None)
            .expect("replica disk backend should scan replicated table"),
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].row.values[0], Value::Int(1));
    assert_eq!(rows[0].row.values[1], Value::Text("snapshot".into()));
    assert_eq!(rows[1].row.values[0], Value::Int(2));
    assert_eq!(rows[1].row.values[1], Value::Text("wal".into()));

    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::remove_dir_all(&seed_dir);
    let _ = std::fs::remove_dir_all(&replica_dir);
}
