#![allow(clippy::pedantic)]

use std::time::Instant;

use super::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn perf_budget_secs(base: f64) -> f64 {
    const CI_DEFAULT_MULTIPLIER: f64 = 2.0;
    const LOCAL_DEFAULT_MULTIPLIER: f64 = 1.0;
    // Keep the override bounded so a bad env value does not
    // accidentally hide real perf regressions.
    const MIN_ALLOWED_MULTIPLIER: f64 = 0.25;
    const MAX_ALLOWED_MULTIPLIER: f64 = 4.0;

    // `cargo test` runs these checks in the debug test profile, where local
    // full-suite runs can be as contended as CI even without a `CI=1` env.
    let default_multiplier = if std::env::var_os("CI").is_some() || cfg!(debug_assertions) {
        CI_DEFAULT_MULTIPLIER
    } else {
        LOCAL_DEFAULT_MULTIPLIER
    };

    let multiplier = std::env::var("AIONDB_PERF_BUDGET_MULTIPLIER")
        .ok()
        .and_then(|raw| raw.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .map_or(default_multiplier, |v| {
            v.clamp(MIN_ALLOWED_MULTIPLIER, MAX_ALLOWED_MULTIPLIER)
        });

    base * multiplier
}

// ---------------------------------------------------------------------------
// 1. Insert throughput: 400 rows within budget
// ---------------------------------------------------------------------------

#[test]
fn perf_budget_insert_throughput() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&session, "CREATE TABLE perf_ins (id INT, val TEXT)")
        .expect("create table");

    const N: usize = 400;
    // Keep this as a broad regression guard, not a contention-sensitive
    // wall-clock race against the rest of the full crate test suite.
    const BUDGET_SECS: f64 = 8.0;

    let start = Instant::now();
    for i in 0..N {
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO perf_ins VALUES ({i}, 'row_{i}')"),
            )
            .expect("insert");
    }
    let elapsed = start.elapsed().as_secs_f64();
    let budget_secs = perf_budget_secs(BUDGET_SECS);

    let count = query_count(&engine, &session, "SELECT COUNT(*) FROM perf_ins");
    assert_eq!(count, N as i64, "all rows should be inserted");

    assert!(
        elapsed < budget_secs,
        "insert throughput: {N} rows took {elapsed:.2}s, budget is {budget_secs:.2}s (base {BUDGET_SECS}s)",
    );
}

// ---------------------------------------------------------------------------
// 2. Sequential scan throughput: 2000 rows, full COUNT(*)
// ---------------------------------------------------------------------------

#[test]
fn perf_budget_sequential_scan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&session, "CREATE TABLE perf_scan (id INT, payload TEXT)")
        .expect("create table");

    const N: usize = 2_000;
    for i in 0..N {
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO perf_scan VALUES ({i}, 'data_{i}')"),
            )
            .expect("insert");
    }

    const BUDGET_SECS: f64 = 5.0;
    const ITERATIONS: usize = 3;

    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let count = query_count(&engine, &session, "SELECT COUNT(*) FROM perf_scan");
        assert_eq!(count, N as i64, "count mismatch");
    }
    let elapsed = start.elapsed().as_secs_f64();
    let budget_secs = perf_budget_secs(BUDGET_SECS);

    assert!(
        elapsed < budget_secs,
        "sequential scan: {ITERATIONS} full scans of {N} rows took \
         {elapsed:.2}s, budget is {budget_secs:.2}s (base {BUDGET_SECS}s)",
    );
}

// ---------------------------------------------------------------------------
// 3. Index scan throughput: 1200 rows, 40 point lookups
// ---------------------------------------------------------------------------

#[test]
fn perf_budget_index_scan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE perf_idx (id INT PRIMARY KEY, val TEXT)",
        )
        .expect("create table");

    const N: usize = 1_200;
    for i in 0..N {
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO perf_idx VALUES ({i}, 'value_{i}')"),
            )
            .expect("insert");
    }

    // Create an explicit index on id (in addition to the PK constraint).
    let _ = engine.execute_sql(&session, "CREATE INDEX perf_idx_id ON perf_idx (id)");

    const LOOKUPS: usize = 40;
    const BUDGET_SECS: f64 = 5.0;

    let start = Instant::now();
    for i in 0..LOOKUPS {
        let target = (i * 50) % N; // spread lookups across the range
        let rows = query_rows(
            &engine,
            &session,
            &format!("SELECT id, val FROM perf_idx WHERE id = {target}"),
        );
        assert_eq!(rows.len(), 1, "point lookup should find exactly 1 row");
    }
    let elapsed = start.elapsed().as_secs_f64();
    let budget_secs = perf_budget_secs(BUDGET_SECS);

    assert!(
        elapsed < budget_secs,
        "index scan: {LOOKUPS} point lookups on {N} rows took \
         {elapsed:.2}s, budget is {budget_secs:.2}s (base {BUDGET_SECS}s)",
    );
}

// ---------------------------------------------------------------------------
// 4. Transaction cycle throughput: 120 BEGIN/INSERT/COMMIT cycles
// ---------------------------------------------------------------------------

#[test]
fn perf_budget_transaction_cycles() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&session, "CREATE TABLE perf_txn (id INT, val TEXT)")
        .expect("create table");

    const CYCLES: usize = 120;
    const BUDGET_SECS: f64 = 5.0;

    let start = Instant::now();
    for c in 0..CYCLES {
        engine.execute_sql(&session, "BEGIN").expect("begin");
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO perf_txn VALUES ({c}, 'txn_{c}')"),
            )
            .expect("insert");
        engine.execute_sql(&session, "COMMIT").expect("commit");
    }
    let elapsed = start.elapsed().as_secs_f64();
    let budget_secs = perf_budget_secs(BUDGET_SECS);

    let count = query_count(&engine, &session, "SELECT COUNT(*) FROM perf_txn");
    assert_eq!(count, CYCLES as i64, "all transaction cycles should commit");

    assert!(
        elapsed < budget_secs,
        "transaction cycles: {CYCLES} BEGIN/INSERT/COMMIT took \
         {elapsed:.2}s, budget is {budget_secs:.2}s (base {BUDGET_SECS}s)",
    );
}

// ---------------------------------------------------------------------------
// 5. Join throughput: two tables, measure join query
// ---------------------------------------------------------------------------

#[test]
fn perf_budget_join_throughput() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE perf_orders (id INT PRIMARY KEY, customer_id INT, amount INT)",
        )
        .expect("create orders");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE perf_customers (id INT PRIMARY KEY, name TEXT)",
        )
        .expect("create customers");

    const CUSTOMERS: usize = 100;
    const ORDERS: usize = 500;

    for i in 0..CUSTOMERS {
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO perf_customers VALUES ({i}, 'customer_{i}')"),
            )
            .expect("insert customer");
    }
    for i in 0..ORDERS {
        let cust = i % CUSTOMERS;
        let amount = (i * 7 + 3) % 1000;
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO perf_orders VALUES ({i}, {cust}, {amount})"),
            )
            .expect("insert order");
    }

    const JOIN_ITERATIONS: usize = 5;
    const BUDGET_SECS: f64 = 10.0;

    let start = Instant::now();
    for _ in 0..JOIN_ITERATIONS {
        let rows = query_rows(
            &engine,
            &session,
            "SELECT o.id, c.name, o.amount \
             FROM perf_orders o \
             JOIN perf_customers c ON o.customer_id = c.id",
        );
        // Every order should join with exactly one customer.
        assert_eq!(rows.len(), ORDERS, "join should return one row per order");
    }
    let elapsed = start.elapsed().as_secs_f64();
    let budget_secs = perf_budget_secs(BUDGET_SECS);

    assert!(
        elapsed < budget_secs,
        "join throughput: {JOIN_ITERATIONS} join queries on {ORDERS} orders \
         x {CUSTOMERS} customers took {elapsed:.2}s, budget is {budget_secs:.2}s (base {BUDGET_SECS}s)",
    );
}
