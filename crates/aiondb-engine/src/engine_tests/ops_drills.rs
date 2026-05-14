#![allow(clippy::pedantic)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use super::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn durable_data_dir(name: &str) -> std::path::PathBuf {
    let dir = crate::test_support::unique_temp_path("engine-tests-ops-drill", name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn build_engine_with_durable_storage(
    data_dir: &std::path::Path,
    catalog: Arc<aiondb_catalog_store::CatalogStore>,
) -> Engine {
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
    build_engine_with_store(catalog, storage)
}

fn build_engine_with_storage_memory_limit(limit_bytes: u64) -> Engine {
    let storage = Arc::new(
        aiondb_storage_engine::InMemoryStorage::new_without_wal_with_memory_limit(Some(
            limit_bytes,
        )),
    );
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    build_engine_with_store(catalog, storage)
}

// ===========================================================================
// A: Restore drills
// ===========================================================================

// ---------------------------------------------------------------------------
// 1. basic_restore_cycle: Create tables, insert data, backup to file,
//    drop tables, restore, verify data
// ---------------------------------------------------------------------------

#[test]
fn basic_restore_cycle() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Create schema with multiple tables and data types.
    engine
        .execute_sql(
            &session,
            "CREATE TABLE ops_users (id INT NOT NULL, name TEXT, active BOOLEAN)",
        )
        .expect("create users");
    engine
        .execute_sql(
            &session,
            "INSERT INTO ops_users VALUES (1, 'Alice', TRUE); \
             INSERT INTO ops_users VALUES (2, 'Bob', FALSE); \
             INSERT INTO ops_users VALUES (3, 'Charlie', TRUE)",
        )
        .expect("insert users");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE ops_products (id INT NOT NULL, price INT)",
        )
        .expect("create products");
    engine
        .execute_sql(
            &session,
            "INSERT INTO ops_products VALUES (10, 100); \
             INSERT INTO ops_products VALUES (20, 200)",
        )
        .expect("insert products");

    // Backup to file.
    let path = unique_relative_backup_path("ops-drill-basic-restore");
    let path_str = path.to_str().unwrap();
    engine
        .execute_sql(&session, &format!("BACKUP DATABASE TO '{path_str}'"))
        .expect("backup");

    // Drop all tables in the original engine.
    engine
        .execute_sql(&session, "DROP TABLE ops_users; DROP TABLE ops_products")
        .expect("drop tables");

    // Restore into a fresh engine.
    let engine2 = EngineBuilder::for_testing().build().unwrap();
    let (session2, _) = engine2.startup(startup_params()).expect("startup2");
    engine2
        .execute_sql(&session2, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect("restore");

    // Verify users data.
    let user_count = query_count(&engine2, &session2, "SELECT COUNT(*) FROM ops_users");
    assert_eq!(user_count, 3, "expected 3 users after restore");

    let rows = query_rows(
        &engine2,
        &session2,
        "SELECT id, name, active FROM ops_users ORDER BY id",
    );
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[0].values[1], Value::Text("Alice".to_owned()));
    assert_eq!(rows[0].values[2], Value::Boolean(true));
    assert_eq!(rows[2].values[0], Value::Int(3));

    // Verify products data.
    let product_count = query_count(&engine2, &session2, "SELECT COUNT(*) FROM ops_products");
    assert_eq!(product_count, 2, "expected 2 products after restore");

    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// 2. restore_idempotency: Backup, restore into 2 separate fresh engines,
//    verify both have identical data
// ---------------------------------------------------------------------------

#[test]
fn restore_idempotency() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE idem_tbl (id INT NOT NULL, val TEXT, PRIMARY KEY (id))",
        )
        .expect("create");
    for i in 0..20 {
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO idem_tbl VALUES ({i}, 'value_{i}')"),
            )
            .expect("insert");
    }

    let path = unique_relative_backup_path("ops-drill-idempotency");
    let path_str = path.to_str().unwrap();
    engine
        .execute_sql(&session, &format!("BACKUP DATABASE TO '{path_str}'"))
        .expect("backup");

    // Restore into two independent engines.
    let engine_a = EngineBuilder::for_testing().build().unwrap();
    let (session_a, _) = engine_a.startup(startup_params()).expect("startup A");
    engine_a
        .execute_sql(&session_a, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect("restore A");

    let engine_b = EngineBuilder::for_testing().build().unwrap();
    let (session_b, _) = engine_b.startup(startup_params()).expect("startup B");
    engine_b
        .execute_sql(&session_b, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect("restore B");

    // Both should have identical row counts.
    let count_a = query_count(&engine_a, &session_a, "SELECT COUNT(*) FROM idem_tbl");
    let count_b = query_count(&engine_b, &session_b, "SELECT COUNT(*) FROM idem_tbl");
    assert_eq!(count_a, 20, "engine A should have 20 rows");
    assert_eq!(count_b, 20, "engine B should have 20 rows");

    // Compare actual data row by row.
    let rows_a = query_rows(
        &engine_a,
        &session_a,
        "SELECT id, val FROM idem_tbl ORDER BY id",
    );
    let rows_b = query_rows(
        &engine_b,
        &session_b,
        "SELECT id, val FROM idem_tbl ORDER BY id",
    );
    assert_eq!(rows_a.len(), rows_b.len(), "row counts must match");
    for (ra, rb) in rows_a.iter().zip(rows_b.iter()) {
        assert_eq!(ra.values, rb.values, "row data must be identical");
    }

    let _ = std::fs::remove_file(&path);
}

// ===========================================================================
// B: Brutal restart simulation
// ===========================================================================

// ---------------------------------------------------------------------------
// 3. drop_and_recreate_engine: Create engine, insert data, drop it,
//    create new engine, verify clean state
// ---------------------------------------------------------------------------

#[test]
fn drop_and_recreate_engine() {
    // First engine: create a table, insert data.
    {
        let engine = EngineBuilder::for_testing().build().unwrap();
        let (session, _) = engine.startup(startup_params()).expect("startup");
        engine
            .execute_sql(&session, "CREATE TABLE restart_tbl (id INT, val TEXT)")
            .expect("create");
        engine
            .execute_sql(
                &session,
                "INSERT INTO restart_tbl VALUES (1, 'data'); \
                 INSERT INTO restart_tbl VALUES (2, 'more_data')",
            )
            .expect("insert");

        let count = query_count(&engine, &session, "SELECT COUNT(*) FROM restart_tbl");
        assert_eq!(count, 2, "should have 2 rows before drop");
        // Engine drops here.
    }

    // Second engine: must be completely clean.
    {
        let engine = EngineBuilder::for_testing().build().unwrap();
        let (session, _) = engine.startup(startup_params()).expect("startup2");

        let err = engine
            .execute_sql(&session, "SELECT * FROM restart_tbl")
            .expect_err("table should not exist in new engine");
        assert_eq!(
            err.sqlstate(),
            aiondb_core::SqlState::UndefinedTable,
            "expected UndefinedTable, got {err:?}",
        );

        // Creating a new table should work fine.
        engine
            .execute_sql(&session, "CREATE TABLE fresh_tbl (x INT)")
            .expect("fresh engine should accept DDL");
        engine
            .execute_sql(&session, "INSERT INTO fresh_tbl VALUES (42)")
            .expect("fresh engine should accept DML");
        let count = query_count(&engine, &session, "SELECT COUNT(*) FROM fresh_tbl");
        assert_eq!(count, 1, "fresh engine should have 1 row");
    }
}

#[test]
fn reopened_durable_storage_recovers_committed_state() {
    let data_dir = durable_data_dir("restart_recovers_committed_state");
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());

    {
        let engine = build_engine_with_durable_storage(&data_dir, catalog.clone());
        let (session, _) = engine.startup(startup_params()).expect("startup");

        engine
            .execute_sql(
                &session,
                "CREATE TABLE durable_restart_tbl (id INT, val TEXT)",
            )
            .expect("create durable table");
        engine
            .execute_sql(
                &session,
                "INSERT INTO durable_restart_tbl VALUES (1, 'alpha'); \
                 INSERT INTO durable_restart_tbl VALUES (2, 'beta')",
            )
            .expect("insert durable rows");

        let count = query_count(
            &engine,
            &session,
            "SELECT COUNT(*) FROM durable_restart_tbl",
        );
        assert_eq!(count, 2, "expected durable rows before restart");
    }

    {
        let engine = build_engine_with_durable_storage(&data_dir, catalog);
        let (session, _) = engine.startup(startup_params()).expect("restart startup");

        let rows = query_rows(
            &engine,
            &session,
            "SELECT id, val FROM durable_restart_tbl ORDER BY id",
        );
        assert_eq!(rows.len(), 2, "expected rows after durable restart");
        assert_eq!(
            rows[0].values,
            vec![Value::Int(1), Value::Text("alpha".into())]
        );
        assert_eq!(
            rows[1].values,
            vec![Value::Int(2), Value::Text("beta".into())]
        );
    }

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn reopened_durable_storage_discards_uncommitted_rows() {
    let data_dir = durable_data_dir("restart_discards_uncommitted_rows");
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());

    {
        let engine = build_engine_with_durable_storage(&data_dir, catalog.clone());
        let (session, _) = engine.startup(startup_params()).expect("startup");

        engine
            .execute_sql(
                &session,
                "CREATE TABLE durable_uncommitted_tbl (id INT, val TEXT); \
                 INSERT INTO durable_uncommitted_tbl VALUES (1, 'committed')",
            )
            .expect("seed durable table");

        engine
            .begin_transaction(&session, aiondb_tx::IsolationLevel::ReadCommitted)
            .expect("begin transaction");
        engine
            .execute_sql(
                &session,
                "INSERT INTO durable_uncommitted_tbl VALUES (2, 'should_not_survive')",
            )
            .expect("insert uncommitted row");
        // Drop engine without COMMIT to simulate abrupt stop with in-flight work.
    }

    {
        let engine = build_engine_with_durable_storage(&data_dir, catalog);
        let (session, _) = engine.startup(startup_params()).expect("restart startup");

        let rows = query_rows(
            &engine,
            &session,
            "SELECT id, val FROM durable_uncommitted_tbl ORDER BY id",
        );
        assert_eq!(rows.len(), 1, "only committed row should survive restart");
        assert_eq!(
            rows[0].values,
            vec![Value::Int(1), Value::Text("committed".into())]
        );
    }

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn reopened_durable_storage_preserves_index_updates_without_stale_keys() {
    let data_dir = durable_data_dir("restart_preserves_index_updates");
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());

    {
        let engine = build_engine_with_durable_storage(&data_dir, catalog.clone());
        let (session, _) = engine.startup(startup_params()).expect("startup");

        engine
            .execute_sql(
                &session,
                "CREATE TABLE durable_index_tbl (id INT, val TEXT); \
                 CREATE INDEX durable_index_tbl_id_idx ON durable_index_tbl (id); \
                 INSERT INTO durable_index_tbl VALUES (1, 'one'); \
                 INSERT INTO durable_index_tbl VALUES (2, 'two'); \
                 UPDATE durable_index_tbl SET id = 3, val = 'three' WHERE id = 2; \
                 DELETE FROM durable_index_tbl WHERE id = 1",
            )
            .expect("create and mutate indexed durable table");

        let rows = query_rows(
            &engine,
            &session,
            "SELECT id, val FROM durable_index_tbl WHERE id = 3",
        );
        assert_eq!(
            rows.len(),
            1,
            "updated indexed row should be visible before restart"
        );
    }

    {
        let engine = build_engine_with_durable_storage(&data_dir, catalog);
        let (session, _) = engine.startup(startup_params()).expect("restart startup");

        let stale_update_rows = query_rows(
            &engine,
            &session,
            "SELECT id, val FROM durable_index_tbl WHERE id = 2",
        );
        assert!(
            stale_update_rows.is_empty(),
            "old indexed key from UPDATE must not survive restart"
        );

        let stale_delete_rows = query_rows(
            &engine,
            &session,
            "SELECT id, val FROM durable_index_tbl WHERE id = 1",
        );
        assert!(
            stale_delete_rows.is_empty(),
            "deleted indexed key must not survive restart"
        );

        let rows = query_rows(
            &engine,
            &session,
            "SELECT id, val FROM durable_index_tbl WHERE id = 3",
        );
        assert_eq!(rows.len(), 1, "updated indexed row should survive restart");
        assert_eq!(
            rows[0].values,
            vec![Value::Int(3), Value::Text("three".into())]
        );
    }

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn backup_database_writes_manifest_header_and_checksum() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE backup_manifest_tbl (id INT NOT NULL, name TEXT); \
             INSERT INTO backup_manifest_tbl VALUES (1, 'alpha')",
        )
        .expect("setup");

    let path = unique_relative_backup_path("ops-drill-backup-manifest");
    let path_str = path.to_str().unwrap();
    engine
        .execute_sql(&session, &format!("BACKUP DATABASE TO '{path_str}'"))
        .expect("backup");

    let backup = std::fs::read_to_string(
        std::env::current_dir()
            .expect("current dir")
            .join("backups")
            .join(&path),
    )
    .expect("read backup");
    assert!(
        backup.starts_with("-- AionDB Backup\n"),
        "backup must start with banner"
    );
    assert!(
        backup.contains("-- backup-format-version: 2\n"),
        "backup must include format version"
    );
    assert!(
        backup.contains("-- backup-payload-sha256: "),
        "backup must include payload checksum"
    );

    let _ = std::fs::remove_file(
        std::env::current_dir()
            .expect("current dir")
            .join("backups")
            .join(&path),
    );
}

// ---------------------------------------------------------------------------
// 4. rapid_engine_lifecycle: Create/populate/drop engine 10 times rapidly,
//    verify no panics
// ---------------------------------------------------------------------------

#[test]
fn rapid_engine_lifecycle() {
    const CYCLES: usize = 10;
    const ROWS_PER_CYCLE: usize = 50;

    for cycle in 0..CYCLES {
        let engine = EngineBuilder::for_testing().build().unwrap();
        let (session, _) = engine
            .startup(startup_params())
            .unwrap_or_else(|e| panic!("cycle {cycle} startup failed: {e}"));

        engine
            .execute_sql(&session, "CREATE TABLE lifecycle_tbl (id INT, val TEXT)")
            .unwrap_or_else(|e| panic!("cycle {cycle} create failed: {e}"));

        for i in 0..ROWS_PER_CYCLE {
            engine
                .execute_sql(
                    &session,
                    &format!("INSERT INTO lifecycle_tbl VALUES ({i}, 'cycle_{cycle}')"),
                )
                .unwrap_or_else(|e| panic!("cycle {cycle} insert {i} failed: {e}"));
        }

        let count = query_count(&engine, &session, "SELECT COUNT(*) FROM lifecycle_tbl");
        assert_eq!(
            count, ROWS_PER_CYCLE as i64,
            "cycle {cycle}: expected {ROWS_PER_CYCLE} rows, got {count}",
        );

        // Engine drops at end of loop iteration.
    }
}

// ===========================================================================
// C: Memory saturation
// ===========================================================================

// ---------------------------------------------------------------------------
// 5. memory_limit_enforcement: Configure engine with storage-level memory
//    limits, insert until ProgramLimitExceeded
// ---------------------------------------------------------------------------

#[test]
fn memory_limit_enforcement() {
    // Use a very tight storage-level memory limit.
    let engine = build_engine_with_storage_memory_limit(4096);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE mem_sat (id INT, payload TEXT)")
        .expect("create table");

    let mut hit_limit = false;
    let big_payload = "x".repeat(256);
    for i in 0..10_000 {
        let sql = format!("INSERT INTO mem_sat VALUES ({i}, '{big_payload}')");
        match engine.execute_sql(&session, &sql) {
            Ok(_) => {}
            Err(e) => {
                assert_eq!(
                    e.sqlstate(),
                    aiondb_core::SqlState::ProgramLimitExceeded,
                    "expected ProgramLimitExceeded, got {e:?}",
                );
                hit_limit = true;
                break;
            }
        }
    }

    assert!(hit_limit, "should have hit storage memory limit");

    // The engine should still be usable for reads after hitting the limit.
    let count = query_count(&engine, &session, "SELECT COUNT(*) FROM mem_sat");
    assert!(
        count > 0,
        "some rows should have been inserted before limit"
    );
}

// ---------------------------------------------------------------------------
// 6. memory_pressure_concurrent: Multiple threads approaching memory limit,
//    verify errors not panics
// ---------------------------------------------------------------------------

#[test]
fn memory_pressure_concurrent() {
    let engine = build_engine_with_storage_memory_limit(8192);
    let (setup, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&setup, "CREATE TABLE mem_press (id INT, payload TEXT)")
        .expect("create table");

    const THREADS: usize = 4;
    const INSERTS_PER_THREAD: usize = 500;

    let had_panic = AtomicBool::new(false);
    let big_payload = "y".repeat(128);

    thread::scope(|s| {
        for t in 0..THREADS {
            let engine = &engine;
            let had_panic = &had_panic;
            let big_payload = &big_payload;
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup");
                for i in 0..INSERTS_PER_THREAD {
                    let id = t * INSERTS_PER_THREAD + i;
                    let sql = format!("INSERT INTO mem_press VALUES ({id}, '{big_payload}')");
                    match engine.execute_sql(&session, &sql) {
                        Ok(_) => {}
                        Err(e) => {
                            // Memory limit errors are expected; anything else
                            // is unexpected.
                            let state = e.sqlstate();
                            if state != aiondb_core::SqlState::ProgramLimitExceeded
                                && state != aiondb_core::SqlState::InternalError
                            {
                                had_panic.store(true, Ordering::SeqCst);
                                panic!("thread {t} insert {i}: unexpected error {e:?}");
                            }
                        }
                    }
                }
            });
        }
    });

    assert!(
        !had_panic.load(Ordering::SeqCst),
        "memory pressure caused unexpected errors",
    );

    // Engine should still be functional for reads.
    let count = query_count(&engine, &setup, "SELECT COUNT(*) FROM mem_press");
    assert!(count >= 0, "should be able to read after memory pressure");
}

// ===========================================================================
// D: Connection exhaustion
// ===========================================================================

// ---------------------------------------------------------------------------
// 7. max_sessions_test: Create 50 sessions, verify all work,
//    terminate all, verify cleanup
// ---------------------------------------------------------------------------

#[test]
fn max_sessions_test() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    // Create a table using a setup session.
    let (setup, _) = engine.startup(startup_params()).expect("setup");
    engine
        .execute_sql(&setup, "CREATE TABLE session_test (id INT)")
        .expect("create");
    engine
        .execute_sql(&setup, "INSERT INTO session_test VALUES (1)")
        .expect("insert");

    const NUM_SESSIONS: usize = 50;
    let mut sessions = Vec::with_capacity(NUM_SESSIONS);

    // Create 50 sessions and verify each can query.
    for i in 0..NUM_SESSIONS {
        let (session, _) = engine
            .startup(startup_params())
            .unwrap_or_else(|e| panic!("session {i} startup failed: {e}"));

        let count = query_count(&engine, &session, "SELECT COUNT(*) FROM session_test");
        assert_eq!(count, 1, "session {i} should see 1 row");

        sessions.push(session);
    }

    // All sessions should be counted (plus the setup session).
    let session_count = engine.session_count().expect("session_count");
    assert_eq!(
        session_count,
        NUM_SESSIONS + 1,
        "should have {num} active sessions (50 + setup)",
        num = NUM_SESSIONS + 1,
    );

    // Terminate all 50 sessions.
    for (i, session) in sessions.into_iter().enumerate() {
        engine
            .terminate(session)
            .unwrap_or_else(|e| panic!("session {i} terminate failed: {e}"));
    }

    // Only the setup session should remain.
    let remaining = engine.session_count().expect("session_count after cleanup");
    assert_eq!(
        remaining, 1,
        "only setup session should remain after termination"
    );
}

// ---------------------------------------------------------------------------
// 8. session_churn_rapid: Rapidly create/destroy sessions,
//    verify no resource leaks
// ---------------------------------------------------------------------------

#[test]
fn session_churn_rapid() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (setup, _) = engine.startup(startup_params()).expect("setup");

    engine
        .execute_sql(&setup, "CREATE TABLE churn_data (id INT)")
        .expect("create");
    engine
        .execute_sql(&setup, "INSERT INTO churn_data VALUES (1)")
        .expect("insert");

    const CHURN_CYCLES: usize = 200;

    for i in 0..CHURN_CYCLES {
        let (session, _) = engine
            .startup(startup_params())
            .unwrap_or_else(|e| panic!("churn {i} startup: {e}"));

        // Execute a query to ensure the session is fully functional.
        let count = query_count(&engine, &session, "SELECT COUNT(*) FROM churn_data");
        assert_eq!(count, 1, "churn {i}: expected 1 row");

        engine
            .terminate(session)
            .unwrap_or_else(|e| panic!("churn {i} terminate: {e}"));
    }

    // After all churn, only the setup session should remain.
    let remaining = engine.session_count().expect("session_count");
    assert_eq!(
        remaining, 1,
        "after {CHURN_CYCLES} churn cycles, only setup session should remain",
    );
}

// ---------------------------------------------------------------------------
// 9. concurrent_session_storm: 8 threads creating 20 sessions each
//    simultaneously
// ---------------------------------------------------------------------------

#[test]
fn concurrent_session_storm() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (setup, _) = engine.startup(startup_params()).expect("setup");

    engine
        .execute_sql(&setup, "CREATE TABLE storm_data (id INT)")
        .expect("create");
    engine
        .execute_sql(&setup, "INSERT INTO storm_data VALUES (42)")
        .expect("insert");

    const THREADS: usize = 8;
    const SESSIONS_PER_THREAD: usize = 20;

    let had_error = AtomicBool::new(false);

    thread::scope(|s| {
        for t in 0..THREADS {
            let engine = &engine;
            let had_error = &had_error;
            s.spawn(move || {
                let mut thread_sessions = Vec::with_capacity(SESSIONS_PER_THREAD);
                for i in 0..SESSIONS_PER_THREAD {
                    let (session, _) = match engine.startup(startup_params()) {
                        Ok(s) => s,
                        Err(e) => {
                            had_error.store(true, Ordering::SeqCst);
                            panic!("thread {t} session {i} startup: {e}");
                        }
                    };

                    // Verify the session works.
                    match engine.execute_sql(&session, "SELECT COUNT(*) FROM storm_data") {
                        Ok(results) => {
                            if let Some(StatementResult::Query { rows, .. }) = results.last() {
                                if let Some(row) = rows.first() {
                                    if let Value::BigInt(n) = &row.values[0] {
                                        if *n != 1 {
                                            had_error.store(true, Ordering::SeqCst);
                                            panic!(
                                                "thread {t} session {i}: \
                                                 expected 1 row, got {n}"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            had_error.store(true, Ordering::SeqCst);
                            panic!("thread {t} session {i} query failed: {e}");
                        }
                    }

                    thread_sessions.push(session);
                }

                // Terminate all sessions from this thread.
                for (i, session) in thread_sessions.into_iter().enumerate() {
                    if let Err(e) = engine.terminate(session) {
                        had_error.store(true, Ordering::SeqCst);
                        panic!("thread {t} session {i} terminate: {e}");
                    }
                }
            });
        }
    });

    assert!(
        !had_error.load(Ordering::SeqCst),
        "concurrent session storm had errors",
    );

    // After the storm, only the setup session should remain.
    let remaining = engine.session_count().expect("session_count");
    assert_eq!(
        remaining, 1,
        "only setup session should remain after concurrent storm",
    );
}
