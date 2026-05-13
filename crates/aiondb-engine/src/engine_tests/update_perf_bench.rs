//! Micro-benchmark for the UPDATE access paths.
//!
//! Run with `cargo test -p aiondb-engine --lib update_perf -- --ignored --nocapture`.
//! Builds two identical tables (`bench_idx` with indexes, `bench_seq`
//! without) and runs the same predicate against each so the only
//! variable is the access path. The bench is `#[ignore]` because it
//! exists to validate optimizations rather than gate CI.

use std::time::Instant;

use super::*;

const ROWS: usize = 5_000;
const REPEATS: usize = 5;

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

fn build_seed_sql(table: &str) -> String {
    let mut sql = format!("INSERT INTO {table} (id, val, tag) VALUES ");
    for i in 0..ROWS {
        if i > 0 {
            sql.push_str(", ");
        }
        let tag = if i % 2 == 0 { "even" } else { "odd" };
        sql.push_str(&format!("({i}, 0, '{tag}')"));
    }
    sql
}

#[test]
#[ignore]
fn update_perf_access_paths() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");

    // Indexed table.
    engine
        .execute_sql(
            &s,
            "CREATE TABLE bench_idx (id INT PRIMARY KEY, val INT, tag TEXT)",
        )
        .expect("create idx");
    engine
        .execute_sql(&s, "CREATE INDEX bench_idx_id_tag ON bench_idx (id, tag)")
        .expect("composite index");
    engine
        .execute_sql(&s, &build_seed_sql("bench_idx"))
        .expect("seed idx");

    // Same data, no indexes (PK is replaced by a non-unique column to
    // ensure no btree exists at all).
    engine
        .execute_sql(&s, "CREATE TABLE bench_seq (id INT, val INT, tag TEXT)")
        .expect("create seq");
    engine
        .execute_sql(&s, &build_seed_sql("bench_seq"))
        .expect("seed seq");

    println!();
    println!("UPDATE access path bench  (rows={ROWS}, repeats={REPEATS})");
    println!();
    println!("  ----- shape: WHERE id = 2500  (eq, 1 row matches) -----");
    let eq_idx = timed("idx", || {
        engine
            .execute_sql(&s, "UPDATE bench_idx SET val = val + 1 WHERE id = 2500")
            .expect("eq idx");
    });
    let eq_seq = timed("seq", || {
        engine
            .execute_sql(&s, "UPDATE bench_seq SET val = val + 1 WHERE id = 2500")
            .expect("eq seq");
    });
    println!("    speedup (seq/idx) = {:>5.2}x", eq_seq / eq_idx.max(1.0));

    println!();
    println!("  ----- shape: WHERE id IN (100, 200, .., 1000)  (10 rows match) -----");
    let in_query = "UPDATE %t SET val = val + 1 WHERE id IN (100, 200, 300, 400, 500, 600, 700, 800, 900, 1000)";
    let in_idx = timed("idx", || {
        engine
            .execute_sql(&s, &in_query.replace("%t", "bench_idx"))
            .expect("in idx");
    });
    let in_seq = timed("seq", || {
        engine
            .execute_sql(&s, &in_query.replace("%t", "bench_seq"))
            .expect("in seq");
    });
    println!("    speedup (seq/idx) = {:>5.2}x", in_seq / in_idx.max(1.0));

    println!();
    println!("  ----- shape: WHERE id BETWEEN 1000 AND 1100  (101 rows match) -----");
    let range_idx = timed("idx", || {
        engine
            .execute_sql(
                &s,
                "UPDATE bench_idx SET val = val + 1 WHERE id BETWEEN 1000 AND 1100",
            )
            .expect("range idx");
    });
    let range_seq = timed("seq", || {
        engine
            .execute_sql(
                &s,
                "UPDATE bench_seq SET val = val + 1 WHERE id BETWEEN 1000 AND 1100",
            )
            .expect("range seq");
    });
    println!(
        "    speedup (seq/idx) = {:>5.2}x",
        range_seq / range_idx.max(1.0)
    );

    println!();
    println!("  ----- shape: WHERE id >= 4900  (100 rows match) -----");
    let single_idx = timed("idx", || {
        engine
            .execute_sql(&s, "UPDATE bench_idx SET val = val + 1 WHERE id >= 4900")
            .expect("single idx");
    });
    let single_seq = timed("seq", || {
        engine
            .execute_sql(&s, "UPDATE bench_seq SET val = val + 1 WHERE id >= 4900")
            .expect("single seq");
    });
    println!(
        "    speedup (seq/idx) = {:>5.2}x",
        single_seq / single_idx.max(1.0)
    );

    println!();
    println!("  ----- shape: WHERE id = 2500 AND tag = 'even'  (1 row matches) -----");
    let composite_idx = timed("idx", || {
        engine
            .execute_sql(
                &s,
                "UPDATE bench_idx SET val = val + 1 WHERE id = 2500 AND tag = 'even'",
            )
            .expect("composite idx");
    });
    let composite_seq = timed("seq", || {
        engine
            .execute_sql(
                &s,
                "UPDATE bench_seq SET val = val + 1 WHERE id = 2500 AND tag = 'even'",
            )
            .expect("composite seq");
    });
    println!(
        "    speedup (seq/idx) = {:>5.2}x",
        composite_seq / composite_idx.max(1.0)
    );
}
