use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use aiondb_embedded::Database;
use aiondb_engine::EngineBuilder;

use crate::harness::{SuiteResult, SuiteStats, TestDb};

pub fn run_all() -> SuiteResult {
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    match concurrent_increment_accounting() {
        Ok(count) => passed += count,
        Err(error) => failures.push(format!("concurrent_increment_accounting: {error}")),
    }

    match concurrent_upsert_same_keyspace() {
        Ok(count) => passed += count,
        Err(error) => failures.push(format!("concurrent_upsert_same_keyspace: {error}")),
    }

    match concurrent_ledger_insert_integrity() {
        Ok(count) => passed += count,
        Err(error) => failures.push(format!("concurrent_ledger_insert_integrity: {error}")),
    }

    match concurrent_phantom_read_guard() {
        Ok(count) => passed += count,
        Err(error) => failures.push(format!("concurrent_phantom_read_guard: {error}")),
    }
    match concurrent_write_skew_detection() {
        Ok(count) => passed += count,
        Err(error) => failures.push(format!("concurrent_write_skew_detection: {error}")),
    }
    match concurrent_lost_update_guard() {
        Ok(count) => passed += count,
        Err(error) => failures.push(format!("concurrent_lost_update_guard: {error}")),
    }
    match concurrent_bulk_delete_reinsert() {
        Ok(count) => passed += count,
        Err(error) => failures.push(format!("concurrent_bulk_delete_reinsert: {error}")),
    }
    match concurrent_serial_gap_stress() {
        Ok(count) => passed += count,
        Err(error) => failures.push(format!("concurrent_serial_gap_stress: {error}")),
    }
    match concurrent_aggregate_consistency() {
        Ok(count) => passed += count,
        Err(error) => failures.push(format!("concurrent_aggregate_consistency: {error}")),
    }
    match thundering_herd_point_lookup() {
        Ok(count) => passed += count,
        Err(error) => failures.push(format!("thundering_herd_point_lookup: {error}")),
    }
    match concurrent_cascading_rollback() {
        Ok(count) => passed += count,
        Err(error) => failures.push(format!("concurrent_cascading_rollback: {error}")),
    }
    match concurrent_hot_row_toggle() {
        Ok(count) => passed += count,
        Err(error) => failures.push(format!("concurrent_hot_row_toggle: {error}")),
    }
    match concurrent_table_scan_under_mutation() {
        Ok(count) => passed += count,
        Err(error) => failures.push(format!("concurrent_table_scan_under_mutation: {error}")),
    }
    match concurrent_multi_table_join_integrity() {
        Ok(count) => passed += count,
        Err(error) => failures.push(format!("concurrent_multi_table_join_integrity: {error}")),
    }
    match concurrent_batch_upsert_sum_invariant() {
        Ok(count) => passed += count,
        Err(error) => failures.push(format!("concurrent_batch_upsert_sum_invariant: {error}")),
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

fn concurrent_increment_accounting() -> Result<usize, String> {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .build()
            .map_err(|error| format!("build engine failed: {error}"))?,
    );
    let db = Database::new(Arc::clone(&engine));
    let bootstrap = db
        .connect_anonymous("default", "race-bootstrap")
        .map_err(|error| format!("bootstrap connection failed: {error}"))?;
    TestDb::exec_ok(
        &bootstrap,
        "CREATE TABLE race_counter (id INTEGER PRIMARY KEY, val INTEGER NOT NULL)",
    );
    TestDb::exec_ok(&bootstrap, "INSERT INTO race_counter VALUES (1, 0)");

    let successful_commits = Arc::new(AtomicUsize::new(0));
    let thread_failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    const THREADS: usize = 8;
    const ITERATIONS: usize = 180;
    std::thread::scope(|scope| {
        for tid in 0..THREADS {
            let engine_ref = Arc::clone(&engine);
            let committed_ref = Arc::clone(&successful_commits);
            let failures_ref = Arc::clone(&thread_failures);
            scope.spawn(move || {
                let worker_db = Database::new(engine_ref);
                let conn = match worker_db.connect_anonymous("default", format!("race-inc-{tid}")) {
                    Ok(conn) => conn,
                    Err(error) => {
                        if let Ok(mut locked) = failures_ref.lock() {
                            locked.push(format!("thread {tid}: connect failed: {error}"));
                        }
                        return;
                    }
                };

                for _ in 0..ITERATIONS {
                    let mut committed = false;
                    for _ in 0..5 {
                        if conn.execute("BEGIN").is_err() {
                            std::thread::yield_now();
                            continue;
                        }
                        let updated = conn
                            .execute("UPDATE race_counter SET val = val + 1 WHERE id = 1")
                            .is_ok();
                        if updated && conn.execute("COMMIT").is_ok() {
                            committed_ref.fetch_add(1, Ordering::Relaxed);
                            committed = true;
                            break;
                        }
                        let _ = conn.execute("ROLLBACK");
                        std::thread::yield_now();
                    }

                    if !committed {
                        std::thread::yield_now();
                    }
                }
            });
        }
    });

    if let Ok(locked) = thread_failures.lock() {
        if !locked.is_empty() {
            return Err(locked.join("; "));
        }
    }

    let expected = successful_commits.load(Ordering::Relaxed);
    let observed = TestDb::scalar(&bootstrap, "SELECT val FROM race_counter WHERE id = 1")
        .map_err(|error| format!("read final counter failed: {error}"))?
        .parse::<usize>()
        .map_err(|error| format!("counter parse failed: {error}"))?;

    let drift = observed.abs_diff(expected);
    if drift > THREADS {
        return Err(format!(
            "counter drift too large after contention: expected around {expected}, got {observed}, drift={drift}"
        ));
    }

    Ok(1)
}

fn concurrent_upsert_same_keyspace() -> Result<usize, String> {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .build()
            .map_err(|error| format!("build engine failed: {error}"))?,
    );
    let db = Database::new(Arc::clone(&engine));
    let bootstrap = db
        .connect_anonymous("default", "race-upsert-bootstrap")
        .map_err(|error| format!("bootstrap connection failed: {error}"))?;
    TestDb::exec_ok(
        &bootstrap,
        "CREATE TABLE race_upsert (id INTEGER PRIMARY KEY, v INTEGER NOT NULL)",
    );

    let thread_failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    const THREADS: usize = 10;
    const ITERATIONS: usize = 240;
    const KEYSPACE: usize = 97;

    std::thread::scope(|scope| {
        for tid in 0..THREADS {
            let engine_ref = Arc::clone(&engine);
            let failures_ref = Arc::clone(&thread_failures);
            scope.spawn(move || {
                let worker_db = Database::new(engine_ref);
                let conn =
                    match worker_db.connect_anonymous("default", format!("race-upsert-{tid}")) {
                        Ok(conn) => conn,
                        Err(error) => {
                            if let Ok(mut locked) = failures_ref.lock() {
                                locked.push(format!("thread {tid}: connect failed: {error}"));
                            }
                            return;
                        }
                    };

                for i in 0..ITERATIONS {
                    let id = ((tid * 13 + i * 7) % KEYSPACE) + 1;
                    let v = ((i * 41 + tid * 17) % 100_000) as i64;
                    let sql = format!(
                        "INSERT INTO race_upsert (id, v) VALUES ({id}, {v}) \
                         ON CONFLICT (id) DO UPDATE SET v = EXCLUDED.v"
                    );
                    if conn.execute(&sql).is_err() {
                        std::thread::yield_now();
                    }
                }
            });
        }
    });

    if let Ok(locked) = thread_failures.lock() {
        if !locked.is_empty() {
            return Err(locked.join("; "));
        }
    }

    let rows = TestDb::query_strings(
        &bootstrap,
        "SELECT count(*), count(DISTINCT id) FROM race_upsert",
    )
    .map_err(|error| format!("final upsert check failed: {error}"))?;
    let first_row = rows
        .first()
        .ok_or_else(|| "no row returned for upsert invariant".to_owned())?;
    let total_rows = first_row
        .first()
        .ok_or_else(|| "missing total row count".to_owned())?
        .parse::<usize>()
        .map_err(|error| format!("total row count parse failed: {error}"))?;
    let distinct_rows = first_row
        .get(1)
        .ok_or_else(|| "missing distinct count".to_owned())?
        .parse::<usize>()
        .map_err(|error| format!("distinct row count parse failed: {error}"))?;

    if total_rows != distinct_rows {
        return Err(format!(
            "primary-key integrity mismatch: count(*)={total_rows}, count(distinct id)={distinct_rows}"
        ));
    }
    if total_rows > KEYSPACE {
        return Err(format!(
            "unexpected row count beyond keyspace: rows={total_rows}, keyspace={KEYSPACE}"
        ));
    }

    Ok(1)
}

fn concurrent_ledger_insert_integrity() -> Result<usize, String> {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .build()
            .map_err(|error| format!("build engine failed: {error}"))?,
    );
    let db = Database::new(Arc::clone(&engine));
    let bootstrap = db
        .connect_anonymous("default", "race-ledger-bootstrap")
        .map_err(|error| format!("bootstrap connection failed: {error}"))?;
    TestDb::exec_ok(
        &bootstrap,
        "CREATE TABLE race_ledger (id BIGINT PRIMARY KEY, delta INTEGER NOT NULL)",
    );

    let inserted_rows = Arc::new(AtomicUsize::new(0));
    let expected_sum = Arc::new(AtomicI64::new(0));
    let thread_failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    const THREADS: usize = 6;
    const ITERATIONS: usize = 420;
    std::thread::scope(|scope| {
        for tid in 0..THREADS {
            let engine_ref = Arc::clone(&engine);
            let inserted_ref = Arc::clone(&inserted_rows);
            let sum_ref = Arc::clone(&expected_sum);
            let failures_ref = Arc::clone(&thread_failures);
            scope.spawn(move || {
                let worker_db = Database::new(engine_ref);
                let conn =
                    match worker_db.connect_anonymous("default", format!("race-ledger-{tid}")) {
                        Ok(conn) => conn,
                        Err(error) => {
                            if let Ok(mut locked) = failures_ref.lock() {
                                locked.push(format!("thread {tid}: connect failed: {error}"));
                            }
                            return;
                        }
                    };

                for i in 0..ITERATIONS {
                    let id = (tid as i64 * 1_000_000_i64) + i as i64;
                    let delta = if (i + tid) % 2 == 0 { 1_i64 } else { -1_i64 };
                    let sql = format!("INSERT INTO race_ledger (id, delta) VALUES ({id}, {delta})");

                    let mut inserted = false;
                    for _ in 0..3 {
                        if conn.execute(&sql).is_ok() {
                            inserted = true;
                            break;
                        }
                        std::thread::yield_now();
                    }
                    if inserted {
                        inserted_ref.fetch_add(1, Ordering::Relaxed);
                        sum_ref.fetch_add(delta, Ordering::Relaxed);
                    }
                }
            });
        }
    });

    if let Ok(locked) = thread_failures.lock() {
        if !locked.is_empty() {
            return Err(locked.join("; "));
        }
    }

    let rows = TestDb::query_strings(
        &bootstrap,
        "SELECT count(*), coalesce(sum(delta), 0) FROM race_ledger",
    )
    .map_err(|error| format!("ledger final check failed: {error}"))?;
    let first_row = rows
        .first()
        .ok_or_else(|| "no row returned for ledger invariant".to_owned())?;
    let observed_count = first_row
        .first()
        .ok_or_else(|| "missing ledger count".to_owned())?
        .parse::<usize>()
        .map_err(|error| format!("ledger count parse failed: {error}"))?;
    let observed_sum = first_row
        .get(1)
        .ok_or_else(|| "missing ledger sum".to_owned())?
        .parse::<i64>()
        .map_err(|error| format!("ledger sum parse failed: {error}"))?;

    let expected_count = inserted_rows.load(Ordering::Relaxed);
    let expected_sum_value = expected_sum.load(Ordering::Relaxed);
    if observed_count != expected_count {
        return Err(format!(
            "ledger count mismatch: expected {expected_count}, got {observed_count}"
        ));
    }
    if observed_sum != expected_sum_value {
        return Err(format!(
            "ledger sum mismatch: expected {expected_sum_value}, got {observed_sum}"
        ));
    }

    Ok(1)
}

fn concurrent_phantom_read_guard() -> Result<usize, String> {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .build()
            .map_err(|e| e.to_string())?,
    );
    let db = Database::new(Arc::clone(&engine));
    let bootstrap = db
        .connect_anonymous("default", "phantom-boot")
        .map_err(|e| e.to_string())?;
    TestDb::exec_ok(
        &bootstrap,
        "CREATE TABLE race_phantom (id INTEGER PRIMARY KEY, val INTEGER)",
    );
    for i in 0..50 {
        TestDb::exec_ok(
            &bootstrap,
            &format!("INSERT INTO race_phantom VALUES ({i}, {i})"),
        );
    }
    let violations = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        let ew = Arc::clone(&engine);
        scope.spawn(move || {
            let wdb = Database::new(ew);
            let wc = wdb.connect_anonymous("default", "phantom-w").unwrap();
            for i in 50..150 {
                let _ = wc.execute(&format!("INSERT INTO race_phantom VALUES ({i}, {i})"));
                std::thread::yield_now();
            }
        });
        let er = Arc::clone(&engine);
        let vr = Arc::clone(&violations);
        scope.spawn(move || {
            let rdb = Database::new(er);
            let rc = rdb.connect_anonymous("default", "phantom-r").unwrap();
            let mut last = 0_i64;
            for _ in 0..200 {
                if let Ok(v) = TestDb::scalar(&rc, "SELECT count(*) FROM race_phantom") {
                    if let Ok(c) = v.parse::<i64>() {
                        if c < last {
                            vr.fetch_add(1, Ordering::Relaxed);
                        }
                        last = c;
                    }
                }
            }
        });
    });
    if violations.load(Ordering::Relaxed) > 0 {
        return Err("count decreased between reads".to_owned());
    }
    Ok(1)
}

fn concurrent_write_skew_detection() -> Result<usize, String> {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .build()
            .map_err(|e| e.to_string())?,
    );
    let db = Database::new(Arc::clone(&engine));
    let boot = db
        .connect_anonymous("default", "skew-boot")
        .map_err(|e| e.to_string())?;
    TestDb::exec_ok(
        &boot,
        "CREATE TABLE race_skew (account INTEGER PRIMARY KEY, balance INTEGER)",
    );
    TestDb::exec_ok(&boot, "INSERT INTO race_skew VALUES (1, 100), (2, 100)");
    let violations = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for tid in 0..4_usize {
            let er = Arc::clone(&engine);
            scope.spawn(move || {
                let wdb = Database::new(er);
                let c = wdb
                    .connect_anonymous("default", format!("skew-{tid}"))
                    .unwrap();
                for _ in 0..80 {
                    let (f, t) = if tid % 2 == 0 { (1, 2) } else { (2, 1) };
                    let _ = c.execute("BEGIN");
                    let _ = c.execute(&format!(
                        "UPDATE race_skew SET balance = balance - 5 WHERE account = {f}"
                    ));
                    let _ = c.execute(&format!(
                        "UPDATE race_skew SET balance = balance + 5 WHERE account = {t}"
                    ));
                    let _ = c.execute("COMMIT");
                }
            });
            let ec = Arc::clone(&engine);
            let vc = Arc::clone(&violations);
            scope.spawn(move || {
                let rdb = Database::new(ec);
                let c = rdb
                    .connect_anonymous("default", format!("chk-{tid}"))
                    .unwrap();
                for _ in 0..150 {
                    if let Ok(v) = TestDb::scalar(&c, "SELECT sum(balance) FROM race_skew") {
                        if let Ok(s) = v.parse::<i64>() {
                            if s != 200 {
                                vc.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    std::thread::yield_now();
                }
            });
        }
    });
    if violations.load(Ordering::Relaxed) > 0 {
        return Err("balance sum != 200".to_owned());
    }
    Ok(1)
}

fn concurrent_lost_update_guard() -> Result<usize, String> {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .build()
            .map_err(|e| e.to_string())?,
    );
    let db = Database::new(Arc::clone(&engine));
    let boot = db
        .connect_anonymous("default", "lost-boot")
        .map_err(|e| e.to_string())?;
    TestDb::exec_ok(
        &boot,
        "CREATE TABLE race_lost (id INTEGER PRIMARY KEY, counter INTEGER)",
    );
    TestDb::exec_ok(&boot, "INSERT INTO race_lost VALUES (1, 0)");
    let success = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for tid in 0..6 {
            let er = Arc::clone(&engine);
            let sr = Arc::clone(&success);
            scope.spawn(move || {
                let wdb = Database::new(er);
                let c = wdb
                    .connect_anonymous("default", format!("lost-{tid}"))
                    .unwrap();
                for _ in 0..120 {
                    if c.execute("UPDATE race_lost SET counter = counter + 1 WHERE id = 1")
                        .is_ok()
                    {
                        sr.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    });
    let expected = success.load(Ordering::Relaxed);
    let observed = TestDb::scalar(&boot, "SELECT counter FROM race_lost WHERE id = 1")
        .map_err(|e| e.clone())?
        .parse::<usize>()
        .map_err(|e| e.to_string())?;
    let drift = observed.abs_diff(expected);
    if drift > 6 {
        return Err(format!(
            "lost update drift={drift}: expected ~{expected}, got {observed}"
        ));
    }
    Ok(1)
}

fn concurrent_bulk_delete_reinsert() -> Result<usize, String> {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .build()
            .map_err(|e| e.to_string())?,
    );
    let db = Database::new(Arc::clone(&engine));
    let boot = db
        .connect_anonymous("default", "bulkdr-boot")
        .map_err(|e| e.to_string())?;
    TestDb::exec_ok(
        &boot,
        "CREATE TABLE race_bulkdr (id INTEGER PRIMARY KEY, gen INTEGER)",
    );
    std::thread::scope(|scope| {
        for tid in 0..4_usize {
            let er = Arc::clone(&engine);
            scope.spawn(move || {
                let wdb = Database::new(er);
                let c = wdb
                    .connect_anonymous("default", format!("bulkdr-{tid}"))
                    .unwrap();
                let base = (tid * 100) as i64;
                for gen in 0..40_i64 {
                    let _ = c.execute(&format!(
                        "DELETE FROM race_bulkdr WHERE id >= {base} AND id < {}",
                        base + 10
                    ));
                    for i in 0..10_i64 {
                        let _ = c.execute(&format!(
                            "INSERT INTO race_bulkdr VALUES ({}, {gen})",
                            base + i
                        ));
                    }
                }
            });
        }
    });
    let total = TestDb::scalar(&boot, "SELECT count(*) FROM race_bulkdr")
        .map_err(|e| e.clone())?
        .parse::<usize>()
        .map_err(|e| e.to_string())?;
    if total != 40 {
        return Err(format!("expected 40 rows, got {total}"));
    }
    Ok(1)
}

fn concurrent_serial_gap_stress() -> Result<usize, String> {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .build()
            .map_err(|e| e.to_string())?,
    );
    let db = Database::new(Arc::clone(&engine));
    let boot = db
        .connect_anonymous("default", "serial-boot")
        .map_err(|e| e.to_string())?;
    TestDb::exec_ok(
        &boot,
        "CREATE TABLE race_serial_stress (id SERIAL PRIMARY KEY, tid INTEGER)",
    );
    let inserted = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for tid in 0..8_usize {
            let er = Arc::clone(&engine);
            let ins = Arc::clone(&inserted);
            scope.spawn(move || {
                let wdb = Database::new(er);
                let c = wdb
                    .connect_anonymous("default", format!("serial-{tid}"))
                    .unwrap();
                for _ in 0..80 {
                    if c.execute(&format!(
                        "INSERT INTO race_serial_stress (tid) VALUES ({tid})"
                    ))
                    .is_ok()
                    {
                        ins.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    });
    let total = inserted.load(Ordering::Relaxed);
    let count = TestDb::scalar(&boot, "SELECT count(*) FROM race_serial_stress")
        .map_err(|e| e.clone())?
        .parse::<usize>()
        .map_err(|e| e.to_string())?;
    if count != total {
        return Err(format!("serial count: inserted={total}, found={count}"));
    }
    let distinct = TestDb::scalar(&boot, "SELECT count(DISTINCT id) FROM race_serial_stress")
        .map_err(|e| e.clone())?
        .parse::<usize>()
        .map_err(|e| e.to_string())?;
    if distinct != count {
        return Err(format!(
            "serial collision: count={count}, distinct={distinct}"
        ));
    }
    Ok(1)
}

fn concurrent_aggregate_consistency() -> Result<usize, String> {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .build()
            .map_err(|e| e.to_string())?,
    );
    let db = Database::new(Arc::clone(&engine));
    let boot = db
        .connect_anonymous("default", "aggcon-boot")
        .map_err(|e| e.to_string())?;
    TestDb::exec_ok(
        &boot,
        "CREATE TABLE race_aggcon (id INTEGER PRIMARY KEY, val INTEGER)",
    );
    for i in 0..10 {
        TestDb::exec_ok(&boot, &format!("INSERT INTO race_aggcon VALUES ({i}, 100)"));
    }
    let successful_reads = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for tid in 0..4_usize {
            let er = Arc::clone(&engine);
            scope.spawn(move || {
                let wdb = Database::new(er);
                let c = wdb
                    .connect_anonymous("default", format!("aggw-{tid}"))
                    .unwrap();
                for i in 0..100_usize {
                    let f = (tid + i) % 10;
                    let t = (tid + i + 3) % 10;
                    if f == t {
                        continue;
                    }
                    let _ = c.execute("BEGIN");
                    let _ = c.execute(&format!(
                        "UPDATE race_aggcon SET val = val - 1 WHERE id = {f}"
                    ));
                    let _ = c.execute(&format!(
                        "UPDATE race_aggcon SET val = val + 1 WHERE id = {t}"
                    ));
                    let _ = c.execute("COMMIT");
                }
            });
        }
        for tid in 0..4 {
            let er = Arc::clone(&engine);
            let v = Arc::clone(&successful_reads);
            scope.spawn(move || {
                let rdb = Database::new(er);
                let c = rdb
                    .connect_anonymous("default", format!("aggr-{tid}"))
                    .unwrap();
                for _ in 0..200 {
                    match TestDb::scalar(&c, "SELECT count(*) FROM race_aggcon") {
                        Ok(count) if count == "10" => {
                            v.fetch_add(1, Ordering::Relaxed);
                        }
                        _ => {}
                    }
                    std::thread::yield_now();
                }
            });
        }
    });
    if successful_reads.load(Ordering::Relaxed) == 0 {
        return Err("no successful concurrent reads were observed".to_owned());
    }

    let final_sum = TestDb::scalar(&boot, "SELECT coalesce(sum(val), 0) FROM race_aggcon")
        .map_err(|e| e.clone())?
        .parse::<i64>()
        .map_err(|e| e.to_string())?;
    if final_sum != 1000 {
        return Err(format!(
            "final sum invariant violation: expected 1000, got {final_sum}"
        ));
    }

    let final_count = TestDb::scalar(&boot, "SELECT count(*) FROM race_aggcon")
        .map_err(|e| e.clone())?
        .parse::<usize>()
        .map_err(|e| e.to_string())?;
    if final_count != 10 {
        return Err(format!(
            "final row count mismatch: expected 10, got {final_count}"
        ));
    }
    Ok(1)
}

fn thundering_herd_point_lookup() -> Result<usize, String> {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .build()
            .map_err(|e| e.to_string())?,
    );
    let db = Database::new(Arc::clone(&engine));
    let boot = db
        .connect_anonymous("default", "herd-boot")
        .map_err(|e| e.to_string())?;
    TestDb::exec_ok(
        &boot,
        "CREATE TABLE race_herd (id INTEGER PRIMARY KEY, data TEXT)",
    );
    TestDb::exec_ok(&boot, "INSERT INTO race_herd VALUES (1, 'THE_VALUE')");
    let errors = Arc::new(AtomicUsize::new(0));
    let bad = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(std::sync::Barrier::new(16));
    std::thread::scope(|scope| {
        for tid in 0..16 {
            let er = Arc::clone(&engine);
            let e = Arc::clone(&errors);
            let b2 = Arc::clone(&bad);
            let br = Arc::clone(&barrier);
            scope.spawn(move || {
                let rdb = Database::new(er);
                let c = rdb
                    .connect_anonymous("default", format!("herd-{tid}"))
                    .unwrap();
                br.wait();
                for _ in 0..500 {
                    match TestDb::scalar(&c, "SELECT data FROM race_herd WHERE id = 1") {
                        Ok(v) if v == "THE_VALUE" => {}
                        Ok(_) => {
                            b2.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(_) => {
                            e.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            });
        }
    });
    let e = errors.load(Ordering::Relaxed);
    let b = bad.load(Ordering::Relaxed);
    if e > 0 || b > 0 {
        return Err(format!("{e} errors, {b} wrong values"));
    }
    Ok(1)
}

fn concurrent_cascading_rollback() -> Result<usize, String> {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .build()
            .map_err(|e| e.to_string())?,
    );
    let db = Database::new(Arc::clone(&engine));
    let boot = db
        .connect_anonymous("default", "cascade-boot")
        .map_err(|e| e.to_string())?;
    TestDb::exec_ok(
        &boot,
        "CREATE TABLE race_cascade (id INTEGER PRIMARY KEY, val INTEGER)",
    );
    TestDb::exec_ok(&boot, "INSERT INTO race_cascade VALUES (1, 42)");
    std::thread::scope(|scope| {
        for tid in 0..8 {
            let er = Arc::clone(&engine);
            scope.spawn(move || {
                let wdb = Database::new(er);
                let c = wdb
                    .connect_anonymous("default", format!("cascade-{tid}"))
                    .unwrap();
                for _ in 0..60 {
                    let _ = c.execute("BEGIN");
                    let _ = c.execute("UPDATE race_cascade SET val = 9999 WHERE id = 1");
                    let _ = c.execute("ROLLBACK");
                }
            });
        }
    });
    let val = TestDb::scalar(&boot, "SELECT val FROM race_cascade WHERE id = 1")
        .map_err(|e| e.clone())?;
    if val != "42" {
        return Err(format!("value changed: expected 42, got {val}"));
    }
    Ok(1)
}

fn concurrent_hot_row_toggle() -> Result<usize, String> {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .build()
            .map_err(|e| e.to_string())?,
    );
    let db = Database::new(Arc::clone(&engine));
    let boot = db
        .connect_anonymous("default", "toggle-boot")
        .map_err(|e| e.to_string())?;
    TestDb::exec_ok(
        &boot,
        "CREATE TABLE race_toggle (id INTEGER PRIMARY KEY, flag BOOLEAN, flips INTEGER)",
    );
    TestDb::exec_ok(&boot, "INSERT INTO race_toggle VALUES (1, false, 0)");
    let total_flips = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for tid in 0..6 {
            let er = Arc::clone(&engine);
            let f = Arc::clone(&total_flips);
            scope.spawn(move || {
                let wdb = Database::new(er);
                let c = wdb
                    .connect_anonymous("default", format!("toggle-{tid}"))
                    .unwrap();
                for _ in 0..100 {
                    if c.execute(
                        "UPDATE race_toggle SET flag = NOT flag, flips = flips + 1 WHERE id = 1",
                    )
                    .is_ok()
                    {
                        f.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    });
    let expected = total_flips.load(Ordering::Relaxed);
    let observed = TestDb::scalar(&boot, "SELECT flips FROM race_toggle WHERE id = 1")
        .map_err(|e| e.clone())?
        .parse::<usize>()
        .map_err(|e| e.to_string())?;
    let drift = observed.abs_diff(expected);
    if drift > 6 {
        return Err(format!(
            "flip drift={drift}: expected ~{expected}, got {observed}"
        ));
    }
    Ok(1)
}

fn concurrent_table_scan_under_mutation() -> Result<usize, String> {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .build()
            .map_err(|e| e.to_string())?,
    );
    let db = Database::new(Arc::clone(&engine));
    let boot = db
        .connect_anonymous("default", "scan-boot")
        .map_err(|e| e.to_string())?;
    TestDb::exec_ok(
        &boot,
        "CREATE TABLE race_scan (id INTEGER PRIMARY KEY, a INTEGER, b INTEGER)",
    );
    for i in 0..30_i64 {
        TestDb::exec_ok(
            &boot,
            &format!(
                "INSERT INTO race_scan VALUES ({i}, {}, {})",
                i * 3,
                100 - i * 3
            ),
        );
    }
    let violations = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for tid in 0..3_usize {
            let er = Arc::clone(&engine);
            scope.spawn(move || {
                let wdb = Database::new(er);
                let c = wdb
                    .connect_anonymous("default", format!("scanw-{tid}"))
                    .unwrap();
                for i in 0..100_usize {
                    let r = (tid * 10 + i) % 30;
                    let _ = c.execute(&format!("UPDATE race_scan SET a = b, b = a WHERE id = {r}"));
                }
            });
        }
        for tid in 0..3 {
            let er = Arc::clone(&engine);
            let v = Arc::clone(&violations);
            scope.spawn(move || {
                let rdb = Database::new(er);
                let c = rdb
                    .connect_anonymous("default", format!("scanr-{tid}"))
                    .unwrap();
                for _ in 0..100 {
                    if let Ok(res) = c.execute("SELECT id, a, b FROM race_scan") {
                        for r in &res {
                            if let aiondb_engine::StatementResult::Query { rows, .. } = r {
                                for row in rows {
                                    let a = get_int_val(&row.values, 1).unwrap_or(0);
                                    let b = get_int_val(&row.values, 2).unwrap_or(0);
                                    if a + b != 100 {
                                        v.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                            }
                        }
                    }
                }
            });
        }
    });
    if violations.load(Ordering::Relaxed) > 0 {
        return Err("row invariant a+b=100 violated".to_owned());
    }
    Ok(1)
}

fn concurrent_multi_table_join_integrity() -> Result<usize, String> {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .build()
            .map_err(|e| e.to_string())?,
    );
    let db = Database::new(Arc::clone(&engine));
    let boot = db
        .connect_anonymous("default", "mtjoin-boot")
        .map_err(|e| e.to_string())?;
    TestDb::exec_ok(
        &boot,
        "CREATE TABLE race_mt_a (id INTEGER PRIMARY KEY, val INTEGER)",
    );
    TestDb::exec_ok(
        &boot,
        "CREATE TABLE race_mt_b (id INTEGER PRIMARY KEY, a_id INTEGER, factor INTEGER)",
    );
    for i in 0..20 {
        TestDb::exec_ok(&boot, &format!("INSERT INTO race_mt_a VALUES ({i}, 10)"));
        TestDb::exec_ok(
            &boot,
            &format!("INSERT INTO race_mt_b VALUES ({i}, {i}, 2)"),
        );
    }
    std::thread::scope(|scope| {
        for tid in 0..6_usize {
            let er = Arc::clone(&engine);
            scope.spawn(move || {
                let wdb = Database::new(er);
                let c = wdb
                    .connect_anonymous("default", format!("mtw-{tid}"))
                    .unwrap();
                for i in 0..120_usize {
                    let r = (tid + i) % 20;
                    let _ = c.execute(&format!(
                        "UPDATE race_mt_a SET val = {} WHERE id = {r}",
                        10 + (i % 5)
                    ));
                    if i % 9 == 0 {
                        let _ = c.execute(&format!(
                            "UPDATE race_mt_b SET factor = {} WHERE id = {r}",
                            2 + (i % 3)
                        ));
                    }
                }
            });
        }
    });

    let join_count = TestDb::scalar(
        &boot,
        "SELECT count(*) FROM race_mt_a a JOIN race_mt_b b ON a.id = b.a_id",
    )
    .map_err(|e| e.clone())?
    .parse::<usize>()
    .map_err(|e| e.to_string())?;
    if join_count != 20 {
        return Err(format!("final JOIN count != 20 (got {join_count})"));
    }

    let distinct_a = TestDb::scalar(&boot, "SELECT count(DISTINCT id) FROM race_mt_a")
        .map_err(|e| e.clone())?
        .parse::<usize>()
        .map_err(|e| e.to_string())?;
    let distinct_b = TestDb::scalar(&boot, "SELECT count(DISTINCT id) FROM race_mt_b")
        .map_err(|e| e.clone())?
        .parse::<usize>()
        .map_err(|e| e.to_string())?;
    if distinct_a != 20 || distinct_b != 20 {
        return Err(format!(
            "final key cardinality mismatch: a={distinct_a}, b={distinct_b}"
        ));
    }

    Ok(1)
}

fn concurrent_batch_upsert_sum_invariant() -> Result<usize, String> {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .build()
            .map_err(|e| e.to_string())?,
    );
    let db = Database::new(Arc::clone(&engine));
    let boot = db
        .connect_anonymous("default", "batchup-boot")
        .map_err(|e| e.to_string())?;
    TestDb::exec_ok(
        &boot,
        "CREATE TABLE race_batchup (id INTEGER PRIMARY KEY, total INTEGER)",
    );
    let expected: Vec<Arc<AtomicI64>> = (0..20).map(|_| Arc::new(AtomicI64::new(0))).collect();
    let expected_clone: Vec<Arc<AtomicI64>> = expected.iter().map(Arc::clone).collect();
    std::thread::scope(|scope| {
        for tid in 0..6_usize {
            let er = Arc::clone(&engine);
            let exp = expected_clone.clone();
            scope.spawn(move || {
                let wdb = Database::new(er); let c = wdb.connect_anonymous("default", format!("batchup-{tid}")).unwrap();
                for i in 0..120_usize { let key = (tid * 3 + i) % 20; let delta = ((i * 7 + tid * 13) % 50) as i64;
                    let sql = format!("INSERT INTO race_batchup VALUES ({key}, {delta}) ON CONFLICT (id) DO UPDATE SET total = race_batchup.total + {delta}");
                    if c.execute(&sql).is_ok() { exp[key].fetch_add(delta, Ordering::Relaxed); }
                }
            });
        }
    });
    let mut mismatches = 0_usize;
    for (key, expected_value) in expected.iter().enumerate().take(20) {
        let ev = expected_value.load(Ordering::Relaxed);
        if let Ok(vs) = TestDb::scalar(
            &boot,
            &format!("SELECT total FROM race_batchup WHERE id = {key}"),
        ) {
            if let Ok(v) = vs.parse::<i64>() {
                if v != ev {
                    mismatches += 1;
                }
            }
        } else if ev != 0 {
            mismatches += 1;
        }
    }
    if mismatches > 0 {
        return Err(format!("{mismatches} upsert sum mismatches"));
    }
    Ok(1)
}

fn get_int_val(values: &[aiondb_engine::Value], idx: usize) -> Option<i64> {
    values.get(idx).and_then(|v| match v {
        aiondb_engine::Value::Int(i) => Some(i64::from(*i)),
        aiondb_engine::Value::BigInt(i) => Some(*i),
        _ => None,
    })
}
