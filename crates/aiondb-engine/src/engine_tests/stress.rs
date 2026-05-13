#![allow(clippy::pedantic)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use super::*;

// ---------------------------------------------------------------------------
// 1. Concurrent inserts into the same table
// ---------------------------------------------------------------------------

#[test]
fn stress_concurrent_inserts_same_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (setup, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&setup, "CREATE TABLE stress_same (id INT, val TEXT)")
        .expect("create table");

    const THREADS: usize = 8;
    const ROWS_PER_THREAD: usize = 100;

    thread::scope(|s| {
        for t in 0..THREADS {
            let engine = &engine;
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup");
                for i in 0..ROWS_PER_THREAD {
                    let id = t * ROWS_PER_THREAD + i;
                    engine
                        .execute_sql(
                            &session,
                            &format!("INSERT INTO stress_same VALUES ({id}, 'thread_{t}')"),
                        )
                        .unwrap_or_else(|e| panic!("thread {t} insert {i} failed: {e}"));
                }
            });
        }
    });

    let count = query_count(&engine, &setup, "SELECT COUNT(*) FROM stress_same");
    assert_eq!(
        count,
        (THREADS * ROWS_PER_THREAD) as i64,
        "expected exactly {} rows",
        THREADS * ROWS_PER_THREAD,
    );
}

// ---------------------------------------------------------------------------
// 2. Concurrent inserts into different tables
// ---------------------------------------------------------------------------

#[test]
fn stress_concurrent_inserts_different_tables() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (setup, _) = engine.startup(startup_params()).expect("startup");

    const THREADS: usize = 4;
    const ROWS_PER_THREAD: usize = 200;

    for t in 0..THREADS {
        engine
            .execute_sql(
                &setup,
                &format!("CREATE TABLE stress_diff_{t} (id INT, val TEXT)"),
            )
            .expect("create table");
    }

    thread::scope(|s| {
        for t in 0..THREADS {
            let engine = &engine;
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup");
                for i in 0..ROWS_PER_THREAD {
                    engine
                        .execute_sql(
                            &session,
                            &format!("INSERT INTO stress_diff_{t} VALUES ({i}, 'val_{i}')"),
                        )
                        .unwrap_or_else(|e| panic!("thread {t} insert {i} failed: {e}"));
                }
            });
        }
    });

    for t in 0..THREADS {
        let count = query_count(
            &engine,
            &setup,
            &format!("SELECT COUNT(*) FROM stress_diff_{t}"),
        );
        assert_eq!(
            count, ROWS_PER_THREAD as i64,
            "table stress_diff_{t} should have {ROWS_PER_THREAD} rows",
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Concurrent reads during writes
// ---------------------------------------------------------------------------

#[test]
fn stress_concurrent_reads_during_writes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (setup, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&setup, "CREATE TABLE stress_rw (id INT, val TEXT)")
        .expect("create table");

    const INITIAL_ROWS: usize = 50;
    for i in 0..INITIAL_ROWS {
        engine
            .execute_sql(
                &setup,
                &format!("INSERT INTO stress_rw VALUES ({i}, 'init')"),
            )
            .expect("seed insert");
    }

    const WRITER_THREADS: usize = 2;
    const ROWS_PER_WRITER: usize = 100;
    const READER_THREADS: usize = 4;
    const READS_PER_READER: usize = 50;

    let had_error = AtomicBool::new(false);

    thread::scope(|s| {
        // Writers
        for w in 0..WRITER_THREADS {
            let engine = &engine;
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup writer");
                for i in 0..ROWS_PER_WRITER {
                    let id = 1000 + w * ROWS_PER_WRITER + i;
                    engine
                        .execute_sql(
                            &session,
                            &format!("INSERT INTO stress_rw VALUES ({id}, 'w{w}')"),
                        )
                        .unwrap_or_else(|e| panic!("writer {w} insert {i} failed: {e}"));
                }
            });
        }

        // Readers
        for r in 0..READER_THREADS {
            let engine = &engine;
            let had_error = &had_error;
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup reader");
                for _ in 0..READS_PER_READER {
                    match engine.execute_sql(&session, "SELECT COUNT(*) FROM stress_rw") {
                        Ok(results) => {
                            if let Some(StatementResult::Query { rows, .. }) = results.last() {
                                if let Some(row) = rows.first() {
                                    if let Value::BigInt(n) = &row.values[0] {
                                        if *n < INITIAL_ROWS as i64 {
                                            had_error.store(true, Ordering::SeqCst);
                                            panic!(
                                                "reader {r}: count {n} < initial {INITIAL_ROWS}"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            // Read errors should not happen, but if they do, record them.
                            had_error.store(true, Ordering::SeqCst);
                            panic!("reader {r} error: {e}");
                        }
                    }
                }
            });
        }
    });

    assert!(
        !had_error.load(Ordering::SeqCst),
        "readers observed inconsistent state",
    );

    // After all writers finish, verify total count.
    let final_count = query_count(&engine, &setup, "SELECT COUNT(*) FROM stress_rw");
    let expected = (INITIAL_ROWS + WRITER_THREADS * ROWS_PER_WRITER) as i64;
    assert_eq!(final_count, expected, "final row count mismatch");
}

// ---------------------------------------------------------------------------
// 4. Rapid transaction cycles (BEGIN -> INSERT -> COMMIT)
// ---------------------------------------------------------------------------

#[test]
fn stress_rapid_transaction_cycles() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (setup, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&setup, "CREATE TABLE stress_txn (id INT, tid INT)")
        .expect("create table");

    const THREADS: usize = 4;
    const CYCLES: usize = 50;

    thread::scope(|s| {
        for t in 0..THREADS {
            let engine = &engine;
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup");
                for c in 0..CYCLES {
                    let id = t * CYCLES + c;
                    engine
                        .execute_sql(&session, "BEGIN")
                        .unwrap_or_else(|e| panic!("thread {t} cycle {c} BEGIN: {e}"));
                    engine
                        .execute_sql(
                            &session,
                            &format!("INSERT INTO stress_txn VALUES ({id}, {t})"),
                        )
                        .unwrap_or_else(|e| panic!("thread {t} cycle {c} INSERT: {e}"));
                    engine
                        .execute_sql(&session, "COMMIT")
                        .unwrap_or_else(|e| panic!("thread {t} cycle {c} COMMIT: {e}"));
                }
            });
        }
    });

    let count = query_count(&engine, &setup, "SELECT COUNT(*) FROM stress_txn");
    assert_eq!(
        count,
        (THREADS * CYCLES) as i64,
        "expected {} rows from transaction cycles",
        THREADS * CYCLES,
    );
}

// ---------------------------------------------------------------------------
// 5. Rapid session creation and teardown
// ---------------------------------------------------------------------------

#[test]
fn stress_rapid_session_creation() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    const THREADS: usize = 10;
    const SESSIONS_PER_THREAD: usize = 20;

    let had_error = AtomicBool::new(false);

    thread::scope(|s| {
        for t in 0..THREADS {
            let engine = &engine;
            let had_error = &had_error;
            s.spawn(move || {
                for i in 0..SESSIONS_PER_THREAD {
                    let (session, _) = match engine.startup(startup_params()) {
                        Ok(s) => s,
                        Err(e) => {
                            had_error.store(true, Ordering::SeqCst);
                            panic!("thread {t} session {i} startup failed: {e}");
                        }
                    };

                    if let Err(e) = engine.execute_sql(&session, "SELECT 1 AS probe") {
                        had_error.store(true, Ordering::SeqCst);
                        panic!("thread {t} session {i} SELECT 1 failed: {e}");
                    }

                    // Let session drop (or explicitly terminate).
                    if let Err(e) = engine.terminate(session) {
                        had_error.store(true, Ordering::SeqCst);
                        panic!("thread {t} session {i} terminate failed: {e}");
                    }
                }
            });
        }
    });

    assert!(
        !had_error.load(Ordering::SeqCst),
        "session creation/teardown had errors",
    );
}

// ---------------------------------------------------------------------------
// 6. Mixed DDL and DML from concurrent threads
// ---------------------------------------------------------------------------

#[test]
fn stress_mixed_ddl_dml() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (setup, _) = engine.startup(startup_params()).expect("startup");

    // Pre-create the DML target table.
    engine
        .execute_sql(&setup, "CREATE TABLE stress_dml (id INT, val TEXT)")
        .expect("create dml table");

    const DML_THREADS: usize = 2;
    const DML_OPS: usize = 100;
    const DDL_THREADS: usize = 2;
    const DDL_CYCLES: usize = 25;

    let had_error = AtomicBool::new(false);

    thread::scope(|s| {
        // DML threads: insert and select on the pre-created table.
        for d in 0..DML_THREADS {
            let engine = &engine;
            let had_error = &had_error;
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup dml");
                for i in 0..DML_OPS {
                    let id = d * DML_OPS + i;
                    if let Err(e) = engine.execute_sql(
                        &session,
                        &format!("INSERT INTO stress_dml VALUES ({id}, 'dml_{d}')"),
                    ) {
                        had_error.store(true, Ordering::SeqCst);
                        panic!("DML thread {d} insert {i} failed: {e}");
                    }
                }
                // Verify we can read.
                if let Err(e) = engine.execute_sql(&session, "SELECT COUNT(*) FROM stress_dml") {
                    had_error.store(true, Ordering::SeqCst);
                    panic!("DML thread {d} final select failed: {e}");
                }
            });
        }

        // DDL threads: each creates and drops its own uniquely-named tables.
        for d in 0..DDL_THREADS {
            let engine = &engine;
            let had_error = &had_error;
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup ddl");
                for c in 0..DDL_CYCLES {
                    let table = format!("stress_ddl_{d}_{c}");
                    if let Err(e) =
                        engine.execute_sql(&session, &format!("CREATE TABLE {table} (x INT)"))
                    {
                        had_error.store(true, Ordering::SeqCst);
                        panic!("DDL thread {d} create {c} failed: {e}");
                    }
                    if let Err(e) = engine.execute_sql(&session, &format!("DROP TABLE {table}")) {
                        had_error.store(true, Ordering::SeqCst);
                        panic!("DDL thread {d} drop {c} failed: {e}");
                    }
                }
            });
        }
    });

    assert!(
        !had_error.load(Ordering::SeqCst),
        "mixed DDL/DML had errors",
    );

    // Verify DML data is intact.
    let count = query_count(&engine, &setup, "SELECT COUNT(*) FROM stress_dml");
    assert_eq!(
        count,
        (DML_THREADS * DML_OPS) as i64,
        "DML table should have all inserted rows",
    );
}

// ---------------------------------------------------------------------------
// 7. Update contention on the same rows
// ---------------------------------------------------------------------------

#[test]
fn stress_update_contention() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (setup, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&setup, "CREATE TABLE stress_upd (id INT, val TEXT)")
        .expect("create table");

    const INITIAL_ROWS: usize = 10;
    for i in 0..INITIAL_ROWS {
        engine
            .execute_sql(&setup, &format!("INSERT INTO stress_upd VALUES ({i}, '')"))
            .expect("seed insert");
    }

    const THREADS: usize = 4;
    const UPDATES_PER_THREAD: usize = 50;

    let success_counts: Vec<std::sync::atomic::AtomicUsize> = (0..THREADS)
        .map(|_| std::sync::atomic::AtomicUsize::new(0))
        .collect();

    thread::scope(|s| {
        for (t, counter) in success_counts.iter().enumerate().take(THREADS) {
            let engine = &engine;
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup");
                for u in 0..UPDATES_PER_THREAD {
                    let row_id = u % INITIAL_ROWS;
                    // Each thread appends its thread ID to the val column.
                    let sql =
                        format!("UPDATE stress_upd SET val = val || '{t}' WHERE id = {row_id}");
                    match engine.execute_sql(&session, &sql) {
                        Ok(_) => {
                            counter.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            // Serialization failures are expected under contention.
                            let state = e.sqlstate();
                            assert!(
                                state == aiondb_core::SqlState::SerializationFailure
                                    || state == aiondb_core::SqlState::InternalError,
                                "unexpected error in update thread {t}, iteration {u}: {e}",
                            );
                        }
                    }
                }
            });
        }
    });

    // Check that at least some updates succeeded.
    let total_successes: usize = success_counts
        .iter()
        .map(|c| c.load(Ordering::Relaxed))
        .sum();
    assert!(
        total_successes > 0,
        "at least some updates should have succeeded",
    );

    // Verify no data corruption: every row should still exist and be readable.
    let count = query_count(&engine, &setup, "SELECT COUNT(*) FROM stress_upd");
    assert_eq!(
        count, INITIAL_ROWS as i64,
        "row count should remain {INITIAL_ROWS} after updates",
    );

    // Verify all val columns contain only valid characters (digits 0-3).
    let rows = query_rows(
        &engine,
        &setup,
        "SELECT id, val FROM stress_upd ORDER BY id",
    );
    assert_eq!(rows.len(), INITIAL_ROWS, "should have all rows");
    for row in &rows {
        if let Value::Text(val) = &row.values[1] {
            assert!(
                val.chars()
                    .all(|c| c == '0' || c == '1' || c == '2' || c == '3'),
                "val column contains unexpected characters: {val:?}",
            );
        }
    }
}
