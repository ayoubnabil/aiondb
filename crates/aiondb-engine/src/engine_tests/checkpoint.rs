use std::sync::Arc;

use super::*;

/// Build a directory for a durable storage test. Deletes any existing content
/// so tests are deterministic across runs.
fn durable_data_dir(name: &str) -> std::path::PathBuf {
    let dir = crate::test_support::unique_temp_path("engine-tests-checkpoint", name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Build an engine backed by WAL-durable in-memory storage so that
/// `CHECKPOINT` can actually flush the WAL (the no-WAL in-memory backend
/// rejects checkpoints by design).
fn build_engine_with_durable_storage(data_dir: &std::path::Path) -> Engine {
    let storage = Arc::new(
        aiondb_storage_engine::InMemoryStorage::new(
            aiondb_storage_engine::StorageOptions::durable(aiondb_storage_engine::WalConfig {
                dir: data_dir.join("wal"),
                wal_lsn_mode: aiondb_storage_engine::WalLsnMode::Logical,
                ..aiondb_storage_engine::WalConfig::default()
            }),
        )
        .expect("open durable storage"),
    );
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    build_engine_with_store(catalog, storage)
}

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------

/// `CHECKPOINT` on a WAL-backed engine must succeed with the
/// `CHECKPOINT` command tag and zero rows affected.
#[test]
fn checkpoint_on_durable_engine_succeeds() {
    let data_dir = durable_data_dir("succeeds");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let engine = build_engine_with_durable_storage(&data_dir);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "CHECKPOINT")
        .expect("checkpoint must succeed on durable storage");

    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CHECKPOINT".to_owned(),
            rows_affected: 0,
        }]
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

/// `CHECKPOINT` after committed writes must still succeed; the WAL segment
/// advance is observable because subsequent checkpoints remain durable.
#[test]
fn checkpoint_after_writes_succeeds() {
    let data_dir = durable_data_dir("after_writes");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let engine = build_engine_with_durable_storage(&data_dir);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE chkpt_t (id INT NOT NULL, name TEXT); \
             INSERT INTO chkpt_t VALUES (1, 'a'), (2, 'b')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "CHECKPOINT")
        .expect("checkpoint after writes");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CHECKPOINT".to_owned(),
            rows_affected: 0,
        }]
    );

    // A second checkpoint with no new writes must still succeed.
    let results = engine
        .execute_sql(&session, "CHECKPOINT")
        .expect("idempotent checkpoint");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CHECKPOINT".to_owned(),
            rows_affected: 0,
        }]
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

/// `CHECKPOINT` inside an open transaction block must fail, matching
/// `PostgreSQL` behavior (`ObjectNotInPrerequisiteState`).
#[test]
fn checkpoint_inside_transaction_block_rejected() {
    let data_dir = durable_data_dir("inside_txn");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let engine = build_engine_with_durable_storage(&data_dir);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin txn");

    let error = engine
        .execute_sql(&session, "CHECKPOINT")
        .expect_err("checkpoint inside txn must fail");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ObjectNotInPrerequisiteState,
    );

    // Clean up: rollback so subsequent drop is tidy.
    engine.execute_sql(&session, "ROLLBACK").expect("rollback");

    let _ = std::fs::remove_dir_all(&data_dir);
}

/// `CHECKPOINT` on the no-WAL default testing engine must surface the
/// storage engine's "WAL-backed durable storage" requirement, proving
/// engine's checkpoint path.
#[test]
fn checkpoint_without_wal_surfaces_real_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "CHECKPOINT")
        .expect_err("checkpoint without WAL must fail");

    let rendered = format!("{error}");
    assert!(
        rendered.contains("checkpoint requires WAL-backed durable storage"),
        "unexpected error: {rendered}",
    );
}
