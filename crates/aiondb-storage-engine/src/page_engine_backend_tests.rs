use super::super::*;
use super::{
    collect_stream, remove_log_files, test_table_descriptor, unique_temp_path, visible_snapshot,
};
use aiondb_core::Value;

#[test]
fn page_engine_backend_handle_reports_kind() {
    let data_dir = unique_temp_path("page-engine");
    let backend = StorageBackendHandle::open(StorageBackendSpec::PageEngine {
        config: PageEngineBackendConfig::new(&data_dir),
    })
    .expect("page_engine backend should open");

    assert_eq!(backend.kind(), StorageBackendKind::PageEngine);
    assert!(backend.supports_durability());

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn page_engine_backend_offloads_committed_rows_and_recovers_after_reopen() {
    let data_dir = unique_temp_path("page-engine-offload");
    let table = test_table_descriptor(RelationId::new(88));
    let config = PageEngineBackendConfig::new(&data_dir);
    let backend = StorageBackendHandle::open(StorageBackendSpec::PageEngine {
        config: config.clone(),
    })
    .expect("page_engine backend should open");

    backend
        .create_table_storage(TxnId::default(), &table)
        .expect("table should be created");

    let tuple_id = backend
        .insert(
            TxnId::default(),
            table.table_id,
            Row::new(vec![
                Value::Int(88),
                Value::Text("page-engine-offload".into()),
            ]),
        )
        .expect("row should be inserted");

    assert_eq!(
        backend
            .fetch(
                TxnId::default(),
                &visible_snapshot(),
                table.table_id,
                tuple_id,
                None
            )
            .expect("fetch should succeed"),
        Some(Row::new(vec![
            Value::Int(88),
            Value::Text("page-engine-offload".into())
        ]))
    );
    assert!(
        data_dir.join("pages").is_dir(),
        "page_engine commits should publish paged snapshot artifacts"
    );
    assert!(
        data_dir.join("table_pages").join("CURRENT").is_file(),
        "page_engine commits should publish paged table artifacts"
    );

    drop(backend);

    remove_log_files(&data_dir);

    let reopened = StorageBackendHandle::open(StorageBackendSpec::PageEngine { config })
        .expect("page_engine backend should reopen");
    let rows = collect_stream(
        reopened
            .scan_table(TxnId::default(), &visible_snapshot(), table.table_id, None)
            .expect("reopened page_engine backend should scan table"),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].tuple_id, tuple_id);
    assert_eq!(rows[0].row.values[0], Value::Int(88));
    assert_eq!(
        rows[0].row.values[1],
        Value::Text("page-engine-offload".into())
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn page_engine_backend_rejects_unsupported_page_size() {
    let data_dir = unique_temp_path("page-engine-page-size");
    let error = StorageBackendHandle::open(StorageBackendSpec::PageEngine {
        config: PageEngineBackendConfig {
            page_size: 4096,
            ..PageEngineBackendConfig::new(&data_dir)
        },
    })
    .expect_err("page_engine should reject unsupported page sizes");

    assert!(error
        .to_string()
        .contains("page_engine currently requires page_size"));
}

#[test]
fn page_engine_backend_rejects_zero_buffer_pool_pages() {
    let data_dir = unique_temp_path("page-engine-buffer-pool");
    let error = StorageBackendHandle::open(StorageBackendSpec::PageEngine {
        config: PageEngineBackendConfig {
            buffer_pool_pages: 0,
            ..PageEngineBackendConfig::new(&data_dir)
        },
    })
    .expect_err("page_engine should reject empty buffer pools");

    assert!(error
        .to_string()
        .contains("page_engine buffer_pool_pages must be >= 1"));
}
