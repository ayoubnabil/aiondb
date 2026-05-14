#![allow(clippy::pedantic)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Instant;

use super::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_int_ids(results: &[StatementResult]) -> Vec<i32> {
    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => rows
            .iter()
            .map(|r| match &r.values[0] {
                Value::Int(i) => *i,
                other => panic!("expected Int, got {other:?}"),
            })
            .collect(),
        other => panic!("expected Query, got {other:?}"),
    }
}

/// Deterministic 3D vector from integer seed.
fn vec3(seed: usize) -> String {
    let x = ((seed * 7 + 3) % 100) as f64 / 10.0;
    let y = ((seed * 13 + 5) % 100) as f64 / 10.0;
    let z = ((seed * 19 + 7) % 100) as f64 / 10.0;
    format!("[{x},{y},{z}]")
}

/// Deterministic N-dimensional vector from integer seed.
fn vec_nd(seed: usize, dims: usize) -> String {
    let vals: Vec<String> = (0..dims)
        .map(|d| {
            let v = ((seed * (d + 7) + d * 3 + 1) % 100) as f64 / 10.0;
            format!("{v}")
        })
        .collect();
    format!("[{}]", vals.join(","))
}

/// Ground-truth L2 distance between two seed-generated 3D vectors.
fn l2_3d(a: usize, b: usize) -> f64 {
    let f = |s: usize| {
        (
            ((s * 7 + 3) % 100) as f64 / 10.0,
            ((s * 13 + 5) % 100) as f64 / 10.0,
            ((s * 19 + 7) % 100) as f64 / 10.0,
        )
    };
    let (ax, ay, az) = f(a);
    let (bx, by, bz) = f(b);
    ((ax - bx).powi(2) + (ay - by).powi(2) + (az - bz).powi(2)).sqrt()
}

/// Insert N vectors (seeds 1..=n) into a table that has columns (id, v, q).
fn seed_vectors(engine: &Engine, session: &SessionHandle, table: &str, n: usize) {
    let qv = vec3(0);
    for i in 1..=n {
        let v = vec3(i);
        engine
            .execute_sql(
                session,
                &format!("INSERT INTO {table} VALUES ({i}, '{v}', '{qv}')"),
            )
            .expect("seed insert");
    }
}

// ---------------------------------------------------------------------------
// 1. Recall stability under continuous inserts
// ---------------------------------------------------------------------------

#[test]
fn recall_stability_under_continuous_inserts() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE recall_t (id INT, v VECTOR(3), q VECTOR(3)); \
             CREATE INDEX idx_recall ON recall_t USING hnsw (v)",
        )
        .expect("setup");

    const BATCHES: usize = 5;
    const BATCH_SIZE: usize = 40;
    let qv = vec3(0);

    for batch in 0..BATCHES {
        for i in 0..BATCH_SIZE {
            let id = batch * BATCH_SIZE + i + 1;
            let v = vec3(id);
            engine
                .execute_sql(
                    &session,
                    &format!("INSERT INTO recall_t VALUES ({id}, '{v}', '{qv}')"),
                )
                .expect("insert");
        }

        let total = (batch + 1) * BATCH_SIZE;
        let k = 5.min(total);

        // Ground-truth top-k nearest to seed=0.
        let mut dists: Vec<(usize, f64)> = (1..=total).map(|id| (id, l2_3d(id, 0))).collect();
        dists.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        let true_ids: Vec<usize> = dists.iter().take(k).map(|(id, _)| *id).collect();

        let results = engine
            .execute_sql(
                &session,
                &format!("SELECT id FROM recall_t ORDER BY l2_distance(v, q) LIMIT {k}"),
            )
            .expect("nn query");

        let got = extract_int_ids(&results);
        let hits = got
            .iter()
            .filter(|&&id| true_ids.contains(&(id as usize)))
            .count();
        let recall = hits as f64 / k as f64;

        assert!(
            recall >= 0.6,
            "batch {batch}: recall {recall:.2} ({hits}/{k}) < 60% after {total} inserts"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. HNSW memory budget respected under load
// ---------------------------------------------------------------------------

#[test]
fn hnsw_memory_budget_respected_under_load() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE budget_t (id INT, v VECTOR(3)); \
             CREATE INDEX idx_budget ON budget_t USING hnsw (v)",
        )
        .expect("setup");

    // Insert many vectors; track successful inserts.
    const TOTAL: usize = 500;
    let mut ok_count = 0usize;

    for i in 0..TOTAL {
        let v = vec3(i);
        match engine.execute_sql(
            &session,
            &format!("INSERT INTO budget_t VALUES ({i}, '{v}')"),
        ) {
            Ok(_) => ok_count += 1,
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("budget") || msg.contains("memory"),
                    "unexpected error: {e}"
                );
                break;
            }
        }
    }

    let count = query_count(&engine, &session, "SELECT COUNT(*) FROM budget_t");
    assert_eq!(count, ok_count as i64, "row count should match inserts");

    // Engine should still be functional after heavy load.
    let results = engine
        .execute_sql(&session, "SELECT COUNT(*) FROM budget_t")
        .expect("count after load");
    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => assert!(!rows.is_empty()),
        other => panic!("expected Query, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 3. Concurrent vector insert and search
// ---------------------------------------------------------------------------

#[test]
fn concurrent_vector_insert_and_search() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (setup, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &setup,
            "CREATE TABLE conc_vec (id INT, v VECTOR(3), q VECTOR(3)); \
             CREATE INDEX idx_conc ON conc_vec USING hnsw (v)",
        )
        .expect("setup");

    seed_vectors(&engine, &setup, "conc_vec", 20);

    const INS_T: usize = 3;
    const INS_N: usize = 30;
    const SEARCH_T: usize = 3;
    const SEARCH_N: usize = 20;

    let had_error = AtomicBool::new(false);
    let ins_ok = AtomicUsize::new(0);
    let qv = vec3(0);

    thread::scope(|s| {
        for t in 0..INS_T {
            let engine = &engine;
            let had_error = &had_error;
            let ins_ok = &ins_ok;
            let qv = &qv;
            s.spawn(move || {
                let (sess, _) = engine.startup(startup_params()).expect("startup");
                for i in 0..INS_N {
                    let id = 1000 + t * INS_N + i;
                    let v = vec3(id);
                    match engine.execute_sql(
                        &sess,
                        &format!("INSERT INTO conc_vec VALUES ({id}, '{v}', '{qv}')"),
                    ) {
                        Ok(_) => {
                            ins_ok.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            had_error.store(true, Ordering::SeqCst);
                            panic!("inserter {t}/{i}: {e}");
                        }
                    }
                }
            });
        }

        for t in 0..SEARCH_T {
            let engine = &engine;
            let had_error = &had_error;
            s.spawn(move || {
                let (sess, _) = engine.startup(startup_params()).expect("startup");
                for i in 0..SEARCH_N {
                    match engine.execute_sql(
                        &sess,
                        "SELECT id FROM conc_vec ORDER BY l2_distance(v, q) LIMIT 5",
                    ) {
                        Ok(r) => {
                            if let Some(StatementResult::Query { rows, .. }) = r.last() {
                                assert!(!rows.is_empty(), "search {t}/{i}: empty");
                            }
                        }
                        Err(e) => {
                            had_error.store(true, Ordering::SeqCst);
                            panic!("searcher {t}/{i}: {e}");
                        }
                    }
                }
            });
        }
    });

    assert!(!had_error.load(Ordering::SeqCst), "concurrent errors");
    let total = ins_ok.load(Ordering::Relaxed);
    assert_eq!(total, INS_T * INS_N, "all inserts should succeed");
    let count = query_count(&engine, &setup, "SELECT COUNT(*) FROM conc_vec");
    assert_eq!(count, 20 + total as i64, "final count mismatch");
}

// ---------------------------------------------------------------------------
// 4. Vector index rebuild after deletes
// ---------------------------------------------------------------------------

#[test]
fn vector_index_rebuild_after_deletes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE del_vec (id INT, v VECTOR(3), q VECTOR(3)); \
             CREATE INDEX idx_del ON del_vec USING hnsw (v)",
        )
        .expect("setup");

    seed_vectors(&engine, &session, "del_vec", 50);

    // Delete odd-ID vectors.
    for i in (1..=50).step_by(2) {
        engine
            .execute_sql(&session, &format!("DELETE FROM del_vec WHERE id = {i}"))
            .expect("delete");
    }

    assert_eq!(
        query_count(&engine, &session, "SELECT COUNT(*) FROM del_vec"),
        25
    );

    // NN search should return only even-ID vectors.
    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM del_vec ORDER BY l2_distance(v, q) LIMIT 10",
        )
        .expect("nn after delete");

    let ids = extract_int_ids(&results);
    assert_eq!(ids.len(), 10);
    for id in &ids {
        assert!(id % 2 == 0, "id {id} should be even (odd IDs deleted)");
    }

    // Insert more vectors after deletion.
    let qv = vec3(0);
    for i in 51..=70 {
        let v = vec3(i);
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO del_vec VALUES ({i}, '{v}', '{qv}')"),
            )
            .expect("insert after delete");
    }

    assert_eq!(
        query_count(&engine, &session, "SELECT COUNT(*) FROM del_vec"),
        45
    );

    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM del_vec ORDER BY l2_distance(v, q) LIMIT 5",
        )
        .expect("nn after re-insert");
    let ids = extract_int_ids(&results);
    assert_eq!(ids.len(), 5, "should return 5 neighbors after re-insert");
}

// ---------------------------------------------------------------------------
// 5. Vector search latency budget
// ---------------------------------------------------------------------------

#[test]
fn vector_search_latency_budget() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE lat_vec (id INT, v VECTOR(3), q VECTOR(3)); \
             CREATE INDEX idx_lat ON lat_vec USING hnsw (v)",
        )
        .expect("setup");

    seed_vectors(&engine, &session, "lat_vec", 200);

    const SEARCHES: usize = 20;
    const BUDGET_MS: u128 = 2000;

    // Individual search latency check.
    for s in 0..SEARCHES {
        let start = Instant::now();
        let results = engine
            .execute_sql(
                &session,
                "SELECT id, l2_distance(v, q) AS dist FROM lat_vec ORDER BY dist LIMIT 10",
            )
            .expect("search");
        let ms = start.elapsed().as_millis();

        match results.last().unwrap() {
            StatementResult::Query { rows, .. } => {
                assert_eq!(rows.len(), 10, "search {s}: expected 10 rows");
            }
            other => panic!("search {s}: expected Query, got {other:?}"),
        }
        assert!(ms < BUDGET_MS, "search {s}: {ms}ms > {BUDGET_MS}ms budget");
    }

    // Average latency check.
    let start = Instant::now();
    for _ in 0..SEARCHES {
        engine
            .execute_sql(
                &session,
                "SELECT id FROM lat_vec ORDER BY l2_distance(v, q) LIMIT 10",
            )
            .expect("batch search");
    }
    let avg_ms = start.elapsed().as_millis() / SEARCHES as u128;
    assert!(avg_ms < BUDGET_MS, "avg {avg_ms}ms > {BUDGET_MS}ms budget");
}

// ---------------------------------------------------------------------------
// 6. Large dimension vectors (128D and 256D)
// ---------------------------------------------------------------------------

#[test]
fn large_dimension_vectors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // -- 128-dimensional vectors --
    const DIMS: usize = 128;
    engine
        .execute_sql(
            &session,
            &format!(
                "CREATE TABLE ldim (id INT, v VECTOR({DIMS}), q VECTOR({DIMS})); \
                 CREATE INDEX idx_ldim ON ldim USING hnsw (v)"
            ),
        )
        .expect("setup 128d");

    let qv = vec_nd(0, DIMS);
    for i in 1..=50 {
        let v = vec_nd(i, DIMS);
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO ldim VALUES ({i}, '{v}', '{qv}')"),
            )
            .expect("insert 128d");
    }

    assert_eq!(
        query_count(&engine, &session, "SELECT COUNT(*) FROM ldim"),
        50
    );

    // NN search should return ordered distances.
    let results = engine
        .execute_sql(
            &session,
            "SELECT id, l2_distance(v, q) AS dist FROM ldim ORDER BY dist LIMIT 5",
        )
        .expect("nn 128d");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 5);
            let dists: Vec<f64> = rows
                .iter()
                .map(|r| match &r.values[1] {
                    Value::Double(d) => *d,
                    other => panic!("expected Double, got {other:?}"),
                })
                .collect();
            for w in dists.windows(2) {
                assert!(w[0] <= w[1] + 1e-10, "{:.4} > {:.4}", w[0], w[1]);
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }

    // -- 256-dimensional vectors --
    const DIMS2: usize = 256;
    engine
        .execute_sql(
            &session,
            &format!(
                "CREATE TABLE ldim2 (id INT, v VECTOR({DIMS2})); \
                 CREATE INDEX idx_ldim2 ON ldim2 USING hnsw (v)"
            ),
        )
        .expect("setup 256d");

    for i in 1..=20 {
        let v = vec_nd(i, DIMS2);
        engine
            .execute_sql(&session, &format!("INSERT INTO ldim2 VALUES ({i}, '{v}')"))
            .expect("insert 256d");
    }

    assert_eq!(
        query_count(&engine, &session, "SELECT COUNT(*) FROM ldim2"),
        20
    );

    // Self-distance must be zero.
    let results = engine
        .execute_sql(&session, "SELECT l2_distance(v, v) AS d FROM ldim2 LIMIT 5")
        .expect("self-dist 256d");
    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            for row in rows {
                let d = match &row.values[0] {
                    Value::Double(d) => *d,
                    other => panic!("expected Double, got {other:?}"),
                };
                assert!(d.abs() < 1e-6, "self-distance {d} should be ~0");
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 7. Vector recovery after simulated crash (shared storage)
// ---------------------------------------------------------------------------

#[test]
fn vector_recovery_after_crash() {
    use std::sync::Arc;

    // Use shared catalog + storage so a "new" engine sees the same data.
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage.clone());
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE crash_vec (id INT, v VECTOR(3), q VECTOR(3)); \
             CREATE INDEX idx_crash ON crash_vec USING hnsw (v)",
        )
        .expect("setup");

    seed_vectors(&engine, &session, "crash_vec", 30);

    // Record pre-crash NN results.
    let pre = engine
        .execute_sql(
            &session,
            "SELECT id FROM crash_vec ORDER BY l2_distance(v, q) LIMIT 5",
        )
        .expect("pre-crash nn");
    let pre_ids = extract_int_ids(&pre);

    // Simulate crash: drop old engine, build new one on same storage.
    drop(engine);
    let engine2 = build_engine_with_store(catalog, storage);
    let (session2, _) = engine2.startup(startup_params()).expect("startup2");

    assert_eq!(
        query_count(&engine2, &session2, "SELECT COUNT(*) FROM crash_vec"),
        30,
        "row count should survive crash"
    );

    // Post-crash NN results should match (data persisted in shared storage).
    let post = engine2
        .execute_sql(
            &session2,
            "SELECT id FROM crash_vec ORDER BY l2_distance(v, q) LIMIT 5",
        )
        .expect("post-crash nn");
    let post_ids = extract_int_ids(&post);

    assert_eq!(pre_ids, post_ids, "NN results should match after recovery");
}
