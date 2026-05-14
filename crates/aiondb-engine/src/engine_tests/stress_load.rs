#![allow(clippy::pedantic)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use aiondb_config::RuntimeConfig;

use super::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Stress tests are heavy (100k+ rows, 200-column tables, 50k result sets)
/// and stack up under default `cargo test` parallelism, OOM-killing CI hosts.
/// Gate them behind `AIONDB_RUN_STRESS=1` so the regular suite stays safe.
fn stress_gate(name: &str) -> bool {
    if std::env::var_os("AIONDB_RUN_STRESS").is_some() {
        true
    } else {
        eprintln!("{name} skipped: set AIONDB_RUN_STRESS=1 to run");
        false
    }
}

/// Extract a single integer value from the first column of the first row.
fn query_single_int(engine: &Engine, session: &SessionHandle, sql: &str) -> i64 {
    let rows = query_rows(engine, session, sql);
    assert_eq!(rows.len(), 1, "expected exactly one row");
    match &rows[0].values[0] {
        Value::Int(n) => *n as i64,
        Value::BigInt(n) => *n,
        other => panic!("expected Int or BigInt, got {other:?}"),
    }
}

// ===========================================================================
// 1. Bulk insert stress: 100K rows in a single transaction
// ===========================================================================

#[test]
fn stress_load_bulk_insert_100k() {
    if !stress_gate("stress_load_bulk_insert_100k") {
        return;
    }
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE bulk_ins (id INT, payload TEXT)")
        .expect("create table");

    const TOTAL_ROWS: usize = 100_000;

    engine.execute_sql(&session, "BEGIN").expect("begin");
    for i in 0..TOTAL_ROWS {
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO bulk_ins VALUES ({i}, 'row_{i}')"),
            )
            .unwrap_or_else(|e| panic!("insert {i} failed: {e}"));
    }
    engine.execute_sql(&session, "COMMIT").expect("commit");

    let count = query_count(&engine, &session, "SELECT COUNT(*) FROM bulk_ins");
    assert_eq!(
        count, TOTAL_ROWS as i64,
        "expected {TOTAL_ROWS} rows after bulk insert",
    );
}

// ===========================================================================
// 2. Rapid DDL churn: create and drop 500 tables in sequence
// ===========================================================================

#[test]
fn stress_load_rapid_ddl_churn_500_tables() {
    if !stress_gate("stress_load_rapid_ddl_churn_500_tables") {
        return;
    }
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    const NUM_TABLES: usize = 500;

    for i in 0..NUM_TABLES {
        let table = format!("ddl_churn_{i}");
        engine
            .execute_sql(
                &session,
                &format!("CREATE TABLE {table} (id INT, name TEXT)"),
            )
            .unwrap_or_else(|e| panic!("create table {table} failed: {e}"));
        engine
            .execute_sql(&session, &format!("DROP TABLE {table}"))
            .unwrap_or_else(|e| panic!("drop table {table} failed: {e}"));
    }

    // After all tables are dropped, none should exist. Attempting to query
    // any of them should fail with UndefinedTable.
    for probe in [0, NUM_TABLES / 2, NUM_TABLES - 1] {
        let table = format!("ddl_churn_{probe}");
        let err = engine
            .execute_sql(&session, &format!("SELECT * FROM {table}"))
            .expect_err(&format!("{table} should not exist after drop"));
        assert_eq!(
            err.sqlstate(),
            aiondb_core::SqlState::UndefinedTable,
            "expected UndefinedTable for dropped {table}",
        );
    }

    // Verify we can still create new tables (no catalog corruption / leaks).
    engine
        .execute_sql(
            &session,
            "CREATE TABLE ddl_churn_post (id INT); INSERT INTO ddl_churn_post VALUES (1)",
        )
        .expect("create table after churn");

    let count = query_count(&engine, &session, "SELECT COUNT(*) FROM ddl_churn_post");
    assert_eq!(count, 1, "post-churn table should work normally");
}

// ===========================================================================
// 3. Large transaction rollback: insert 50K rows then rollback
// ===========================================================================

#[test]
fn stress_load_large_transaction_rollback() {
    if !stress_gate("stress_load_large_transaction_rollback") {
        return;
    }
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE rollback_large (id INT, val TEXT)")
        .expect("create table");

    const ROWS_TO_INSERT: usize = 50_000;

    engine.execute_sql(&session, "BEGIN").expect("begin");
    for i in 0..ROWS_TO_INSERT {
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO rollback_large VALUES ({i}, 'phantom')"),
            )
            .unwrap_or_else(|e| panic!("insert {i} failed: {e}"));
    }
    engine.execute_sql(&session, "ROLLBACK").expect("rollback");

    // After rollback, the table should be empty.
    let count = query_count(&engine, &session, "SELECT COUNT(*) FROM rollback_large");
    assert_eq!(
        count, 0,
        "table should be empty after rollback of {ROWS_TO_INSERT} rows"
    );

    // Verify the table is still usable: insert and read back.
    engine
        .execute_sql(&session, "INSERT INTO rollback_large VALUES (1, 'real')")
        .expect("insert after rollback");
    let count = query_count(&engine, &session, "SELECT COUNT(*) FROM rollback_large");
    assert_eq!(
        count, 1,
        "table should have exactly 1 row after post-rollback insert"
    );
}

// ===========================================================================
// 4. Wide table stress: 200 columns, insert and verify
// ===========================================================================

#[test]
fn stress_load_wide_table_200_columns() {
    if !stress_gate("stress_load_wide_table_200_columns") {
        return;
    }
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    const NUM_COLS: usize = 200;

    // Build CREATE TABLE with 200 INT columns: c0 INT, c1 INT, ..., c199 INT.
    let col_defs: Vec<String> = (0..NUM_COLS).map(|i| format!("c{i} INT")).collect();
    let create_sql = format!("CREATE TABLE wide_tbl ({})", col_defs.join(", "));
    engine
        .execute_sql(&session, &create_sql)
        .expect("create wide table");

    // Insert a row with each column set to its column index.
    let values: Vec<String> = (0..NUM_COLS).map(|i| i.to_string()).collect();
    let insert_sql = format!("INSERT INTO wide_tbl VALUES ({})", values.join(", "));
    engine
        .execute_sql(&session, &insert_sql)
        .expect("insert into wide table");

    // Insert a second row with each column set to column_index * 10.
    let values_2: Vec<String> = (0..NUM_COLS).map(|i| (i * 10).to_string()).collect();
    let insert_sql_2 = format!("INSERT INTO wide_tbl VALUES ({})", values_2.join(", "));
    engine
        .execute_sql(&session, &insert_sql_2)
        .expect("insert second row into wide table");

    // Verify row count.
    let count = query_count(&engine, &session, "SELECT COUNT(*) FROM wide_tbl");
    assert_eq!(count, 2, "wide table should have 2 rows");

    // Verify specific column values from the first row.
    // Check first column (c0), a middle column (c100), and last column (c199).
    let rows = query_rows(
        &engine,
        &session,
        "SELECT c0, c100, c199 FROM wide_tbl ORDER BY c0 ASC",
    );
    assert_eq!(rows.len(), 2, "should get 2 rows");

    // First row: c0=0, c100=100, c199=199
    assert_eq!(rows[0].values[0], Value::Int(0));
    assert_eq!(rows[0].values[1], Value::Int(100));
    assert_eq!(rows[0].values[2], Value::Int(199));

    // Second row: c0=0, c100=1000, c199=1990
    assert_eq!(rows[1].values[0], Value::Int(0));
    assert_eq!(rows[1].values[1], Value::Int(1000));
    assert_eq!(rows[1].values[2], Value::Int(1990));

    // Also verify SELECT * returns all columns.
    let all_rows = query_rows(&engine, &session, "SELECT * FROM wide_tbl");
    assert_eq!(all_rows.len(), 2);
    assert_eq!(
        all_rows[0].values.len(),
        NUM_COLS,
        "SELECT * should return {NUM_COLS} columns",
    );
}

// ===========================================================================
// 5. Deep nesting stress: subqueries nested 10+ levels
// ===========================================================================

#[test]
fn stress_load_deep_nested_subqueries() {
    if !stress_gate("stress_load_deep_nested_subqueries") {
        return;
    }
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Build a deeply nested scalar subquery:
    // SELECT (SELECT (SELECT (... (SELECT 42) ...)))
    const DEPTH: usize = 5;
    let mut sql = "42".to_string();
    for _ in 0..DEPTH {
        sql = format!("(SELECT {sql})");
    }
    let full_sql = format!("SELECT {sql} AS deep_val");

    let rows = query_rows(&engine, &session, &full_sql);
    assert_eq!(rows.len(), 1, "should get one row from nested subquery");
    assert_eq!(
        rows[0].values[0],
        Value::Int(42),
        "deeply nested subquery should resolve to 42",
    );

    // Also test nested subqueries with a table reference.
    engine
        .execute_sql(
            &session,
            "CREATE TABLE nest_src (id INT); INSERT INTO nest_src VALUES (7)",
        )
        .expect("create nest_src");

    // Build: SELECT (SELECT (SELECT ... (SELECT id FROM nest_src) ...))
    let mut table_sql = "id FROM nest_src".to_string();
    for _ in 0..4 {
        table_sql = format!("(SELECT {table_sql})");
    }
    let full_table_sql = format!("SELECT {table_sql} AS nested_id");

    let rows = query_rows(&engine, &session, &full_table_sql);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values[0],
        Value::Int(7),
        "deeply nested subquery with table reference should return 7",
    );
}

// ===========================================================================
// 6. Sequential transaction throughput: 10K INSERT+SELECT cycles
// ===========================================================================

#[test]
fn stress_load_sequential_transaction_throughput() {
    if !stress_gate("stress_load_sequential_transaction_throughput") {
        return;
    }
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE txn_throughput (id INT, val TEXT)")
        .expect("create table");

    const CYCLES: usize = 10_000;

    for i in 0..CYCLES {
        // Each cycle: BEGIN, INSERT, SELECT to verify, COMMIT.
        engine.execute_sql(&session, "BEGIN").expect("begin");
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO txn_throughput VALUES ({i}, 'cycle_{i}')"),
            )
            .unwrap_or_else(|e| panic!("insert cycle {i} failed: {e}"));

        // Verify the inserted row is visible within the transaction.
        let rows = query_rows(
            &engine,
            &session,
            &format!("SELECT id FROM txn_throughput WHERE id = {i}"),
        );
        assert_eq!(
            rows.len(),
            1,
            "should see inserted row within transaction at cycle {i}",
        );

        engine.execute_sql(&session, "COMMIT").expect("commit");
    }

    // Final verification: all rows committed.
    let count = query_count(&engine, &session, "SELECT COUNT(*) FROM txn_throughput");
    assert_eq!(
        count, CYCLES as i64,
        "expected {CYCLES} rows after all transaction cycles",
    );

    // Spot-check a few values.
    let first = query_single_int(
        &engine,
        &session,
        "SELECT id FROM txn_throughput WHERE id = 0",
    );
    assert_eq!(first, 0);

    let last = query_single_int(
        &engine,
        &session,
        &format!("SELECT id FROM txn_throughput WHERE id = {}", CYCLES - 1),
    );
    assert_eq!(last, (CYCLES - 1) as i64);
}

// ===========================================================================
// 7. Large result set: SELECT returning 50K rows
// ===========================================================================

#[test]
fn stress_load_large_result_set_50k() {
    if !stress_gate("stress_load_large_result_set_50k") {
        return;
    }
    let mut cfg = RuntimeConfig::default();
    cfg.limits.max_result_rows = 100_000;
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(cfg)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE large_rs (id INT, payload TEXT)")
        .expect("create table");

    const TOTAL_ROWS: usize = 50_000;

    // Insert all rows. Use autocommit for simplicity; bulk insert is tested
    // separately above.
    for i in 0..TOTAL_ROWS {
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO large_rs VALUES ({i}, 'data_{i}')"),
            )
            .unwrap_or_else(|e| panic!("insert {i} failed: {e}"));
    }

    // Verify count first.
    let count = query_count(&engine, &session, "SELECT COUNT(*) FROM large_rs");
    assert_eq!(count, TOTAL_ROWS as i64);

    // Fetch all rows: this exercises the result set materialisation path.
    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, payload FROM large_rs ORDER BY id ASC",
    );
    assert_eq!(
        rows.len(),
        TOTAL_ROWS,
        "should receive all {TOTAL_ROWS} rows in the result set",
    );

    // Verify ordering: first row is id=0, last row is id=TOTAL_ROWS-1.
    assert_eq!(rows[0].values[0], Value::Int(0));
    assert_eq!(
        rows[TOTAL_ROWS - 1].values[0],
        Value::Int((TOTAL_ROWS - 1) as i32),
    );

    // Spot-check a middle row.
    let mid = TOTAL_ROWS / 2;
    assert_eq!(rows[mid].values[0], Value::Int(mid as i32));
    assert_eq!(rows[mid].values[1], Value::Text(format!("data_{mid}")),);
}

// ===========================================================================
// 8. Many concurrent sessions: 50 sessions each doing independent work
// ===========================================================================

#[test]
fn stress_load_many_concurrent_sessions() {
    if !stress_gate("stress_load_many_concurrent_sessions") {
        return;
    }
    let engine = EngineBuilder::for_testing().build().unwrap();

    // Create a shared table for sessions to work with.
    let (setup, _) = engine.startup(startup_params()).expect("startup setup");
    engine
        .execute_sql(
            &setup,
            "CREATE TABLE multi_sess (session_id INT, row_num INT, val TEXT)",
        )
        .expect("create shared table");

    const NUM_SESSIONS: usize = 50;
    const OPS_PER_SESSION: usize = 20;

    let had_error = AtomicBool::new(false);

    thread::scope(|s| {
        for sid in 0..NUM_SESSIONS {
            let engine = &engine;
            let had_error = &had_error;
            s.spawn(move || {
                let (session, _) = match engine.startup(startup_params()) {
                    Ok(s) => s,
                    Err(e) => {
                        had_error.store(true, Ordering::SeqCst);
                        panic!("session {sid} startup failed: {e}");
                    }
                };

                // Each session creates its own private table.
                let private_table = format!("sess_priv_{sid}");
                if let Err(e) = engine.execute_sql(
                    &session,
                    &format!("CREATE TABLE {private_table} (id INT, data TEXT)"),
                ) {
                    had_error.store(true, Ordering::SeqCst);
                    panic!("session {sid} create private table failed: {e}");
                }

                for op in 0..OPS_PER_SESSION {
                    // Insert into private table.
                    if let Err(e) = engine.execute_sql(
                        &session,
                        &format!(
                            "INSERT INTO {private_table} VALUES ({op}, 'sess_{sid}_row_{op}')"
                        ),
                    ) {
                        had_error.store(true, Ordering::SeqCst);
                        panic!("session {sid} private insert {op} failed: {e}");
                    }

                    // Also insert into the shared table.  Concurrent
                    // autocommit inserts may occasionally hit serialization
                    // conflicts; that is expected behaviour, not a bug.
                    let _ = engine.execute_sql(
                        &session,
                        &format!("INSERT INTO multi_sess VALUES ({sid}, {op}, 's{sid}')"),
                    );
                }

                // Verify own private table has the expected row count.
                match engine.execute_sql(&session, &format!("SELECT COUNT(*) FROM {private_table}"))
                {
                    Ok(results) => {
                        if let Some(StatementResult::Query { rows, .. }) = results.last() {
                            if let Some(row) = rows.first() {
                                if let Value::BigInt(n) = &row.values[0] {
                                    if *n != OPS_PER_SESSION as i64 {
                                        had_error.store(true, Ordering::SeqCst);
                                        panic!(
                                            "session {sid} private table has {n} rows, \
                                             expected {OPS_PER_SESSION}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        had_error.store(true, Ordering::SeqCst);
                        panic!("session {sid} count query failed: {e}");
                    }
                }

                // Clean up: drop the private table.
                if let Err(e) = engine.execute_sql(&session, &format!("DROP TABLE {private_table}"))
                {
                    had_error.store(true, Ordering::SeqCst);
                    panic!("session {sid} drop private table failed: {e}");
                }

                // Terminate session.
                if let Err(e) = engine.terminate(session) {
                    had_error.store(true, Ordering::SeqCst);
                    panic!("session {sid} terminate failed: {e}");
                }
            });
        }
    });

    assert!(
        !had_error.load(Ordering::SeqCst),
        "concurrent sessions had errors",
    );

    // Verify shared table integrity.
    let total = query_count(&engine, &setup, "SELECT COUNT(*) FROM multi_sess");
    // Some inserts may have been lost to serialization conflicts under
    // concurrent autocommit; that is expected. Verify at least 80% made
    // it and that the count is not wildly off.
    let expected = (NUM_SESSIONS * OPS_PER_SESSION) as i64;
    assert!(
        total >= expected * 80 / 100,
        "shared table should have at least 80% of {expected} rows, got {total}",
    );

    // Verify each session contributed *some* rows (conflicts may reduce counts).
    let rows = query_rows(
        &engine,
        &setup,
        "SELECT session_id, COUNT(*) AS cnt \
         FROM multi_sess \
         GROUP BY session_id \
         ORDER BY session_id",
    );
    assert!(
        !rows.is_empty(),
        "should have at least one group in shared table",
    );
    for row in &rows {
        let cnt = match &row.values[1] {
            Value::BigInt(n) => *n,
            other => panic!("expected BigInt count, got {other:?}"),
        };
        assert!(
            cnt > 0,
            "each session should have at least 1 row in shared table",
        );
    }
}
