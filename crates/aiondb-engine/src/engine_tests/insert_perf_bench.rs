//! Micro-benchmark for the INSERT / batch INSERT / COPY paths.
//!
//! Run with `cargo test -p aiondb-engine --lib insert_perf -- --ignored --nocapture`.
//! Gated as `#[ignore]` like the SELECT bench: it exists to validate
//! ingest-side optimizations rather than gate CI.

use std::time::Instant;

use super::*;

const ROWS_PER_BATCH: usize = 5_000;
const REPEATS: usize = 3;

fn timed<F: FnMut()>(label: &str, mut f: F) -> f64 {
    f();
    let start = Instant::now();
    for _ in 0..REPEATS {
        f();
    }
    let elapsed = start.elapsed();
    let per_iter_us = elapsed.as_micros() as f64 / REPEATS as f64;
    println!("  {label:<48} {per_iter_us:>10.1} us/iter");
    per_iter_us
}

fn build_batch_insert_sql(table: &str, base_id: i64) -> String {
    let mut sql = format!("INSERT INTO {table} (id, val, tag) VALUES ");
    for i in 0..ROWS_PER_BATCH {
        if i > 0 {
            sql.push_str(", ");
        }
        let id = base_id + i as i64;
        let tag = if i % 2 == 0 { "even" } else { "odd" };
        sql.push_str(&format!("({id}, {i}, '{tag}')"));
    }
    sql
}

fn build_copy_data(rows: usize, base_id: i64) -> String {
    let mut data = String::with_capacity(rows * 16);
    for i in 0..rows {
        let id = base_id + i as i64;
        let tag = if i % 2 == 0 { "even" } else { "odd" };
        data.push_str(&format!("{id}\t{i}\t{tag}\n"));
    }
    data
}

#[test]
#[ignore]
fn insert_perf_paths() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");

    println!();
    println!("INSERT path bench  (rows_per_batch={ROWS_PER_BATCH}, repeats={REPEATS})");

    // ---- 1. Single-statement multi-row INSERT VALUES into a fresh table.
    println!();
    println!("  ----- shape: INSERT VALUES (...), (...) ×{ROWS_PER_BATCH}  (no indexes) -----");
    engine
        .execute_sql(&s, "CREATE TABLE bench_ins (id INT, val INT, tag TEXT)")
        .expect("create");
    let mut counter = 0i64;
    timed("batch-insert-no-idx", || {
        let sql = build_batch_insert_sql("bench_ins", counter);
        counter += ROWS_PER_BATCH as i64;
        engine.execute_sql(&s, &sql).expect("batch insert");
    });

    // ---- 2. Same shape but the table has a PK + secondary index.
    println!();
    println!("  ----- shape: INSERT VALUES ×{ROWS_PER_BATCH}  (PK + secondary index) -----");
    engine
        .execute_sql(
            &s,
            "CREATE TABLE bench_ins_idx (id INT PRIMARY KEY, val INT, tag TEXT)",
        )
        .expect("create idx");
    engine
        .execute_sql(&s, "CREATE INDEX bench_ins_idx_tag ON bench_ins_idx (tag)")
        .expect("secondary index");
    let mut counter = 0i64;
    timed("batch-insert-with-idx", || {
        let sql = build_batch_insert_sql("bench_ins_idx", counter);
        counter += ROWS_PER_BATCH as i64;
        engine.execute_sql(&s, &sql).expect("batch insert idx");
    });

    // ---- 3. One INSERT per row (worst case — separate statement each).
    println!();
    println!("  ----- shape: ×{ROWS_PER_BATCH} single-row INSERTs (no indexes) -----");
    engine
        .execute_sql(&s, "CREATE TABLE bench_ins_one (id INT, val INT, tag TEXT)")
        .expect("create one");
    let mut counter = 0i64;
    timed("single-insert-no-idx", || {
        for i in 0..ROWS_PER_BATCH {
            let id = counter + i as i64;
            let tag = if i % 2 == 0 { "even" } else { "odd" };
            engine
                .execute_sql(
                    &s,
                    &format!(
                        "INSERT INTO bench_ins_one (id, val, tag) VALUES ({id}, {i}, '{tag}')"
                    ),
                )
                .expect("single insert");
        }
        counter += ROWS_PER_BATCH as i64;
    });

    // COPY FROM is exercised separately; getting the live `RelationId`
    // out of the in-process engine requires plumbing the wire layer
    // doesn't expose here. The batched INSERT VALUES shape exercises
    // the same `insert_batch` code path COPY ultimately funnels into.
    let _ = build_copy_data; // silence unused warning when COPY shape is gated off.
}

/// Regression: multi-row autocommit `INSERT VALUES` must commit every
/// row through the batched fast path AND maintain unique/PK
/// constraints (since `insert_batch_autocommit` runs the unique
/// preflight against in-batch entries too).
#[test]
fn batch_insert_autocommit_commits_all_and_enforces_unique() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE t (id INT PRIMARY KEY, name TEXT)")
        .expect("create");

    // Happy path: every distinct id commits.
    engine
        .execute_sql(
            &s,
            "INSERT INTO t (id, name) VALUES (1, 'a'), (2, 'b'), (3, 'c')",
        )
        .expect("insert distinct");
    let count = query_count(&engine, &s, "SELECT COUNT(*) FROM t");
    assert_eq!(count, 3);

    // Within-batch duplicate must error and leave the table at 3 rows
    // (no partial apply).
    let result = engine.execute_sql(&s, "INSERT INTO t (id, name) VALUES (10, 'x'), (10, 'y')");
    assert!(
        result.is_err(),
        "duplicate PK within batch should be rejected"
    );
    let count_after_err = query_count(&engine, &s, "SELECT COUNT(*) FROM t");
    assert_eq!(count_after_err, 3, "failed batch must not partially apply");

    // Cross-batch duplicate: a previously-committed key is still
    // detected by the preflight when a later batch tries to reuse it.
    let result = engine.execute_sql(&s, "INSERT INTO t (id, name) VALUES (1, 'dup')");
    assert!(
        result.is_err(),
        "duplicate PK across batches should be rejected"
    );
}
