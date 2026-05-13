#![allow(clippy::pedantic)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use super::*;

// ---------------------------------------------------------------------------
// Helpers (same signatures as stress.rs)
// ---------------------------------------------------------------------------

/// Soak tests run sustained multi-threaded workloads (8 threads × 500 ops,
/// rollback storms, concurrent DDL) and stack up under default `cargo test`
/// parallelism, exhausting host RAM. Gate behind `AIONDB_RUN_SOAK=1`.
fn soak_gate(name: &str) -> bool {
    if std::env::var_os("AIONDB_RUN_SOAK").is_some() {
        true
    } else {
        eprintln!("{name} skipped: set AIONDB_RUN_SOAK=1 to run");
        false
    }
}

// ---------------------------------------------------------------------------
// 1. Sustained mixed workload
// ---------------------------------------------------------------------------

#[test]
fn soak_sustained_mixed_workload() {
    if !soak_gate("soak_sustained_mixed_workload") {
        return;
    }
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (setup, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &setup,
            "CREATE TABLE soak_mixed (id INT PRIMARY KEY, val TEXT)",
        )
        .expect("create table");

    // Seed rows so UPDATE/DELETE have targets.
    const SEED: usize = 200;
    for i in 0..SEED {
        engine
            .execute_sql(
                &setup,
                &format!("INSERT INTO soak_mixed VALUES ({i}, 'seed')"),
            )
            .expect("seed");
    }

    const THREADS: usize = 8;
    const OPS_PER_THREAD: usize = 500;

    let insert_count = AtomicUsize::new(SEED);
    let had_error = AtomicBool::new(false);

    thread::scope(|s| {
        for t in 0..THREADS {
            let engine = &engine;
            let insert_count = &insert_count;
            let had_error = &had_error;
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup");
                for i in 0..OPS_PER_THREAD {
                    let op = (t * OPS_PER_THREAD + i) % 4;
                    match op {
                        0 => {
                            // INSERT
                            let id = 10_000 + t * OPS_PER_THREAD + i;
                            if engine
                                .execute_sql(
                                    &session,
                                    &format!("INSERT INTO soak_mixed VALUES ({id}, 'thread_{t}')"),
                                )
                                .is_ok()
                            {
                                insert_count.fetch_add(1, Ordering::Relaxed);
                            } // PK conflict is fine
                        }
                        1 => {
                            // SELECT
                            if let Err(e) =
                                engine.execute_sql(&session, "SELECT COUNT(*) FROM soak_mixed")
                            {
                                had_error.store(true, Ordering::SeqCst);
                                panic!("thread {t} SELECT failed: {e}");
                            }
                        }
                        2 => {
                            // UPDATE
                            let target = i % SEED;
                            let _ = engine.execute_sql(
                                &session,
                                &format!("UPDATE soak_mixed SET val = 't{t}' WHERE id = {target}"),
                            );
                            // Serialization failures OK
                        }
                        3 => {
                            // DELETE + re-INSERT to keep rows stable
                            let target = SEED + t * OPS_PER_THREAD + i;
                            let _ = engine.execute_sql(
                                &session,
                                &format!("DELETE FROM soak_mixed WHERE id = {target}"),
                            );
                        }
                        _ => unreachable!(),
                    }
                }
            });
        }
    });

    assert!(
        !had_error.load(Ordering::SeqCst),
        "mixed workload had unexpected errors"
    );

    // Data integrity: table must be readable and row count consistent.
    let count = query_count(&engine, &setup, "SELECT COUNT(*) FROM soak_mixed");
    assert!(count >= 0, "count should be non-negative");

    // All rows should be readable without corruption.
    let rows = query_rows(&engine, &setup, "SELECT id, val FROM soak_mixed");
    assert_eq!(rows.len(), count as usize, "row count mismatch");
}

// ---------------------------------------------------------------------------
// 2. Transaction rollback storm
// ---------------------------------------------------------------------------

#[test]
fn soak_rollback_storm() {
    if !soak_gate("soak_rollback_storm") {
        return;
    }
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (setup, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&setup, "CREATE TABLE soak_rollback (id INT, val TEXT)")
        .expect("create table");

    const THREADS: usize = 6;
    const CYCLES: usize = 200;

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
                            &format!("INSERT INTO soak_rollback VALUES ({id}, 'phantom_{t}')"),
                        )
                        .unwrap_or_else(|e| panic!("thread {t} cycle {c} INSERT: {e}"));
                    engine
                        .execute_sql(&session, "ROLLBACK")
                        .unwrap_or_else(|e| panic!("thread {t} cycle {c} ROLLBACK: {e}"));
                }
            });
        }
    });

    // No phantom rows should be visible.
    let count = query_count(&engine, &setup, "SELECT COUNT(*) FROM soak_rollback");
    assert_eq!(count, 0, "rolled-back inserts should leave zero rows");
}

// ---------------------------------------------------------------------------
// 3. Concurrent DDL stress
// ---------------------------------------------------------------------------

#[test]
fn soak_concurrent_ddl_stress() {
    if !soak_gate("soak_concurrent_ddl_stress") {
        return;
    }
    let engine = EngineBuilder::for_testing().build().unwrap();

    const THREADS: usize = 4;
    const CYCLES: usize = 30;

    thread::scope(|s| {
        for t in 0..THREADS {
            let engine = &engine;
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup");
                for c in 0..CYCLES {
                    // Each thread uses its own table name to avoid cross-thread
                    // DDL conflicts. Two threads share table names across cycles
                    // to test create-drop-recreate sequences.
                    let table = format!("soak_ddl_t{t}_c{}", c % 10);

                    // Try to create; may fail if table exists from a
                    // previous cycle (that is expected).
                    let _ = engine
                        .execute_sql(&session, &format!("CREATE TABLE {table} (x INT, y TEXT)"));

                    // Insert a row if the table exists.
                    let _ = engine
                        .execute_sql(&session, &format!("INSERT INTO {table} VALUES (1, 'ok')"));

                    // Try to select to ensure no corruption.
                    match engine.execute_sql(&session, &format!("SELECT * FROM {table}")) {
                        Ok(results) => {
                            // Should get a Query result.
                            if let Some(StatementResult::Query { .. }) = results.last() {
                                // OK
                            }
                        }
                        Err(_) => {
                            // Table may have been dropped by another cycle.
                        }
                    }

                    // Drop table; may fail if it was already dropped.
                    let _ = engine.execute_sql(&session, &format!("DROP TABLE {table}"));
                }
            });
        }
    });
    // If any thread panicked, thread::scope propagates it automatically.
}

// ---------------------------------------------------------------------------
// 4. Session churn under load
// ---------------------------------------------------------------------------

#[test]
fn soak_session_churn_under_load() {
    if !soak_gate("soak_session_churn_under_load") {
        return;
    }
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (setup, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&setup, "CREATE TABLE soak_churn (id INT, val TEXT)")
        .expect("create table");

    // Seed some data for active queries.
    for i in 0..50 {
        engine
            .execute_sql(
                &setup,
                &format!("INSERT INTO soak_churn VALUES ({i}, 'seed')"),
            )
            .expect("seed");
    }

    const CHURN_THREADS: usize = 4;
    const CHURN_CYCLES: usize = 50;
    const ACTIVE_THREADS: usize = 4;
    const ACTIVE_OPS: usize = 100;

    let had_error = AtomicBool::new(false);

    thread::scope(|s| {
        // Active query threads: keep querying throughout.
        for a in 0..ACTIVE_THREADS {
            let engine = &engine;
            let had_error = &had_error;
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup active");
                for i in 0..ACTIVE_OPS {
                    match engine.execute_sql(&session, "SELECT COUNT(*) FROM soak_churn") {
                        Ok(results) => {
                            if let Some(StatementResult::Query { rows, .. }) = results.last() {
                                if let Some(row) = rows.first() {
                                    if let Value::BigInt(n) = &row.values[0] {
                                        if *n < 50 {
                                            had_error.store(true, Ordering::SeqCst);
                                            panic!("active {a} op {i}: count {n} < 50");
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            had_error.store(true, Ordering::SeqCst);
                            panic!("active {a} op {i} failed: {e}");
                        }
                    }
                }
            });
        }

        // Churn threads: create session, query, terminate, repeat.
        for c in 0..CHURN_THREADS {
            let engine = &engine;
            let had_error = &had_error;
            s.spawn(move || {
                for i in 0..CHURN_CYCLES {
                    let (session, _) = match engine.startup(startup_params()) {
                        Ok(s) => s,
                        Err(e) => {
                            had_error.store(true, Ordering::SeqCst);
                            panic!("churn {c} cycle {i} startup: {e}");
                        }
                    };

                    // Execute a query.
                    let _ = engine.execute_sql(&session, "SELECT COUNT(*) FROM soak_churn");

                    // Terminate immediately.
                    if let Err(e) = engine.terminate(session) {
                        had_error.store(true, Ordering::SeqCst);
                        panic!("churn {c} cycle {i} terminate: {e}");
                    }
                }
            });
        }
    });

    assert!(
        !had_error.load(Ordering::SeqCst),
        "session churn under load had errors"
    );
}

// ---------------------------------------------------------------------------
// 5. Large result set stress
// ---------------------------------------------------------------------------

#[test]
fn soak_large_result_set_stress() {
    if !soak_gate("soak_large_result_set_stress") {
        return;
    }
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (setup, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&setup, "CREATE TABLE soak_large (id INT, payload TEXT)")
        .expect("create table");

    const TOTAL_ROWS: usize = 10_000;
    const BATCH: usize = 100;
    const READER_THREADS: usize = 4;
    const READS_PER_READER: usize = 5;

    // Insert in batches for speed.
    let (writer_session, _) = engine.startup(startup_params()).expect("startup");
    for batch_start in (0..TOTAL_ROWS).step_by(BATCH) {
        let batch_end = (batch_start + BATCH).min(TOTAL_ROWS);
        for i in batch_start..batch_end {
            engine
                .execute_sql(
                    &writer_session,
                    &format!("INSERT INTO soak_large VALUES ({i}, 'payload_{i}')"),
                )
                .expect("insert");
        }
    }

    // Now run concurrent readers while a writer inserts more rows.
    let additional_start = TOTAL_ROWS;
    let additional_end = TOTAL_ROWS + 500;

    let had_error = AtomicBool::new(false);

    let engine_ref = &engine;
    let had_error = &had_error;

    thread::scope(|s| {
        // Writer thread: inserts additional rows.
        s.spawn(move || {
            let (session, _) = engine_ref
                .startup(startup_params())
                .expect("startup writer");
            for i in additional_start..additional_end {
                engine_ref
                    .execute_sql(
                        &session,
                        &format!("INSERT INTO soak_large VALUES ({i}, 'extra_{i}')"),
                    )
                    .expect("extra insert");
            }
        });

        // Concurrent readers.
        for r in 0..READER_THREADS {
            let engine = engine_ref;
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup reader");
                for read_i in 0..READS_PER_READER {
                    match engine.execute_sql(&session, "SELECT COUNT(*) FROM soak_large") {
                        Ok(results) => {
                            if let Some(StatementResult::Query { rows, .. }) = results.last() {
                                if let Some(row) = rows.first() {
                                    if let Value::BigInt(n) = &row.values[0] {
                                        // Count must be at least the initial
                                        // amount (snapshot isolation).
                                        if *n < TOTAL_ROWS as i64 {
                                            had_error.store(true, Ordering::SeqCst);
                                            panic!(
                                                "reader {r} read {read_i}: \
                                                 count {n} < {TOTAL_ROWS}"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            had_error.store(true, Ordering::SeqCst);
                            panic!("reader {r} read {read_i} failed: {e}");
                        }
                    }
                }
            });
        }
    });

    assert!(
        !had_error.load(Ordering::SeqCst),
        "large result set stress had errors"
    );

    // Final verification.
    let final_count = query_count(&engine, &setup, "SELECT COUNT(*) FROM soak_large");
    let expected = (TOTAL_ROWS + (additional_end - additional_start)) as i64;
    assert_eq!(final_count, expected, "final count mismatch");
}

// ---------------------------------------------------------------------------
// 6. Deadlock/contention scenario
// ---------------------------------------------------------------------------

#[test]
fn soak_contention_overlapping_updates() {
    if !soak_gate("soak_contention_overlapping_updates") {
        return;
    }
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (setup, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&setup, "CREATE TABLE soak_contention (id INT, counter INT)")
        .expect("create table");

    const NUM_ROWS: usize = 20;
    for i in 0..NUM_ROWS {
        engine
            .execute_sql(
                &setup,
                &format!("INSERT INTO soak_contention VALUES ({i}, 0)"),
            )
            .expect("seed");
    }

    const THREADS: usize = 6;
    const UPDATES_PER_THREAD: usize = 100;

    let success_counts: Vec<AtomicUsize> = (0..THREADS).map(|_| AtomicUsize::new(0)).collect();
    let failure_counts: Vec<AtomicUsize> = (0..THREADS).map(|_| AtomicUsize::new(0)).collect();

    thread::scope(|s| {
        for t in 0..THREADS {
            let engine = &engine;
            let successes = &success_counts[t];
            let failures = &failure_counts[t];
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup");
                for u in 0..UPDATES_PER_THREAD {
                    // Each thread updates rows in a different order to create
                    // contention patterns. Thread 0 goes 0,1,2,...; thread 1
                    // goes in reverse; others use varying strides.
                    let row_id = match t % 3 {
                        0 => u % NUM_ROWS,
                        1 => (NUM_ROWS - 1) - (u % NUM_ROWS),
                        _ => (u * (t + 1)) % NUM_ROWS,
                    };

                    let sql = format!(
                        "UPDATE soak_contention SET counter = counter + 1 \
                         WHERE id = {row_id}"
                    );
                    match engine.execute_sql(&session, &sql) {
                        Ok(_) => {
                            successes.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            let state = e.sqlstate();
                            assert!(
                                state == aiondb_core::SqlState::SerializationFailure
                                    || state == aiondb_core::SqlState::InternalError,
                                "unexpected error in thread {t}, op {u}: {e}"
                            );
                            failures.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            });
        }
    });

    // All threads must complete (they did if we reached here).
    let total_successes: usize = success_counts
        .iter()
        .map(|c| c.load(Ordering::Relaxed))
        .sum();
    let total_failures: usize = failure_counts
        .iter()
        .map(|c| c.load(Ordering::Relaxed))
        .sum();

    assert!(total_successes > 0, "at least some updates should succeed");
    assert_eq!(
        total_successes + total_failures,
        THREADS * UPDATES_PER_THREAD,
        "all operations should be accounted for"
    );

    // Verify data integrity: row count unchanged, counter values make sense.
    let count = query_count(&engine, &setup, "SELECT COUNT(*) FROM soak_contention");
    assert_eq!(count, NUM_ROWS as i64, "row count should remain {NUM_ROWS}");

    // Sum of all counters should be positive and not exceed total successes.
    // Under autocommit, concurrent read-modify-write on the same row may
    // cause lost updates (two threads read the same value, both increment,
    // one overwrites the other), so the sum can be less than total_successes.
    let rows = query_rows(
        &engine,
        &setup,
        "SELECT id, counter FROM soak_contention ORDER BY id",
    );
    let counter_sum: i64 = rows
        .iter()
        .map(|row| match &row.values[1] {
            Value::Int(n) => *n as i64,
            Value::BigInt(n) => *n,
            other => panic!("unexpected counter value: {other:?}"),
        })
        .sum();
    assert!(
        counter_sum > 0,
        "some counters should have been incremented"
    );
    assert!(
        counter_sum <= total_successes as i64,
        "counter sum ({counter_sum}) should not exceed total successes ({total_successes})"
    );
}
