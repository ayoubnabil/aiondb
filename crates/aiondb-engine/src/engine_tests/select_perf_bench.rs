//! Micro-benchmark + targeted regression tests for the SELECT access paths.
//!
//! Run with `cargo test -p aiondb-engine --release --lib select_perf -- --ignored --nocapture`.
//! Builds two identical tables (`bench_idx` with indexes, `bench_seq`
//! without) and runs the same predicate against each so the only
//! variable is the access path.

use std::time::Instant;

use super::*;

const ROWS: usize = 500;
const REPEATS: usize = 2;

fn timed<F: FnMut()>(label: &str, mut f: F) -> f64 {
    // Warm cache + plan.
    f();
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
        sql.push_str(&format!("({i}, {i}, '{tag}')"));
    }
    sql
}

#[test]
fn select_perf_access_paths() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");

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

    engine
        .execute_sql(&s, "CREATE TABLE bench_seq (id INT, val INT, tag TEXT)")
        .expect("create seq");
    engine
        .execute_sql(&s, &build_seed_sql("bench_seq"))
        .expect("seed seq");

    // Match PG conventions: collect statistics before measurement so the
    // optimizer has histograms + per-column correlation available, just
    // like a freshly-loaded production workload that's been auto-analyzed.
    engine
        .execute_sql(&s, "ANALYZE bench_idx")
        .expect("analyze idx");
    engine
        .execute_sql(&s, "ANALYZE bench_seq")
        .expect("analyze seq");

    println!();
    println!("SELECT access path bench  (rows={ROWS}, repeats={REPEATS})");

    println!();
    println!("  ----- shape: WHERE id = 2500  (eq, 1 row) -----");
    let eq_idx = timed("idx", || {
        let _ = engine
            .execute_sql(&s, "SELECT id, val FROM bench_idx WHERE id = 2500")
            .expect("eq idx");
    });
    let eq_seq = timed("seq", || {
        let _ = engine
            .execute_sql(&s, "SELECT id, val FROM bench_seq WHERE id = 2500")
            .expect("eq seq");
    });
    println!("    speedup (seq/idx) = {:>5.2}x", eq_seq / eq_idx.max(1.0));

    println!();
    println!("  ----- shape: WHERE id IN (100..1000 step 100)  (10 rows) -----");
    let in_q =
        "SELECT id, val FROM %t WHERE id IN (100, 200, 300, 400, 500, 600, 700, 800, 900, 1000)";
    let in_idx = timed("idx", || {
        let _ = engine
            .execute_sql(&s, &in_q.replace("%t", "bench_idx"))
            .expect("in idx");
    });
    let in_seq = timed("seq", || {
        let _ = engine
            .execute_sql(&s, &in_q.replace("%t", "bench_seq"))
            .expect("in seq");
    });
    println!("    speedup (seq/idx) = {:>5.2}x", in_seq / in_idx.max(1.0));

    println!();
    println!("  ----- shape: WHERE id BETWEEN 1000 AND 1100  (101 rows) -----");
    let range_idx = timed("idx", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT id, val FROM bench_idx WHERE id BETWEEN 1000 AND 1100",
            )
            .expect("range idx");
    });
    let range_seq = timed("seq", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT id, val FROM bench_seq WHERE id BETWEEN 1000 AND 1100",
            )
            .expect("range seq");
    });
    println!(
        "    speedup (seq/idx) = {:>5.2}x",
        range_seq / range_idx.max(1.0)
    );

    println!();
    println!("  ----- shape: WHERE id >= 4900  (100 rows tail) -----");
    let tail_idx = timed("idx", || {
        let _ = engine
            .execute_sql(&s, "SELECT id, val FROM bench_idx WHERE id >= 4900")
            .expect("tail idx");
    });
    let tail_seq = timed("seq", || {
        let _ = engine
            .execute_sql(&s, "SELECT id, val FROM bench_seq WHERE id >= 4900")
            .expect("tail seq");
    });
    println!(
        "    speedup (seq/idx) = {:>5.2}x",
        tail_seq / tail_idx.max(1.0)
    );

    println!();
    println!("  ----- shape: WHERE id = 2500 AND tag = 'even'  (composite, 1 row) -----");
    let cmp_idx = timed("idx", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT id, val FROM bench_idx WHERE id = 2500 AND tag = 'even'",
            )
            .expect("composite idx");
    });
    let cmp_seq = timed("seq", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT id, val FROM bench_seq WHERE id = 2500 AND tag = 'even'",
            )
            .expect("composite seq");
    });
    println!(
        "    speedup (seq/idx) = {:>5.2}x",
        cmp_seq / cmp_idx.max(1.0)
    );

    println!();
    println!("  ----- shape: SELECT * full scan  ({ROWS} rows) -----");
    timed("full-seq", || {
        let _ = engine
            .execute_sql(&s, "SELECT id, val, tag FROM bench_seq")
            .expect("full seq");
    });
    timed("full-idx", || {
        let _ = engine
            .execute_sql(&s, "SELECT id, val, tag FROM bench_idx")
            .expect("full idx");
    });

    println!();
    println!("  ----- shape: SELECT COUNT(*)  (full scan agg) -----");
    timed("count-seq", || {
        let _ = engine
            .execute_sql(&s, "SELECT COUNT(*) FROM bench_seq")
            .expect("count seq");
    });
    timed("count-idx", || {
        let _ = engine
            .execute_sql(&s, "SELECT COUNT(*) FROM bench_idx")
            .expect("count idx");
    });

    println!();
    println!("  ----- shape: SELECT id WHERE val > 4000  (~999 rows non-indexed col) -----");
    timed("filter-seq", || {
        let _ = engine
            .execute_sql(&s, "SELECT id FROM bench_seq WHERE val > 4000")
            .expect("filter seq");
    });
    timed("filter-idx", || {
        let _ = engine
            .execute_sql(&s, "SELECT id FROM bench_idx WHERE val > 4000")
            .expect("filter idx");
    });

    println!();
    println!("  ----- shape: SELECT * ORDER BY id LIMIT 10 -----");
    timed("order-limit-seq", || {
        let _ = engine
            .execute_sql(&s, "SELECT id, val FROM bench_seq ORDER BY id LIMIT 10")
            .expect("order seq");
    });
    timed("order-limit-idx", || {
        let _ = engine
            .execute_sql(&s, "SELECT id, val FROM bench_idx ORDER BY id LIMIT 10")
            .expect("order idx");
    });

    println!();
    println!(
        "  ----- shape: SELECT COUNT(*) WHERE id BETWEEN 1000 AND 1100 (indexed-range count) -----"
    );
    timed("count-range-seq", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT COUNT(*) FROM bench_seq WHERE id BETWEEN 1000 AND 1100",
            )
            .expect("count range seq");
    });
    timed("count-range-idx", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT COUNT(*) FROM bench_idx WHERE id BETWEEN 1000 AND 1100",
            )
            .expect("count range idx");
    });

    println!();
    println!("  ----- shape: SELECT id WHERE id = 2500  (PK eq, single col projection — index-only) -----");
    timed("eq-id-only-seq", || {
        let _ = engine
            .execute_sql(&s, "SELECT id FROM bench_seq WHERE id = 2500")
            .expect("eq-id-only seq");
    });
    timed("eq-id-only-idx", || {
        let _ = engine
            .execute_sql(&s, "SELECT id FROM bench_idx WHERE id = 2500")
            .expect("eq-id-only idx");
    });

    println!();
    println!("  ----- shape: SELECT id WHERE id BETWEEN 1000 AND 1100  (index-only scan candidate) -----");
    timed("io-range-seq", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT id FROM bench_seq WHERE id BETWEEN 1000 AND 1100",
            )
            .expect("io range seq");
    });
    timed("io-range-idx", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT id FROM bench_idx WHERE id BETWEEN 1000 AND 1100",
            )
            .expect("io range idx");
    });

    println!();
    println!(
        "  ----- shape: WHERE id > 1000 AND val < 3000  (multi-col AND range, non-indexed) -----"
    );
    timed("multi-range-seq", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT id FROM bench_seq WHERE id > 1000 AND val < 3000",
            )
            .expect("multi-range seq");
    });
    timed("multi-range-idx", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT id FROM bench_idx WHERE id > 1000 AND val < 3000",
            )
            .expect("multi-range idx");
    });

    println!();
    println!("  ----- shape: SELECT MIN(id), MAX(id) (PG planagg.c MIN/MAX) -----");
    timed("min-max-seq", || {
        let _ = engine
            .execute_sql(&s, "SELECT MIN(id), MAX(id) FROM bench_seq")
            .expect("minmax seq");
    });
    timed("min-max-idx", || {
        let _ = engine
            .execute_sql(&s, "SELECT MIN(id), MAX(id) FROM bench_idx")
            .expect("minmax idx");
    });
}

/// Regression: multi-aggregate `MIN/MAX` on indexed columns must rewrite
/// to per-aggregate scalar index probes (PG `planagg.c::optimize_minmax_aggregates`).
/// Without the rewrite the query plans into a SeqScan aggregate, which is
/// O(N) instead of O(log N).
#[test]
fn multi_minmax_uses_index_probes() {
    use aiondb_parser::parse_prepared_statement;

    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE t (id INT PRIMARY KEY, val INT)")
        .expect("create");
    engine
        .execute_sql(&s, "INSERT INTO t VALUES (1, 10), (2, 20), (3, 30)")
        .expect("insert");

    let stmt = parse_prepared_statement("SELECT MIN(id), MAX(id) FROM t").expect("parse");
    let plan = engine
        .build_physical_plan(&s, &stmt)
        .expect("physical plan");
    // After the planagg.c-style rewrite the plan is a `ProjectOnce` whose
    // outputs are `ScalarSubquery` expressions — one per aggregate. The
    // pre-rewrite shape was `Aggregate { access_path: SeqScan, ... }`.
    let aiondb_plan::PhysicalPlan::ProjectOnce { outputs, .. } = plan else {
        panic!("expected ProjectOnce after MIN/MAX rewrite, got a different plan");
    };
    assert_eq!(outputs.len(), 2, "should keep one output per aggregate");
    for output in &outputs {
        assert!(
            matches!(
                output.expr.kind,
                aiondb_plan::TypedExprKind::ScalarSubquery { .. }
            ),
            "each aggregate must lower to a ScalarSubquery (LIMIT 1 index probe)"
        );
    }

    // Sanity: result is correct.
    let rows = query_rows(&engine, &s, "SELECT MIN(id), MAX(id) FROM t");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values, vec![Value::Int(1), Value::Int(3)]);
}

/// Regression: when one of several aggregates is *not* MIN/MAX (e.g.
/// `SUM`), the planagg.c-style rewrite must back off and let the normal
/// aggregate plan handle every aggregate. Without this fallback we'd
/// either drop the SUM or produce wrong results.
#[test]
fn mixed_aggregate_falls_back_to_seq_aggregate() {
    use aiondb_parser::parse_prepared_statement;

    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE t (id INT PRIMARY KEY, val INT)")
        .expect("create");
    engine
        .execute_sql(&s, "INSERT INTO t VALUES (1, 10), (2, 20), (3, 30)")
        .expect("insert");

    let stmt = parse_prepared_statement("SELECT MIN(id), SUM(val) FROM t").expect("parse");
    let plan = engine
        .build_physical_plan(&s, &stmt)
        .expect("physical plan");
    // Mixed aggregates must keep the regular Aggregate plan shape.
    assert!(
        matches!(plan, aiondb_plan::PhysicalPlan::Aggregate { .. }),
        "expected Aggregate fallback for mixed MIN/SUM, got a different plan"
    );

    // Result correctness.
    let rows = query_rows(&engine, &s, "SELECT MIN(id), SUM(val) FROM t");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(1));
}

/// Regression: SeqScan + simple range literal filter must agree with
/// the executor-evaluated path. Exercises `scan_table_range_filter`
/// pushdown (the storage-side qualEval) for one-sided ranges,
/// closed ranges, and `BETWEEN`.
#[test]
fn seq_scan_range_filter_pushdown_agrees_with_full_eval() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE t (id INT, val INT, name TEXT)")
        .expect("create");
    let mut sql = String::from("INSERT INTO t (id, val, name) VALUES ");
    for i in 0..50 {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("({i}, {}, 'row_{i}')", i * 2));
    }
    engine.execute_sql(&s, &sql).expect("seed");

    // Vals are 0, 2, 4, ..., 98. Counts derived from that arithmetic.
    let cases = [
        // (sql, expected row count)
        ("SELECT id FROM t WHERE val > 50", 24),
        ("SELECT id FROM t WHERE val >= 50", 25),
        ("SELECT id FROM t WHERE val < 20", 10),
        ("SELECT id FROM t WHERE val <= 20", 11),
        ("SELECT id FROM t WHERE val BETWEEN 30 AND 70", 21),
        ("SELECT id FROM t WHERE val > 10 AND val < 90", 39),
    ];
    for (q, expected) in cases {
        let rows = query_rows(&engine, &s, q);
        assert_eq!(
            rows.len(),
            expected,
            "row count mismatch for `{q}`: got {} expected {expected}",
            rows.len()
        );
    }
}

/// Regression: COUNT(*) WHERE col CMP literal on a non-indexed column
/// must use the storage range pushdown and produce the same result as
/// a fully-evaluated count.
#[test]
fn count_range_pushdown_agrees_with_full_eval() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE t (id INT, val INT)")
        .expect("create");
    let mut sql = String::from("INSERT INTO t (id, val) VALUES ");
    for i in 0..100 {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("({i}, {})", i * 3));
    }
    engine.execute_sql(&s, &sql).expect("seed");

    let cases = [
        ("SELECT COUNT(*) FROM t WHERE val > 100", 66),
        ("SELECT COUNT(*) FROM t WHERE val >= 150", 50),
        ("SELECT COUNT(*) FROM t WHERE val < 30", 10),
        ("SELECT COUNT(*) FROM t WHERE val BETWEEN 60 AND 120", 21),
    ];
    for (q, expected) in cases {
        let n = query_count(&engine, &s, q);
        assert_eq!(
            n, expected,
            "count mismatch for `{q}`: got {n} expected {expected}"
        );
    }
}

/// Regression: SeqScan + AND-of-ranges over distinct columns
/// (`scan_table_multi_range_filter`) must agree with the executor-eval
/// path for both bounded and half-open shapes.
#[test]
fn seq_scan_multi_range_pushdown_agrees_with_full_eval() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE t (id INT, x INT, y INT)")
        .expect("create");
    let mut sql = String::from("INSERT INTO t (id, x, y) VALUES ");
    for i in 0..100 {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("({i}, {}, {})", i * 2, i * 3));
    }
    engine.execute_sql(&s, &sql).expect("seed");

    // x = 2*i, y = 3*i. 100 rows, i in 0..100.
    // x > 50 → i > 25 → 74 rows.
    // y < 60 → i < 20 → 20 rows.
    // x > 50 AND y < 60 → no overlap → 0 rows.
    // x > 20 AND y > 30 → i > 10 AND i > 10 → 89 rows.
    // x BETWEEN 40 AND 100 AND y < 120 → 20<=i<=50 AND i<40 → i in [20,39] → 20 rows.
    let cases = [
        ("SELECT id FROM t WHERE x > 50 AND y < 60", 0),
        ("SELECT id FROM t WHERE x > 20 AND y > 30", 89),
        (
            "SELECT id FROM t WHERE x BETWEEN 40 AND 100 AND y < 120",
            20,
        ),
        ("SELECT id FROM t WHERE x >= 40 AND y >= 60 AND y < 90", 10),
    ];
    for (q, expected) in cases {
        let rows = query_rows(&engine, &s, q);
        assert_eq!(
            rows.len(),
            expected,
            "row count mismatch for `{q}`: got {} expected {expected}",
            rows.len()
        );
    }
}

/// Regression: SeqScan + `IS NULL` / `IS NOT NULL` pushdown must
/// agree with the executor-evaluated path. Exercises
/// `scan_table_null_filter`.
#[test]
fn seq_scan_null_filter_pushdown_agrees_with_full_eval() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE t (id INT, name TEXT)")
        .expect("create");
    let mut sql = String::from("INSERT INTO t (id, name) VALUES ");
    for i in 0..50 {
        if i > 0 {
            sql.push_str(", ");
        }
        if i % 4 == 0 {
            sql.push_str(&format!("({i}, NULL)"));
        } else {
            sql.push_str(&format!("({i}, 'row_{i}')"));
        }
    }
    engine.execute_sql(&s, &sql).expect("seed");

    // 50 rows total; every 4th row has NULL name → 13 nulls (i = 0,4,...,48), 37 non-null.
    let cases = [
        ("SELECT id FROM t WHERE name IS NULL", 13),
        ("SELECT id FROM t WHERE name IS NOT NULL", 37),
    ];
    for (q, expected) in cases {
        let rows = query_rows(&engine, &s, q);
        assert_eq!(
            rows.len(),
            expected,
            "row count mismatch for `{q}`: got {} expected {expected}",
            rows.len()
        );
    }
}

/// Regression: `COUNT(col)` over a `NOT NULL` column collapses to
/// the `COUNT(*)` fast path. Asserts equivalence vs the slow path.
#[test]
fn count_of_notnull_column_matches_count_star() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE t (id INT PRIMARY KEY, name TEXT)")
        .expect("create");
    let mut sql = String::from("INSERT INTO t (id, name) VALUES ");
    for i in 0..100 {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("({i}, 'r{i}')"));
    }
    engine.execute_sql(&s, &sql).expect("seed");

    // PK is NOT NULL — both forms must return the same total.
    let star = query_count(&engine, &s, "SELECT COUNT(*) FROM t");
    let by_pk = query_count(&engine, &s, "SELECT COUNT(id) FROM t");
    assert_eq!(star, 100);
    assert_eq!(by_pk, star, "COUNT(NOT NULL col) should equal COUNT(*)");

    // For a nullable column with NULLs present the two diverge — make
    // sure we did NOT incorrectly rewrite that case.
    engine
        .execute_sql(&s, "CREATE TABLE u (id INT PRIMARY KEY, opt INT)")
        .expect("create u");
    engine
        .execute_sql(
            &s,
            "INSERT INTO u VALUES (1, 10), (2, NULL), (3, 30), (4, NULL)",
        )
        .expect("seed u");
    let total = query_count(&engine, &s, "SELECT COUNT(*) FROM u");
    let non_null = query_count(&engine, &s, "SELECT COUNT(opt) FROM u");
    assert_eq!(total, 4);
    assert_eq!(non_null, 2, "COUNT(nullable) should skip NULLs");
}

#[test]
fn correlated_exists_caches_results_per_outer_correlation() {
    // Regression: a correlated EXISTS subquery whose outer correlation
    // values repeat across many outer rows must not re-execute the
    // sub-plan once per outer row. PG materialises the SubPlan output
    // keyed by `extParam`; aiondb's TLS `STATEMENT_CORRELATED_EXISTS_CACHE`
    // does the equivalent. Without the cache, the outer table on the
    // left side of the EXISTS scans the inner relation per row, so
    // wall-clock grows quadratically with `outer_rows × inner_rows`.
    //
    // The two outer rows below repeat the same correlation value (`fk`)
    // many times each, so a working cache keeps the inner sub-plan
    // execution count down to `distinct_correlation_values`, not
    // `outer_rows`.
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE outer_t (id INT PRIMARY KEY, fk INT)")
        .expect("create outer");
    engine
        .execute_sql(&s, "CREATE TABLE inner_t (id INT PRIMARY KEY, payload INT)")
        .expect("create inner");

    // 200 outer rows but only 4 distinct fk values — a working cache
    // turns 200 sub-plan executions into 4.
    let mut outer_sql = String::from("INSERT INTO outer_t (id, fk) VALUES ");
    for i in 0..200 {
        if i > 0 {
            outer_sql.push_str(", ");
        }
        outer_sql.push_str(&format!("({i}, {})", i % 4));
    }
    engine.execute_sql(&s, &outer_sql).expect("seed outer");

    let mut inner_sql = String::from("INSERT INTO inner_t (id, payload) VALUES ");
    for i in 0..4 {
        if i > 0 {
            inner_sql.push_str(", ");
        }
        inner_sql.push_str(&format!("({i}, {i})"));
    }
    engine.execute_sql(&s, &inner_sql).expect("seed inner");

    // Sanity: every outer row matches because every fk value 0..3 has
    // a corresponding inner row.
    let results = engine
        .execute_sql(
            &s,
            "SELECT COUNT(*) FROM outer_t WHERE EXISTS (SELECT 1 FROM inner_t WHERE inner_t.id = outer_t.fk)",
        )
        .expect("exists count");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected Query result, got {:?}", &results[0]);
    };
    let count = match &rows[0].values[0] {
        aiondb_core::Value::BigInt(n) => *n,
        aiondb_core::Value::Int(n) => i64::from(*n),
        other => panic!("unexpected count value: {other:?}"),
    };
    assert_eq!(count, 200, "all 200 outer rows should satisfy EXISTS");
}

#[test]
fn exists_correlated_perf_bench() {
    // Bench shape mirrors the user-reported `exists_correlated`
    // workload. Outer table = 5 000 rows, inner table = 5 000 rows,
    // correlation cardinality kept low so the per-outer-row sub-plan
    // execution would be the dominant cost without the correlated
    // EXISTS cache.
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE outer_t (id INT PRIMARY KEY, fk INT)")
        .expect("create outer");
    engine
        .execute_sql(&s, "CREATE TABLE inner_t (id INT PRIMARY KEY, payload INT)")
        .expect("create inner");

    let mut outer_sql = String::from("INSERT INTO outer_t (id, fk) VALUES ");
    for i in 0..ROWS {
        if i > 0 {
            outer_sql.push_str(", ");
        }
        outer_sql.push_str(&format!("({i}, {})", i % 50));
    }
    engine.execute_sql(&s, &outer_sql).expect("seed outer");

    let mut inner_sql = String::from("INSERT INTO inner_t (id, payload) VALUES ");
    for i in 0..ROWS {
        if i > 0 {
            inner_sql.push_str(", ");
        }
        inner_sql.push_str(&format!("({i}, {})", i));
    }
    engine.execute_sql(&s, &inner_sql).expect("seed inner");

    engine
        .execute_sql(&s, "ANALYZE outer_t")
        .expect("analyze outer");
    engine
        .execute_sql(&s, "ANALYZE inner_t")
        .expect("analyze inner");

    println!();
    println!(
        "EXISTS correlated bench  (outer={ROWS}, inner={ROWS}, repeats={REPEATS}, distinct_fk=50)"
    );
    timed("exists-correlated", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT COUNT(*) FROM outer_t WHERE EXISTS \
                 (SELECT 1 FROM inner_t WHERE inner_t.id = outer_t.fk)",
            )
            .expect("exists correlated");
    });
}

/// Regression: ANALYZE populates `correlation` on a clustered PK column.
/// Without correlation the cost model can't prefer an index range scan
/// over a SeqScan for open-ended ranges (`PG genericcostestimate`).
#[test]
fn analyze_records_index_correlation_for_clustered_pk() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE t (id INT PRIMARY KEY, val INT)")
        .expect("create");
    // The cost-model crossover where index range beats SeqScan needs a
    // table large enough that the page count outweighs index traversal —
    // 5000 rows / 64 B per row puts us well past it. Smaller tables
    // legitimately prefer SeqScan even with perfect correlation, which
    // matches PG.
    let mut sql = String::from("INSERT INTO t (id, val) VALUES ");
    for i in 0..5_000 {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("({i}, {i})"));
    }
    engine.execute_sql(&s, &sql).expect("seed");
    engine.execute_sql(&s, "ANALYZE t").expect("analyze");

    // After ANALYZE, an open-ended range that selects the tail of a
    // clustered PK should prefer the index even though the default
    // open-range selectivity is conservative — correlation collapses
    // the heap-fetch I/O to sequential cost (PG `genericcostestimate`).
    let path = access_path_for_query(&engine, &s, "SELECT id FROM t WHERE id >= 4900");
    assert!(
        matches!(
            path,
            aiondb_plan::ScanAccessPath::IndexRange { .. }
                | aiondb_plan::ScanAccessPath::IndexOnlyScan { .. }
        ),
        "expected IndexRange/IndexOnlyScan after ANALYZE, got {path:?}"
    );
}
