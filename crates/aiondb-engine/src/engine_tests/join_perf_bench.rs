//! Micro-benchmark for the join paths.
//!
//! Run with `cargo test -p aiondb-engine --lib join_perf -- --ignored --nocapture`.

use std::time::Instant;

use super::*;

const CUSTOMERS: usize = 50;
const ORDERS: usize = 500;
const REPEATS: usize = 1;

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

#[test]
fn join_perf_paths() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");

    // Customers: distinct ids.
    engine
        .execute_sql(&s, "CREATE TABLE customers (id INT PRIMARY KEY, name TEXT)")
        .expect("create customers");
    let mut sql = String::from("INSERT INTO customers (id, name) VALUES ");
    for i in 0..CUSTOMERS {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("({i}, 'cust_{i}')"));
    }
    engine.execute_sql(&s, &sql).expect("seed customers");

    // Orders: customer_id ~ uniform over customers.
    engine
        .execute_sql(
            &s,
            "CREATE TABLE orders (id INT PRIMARY KEY, customer_id INT, amount INT)",
        )
        .expect("create orders");
    let mut sql = String::from("INSERT INTO orders (id, customer_id, amount) VALUES ");
    for i in 0..ORDERS {
        if i > 0 {
            sql.push_str(", ");
        }
        let cust = i % CUSTOMERS;
        let amount = (i * 7 + 3) % 1_000;
        sql.push_str(&format!("({i}, {cust}, {amount})"));
    }
    engine.execute_sql(&s, &sql).expect("seed orders");

    engine.execute_sql(&s, "ANALYZE customers").ok();
    engine.execute_sql(&s, "ANALYZE orders").ok();

    println!();
    println!("JOIN path bench  (orders={ORDERS}, customers={CUSTOMERS}, repeats={REPEATS})");

    println!();
    println!("  ----- shape: orders ⋈ customers  (small build, hash join) -----");
    timed("hash-inner-orders-customers", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT o.id, c.name, o.amount \
                 FROM orders o \
                 JOIN customers c ON o.customer_id = c.id",
            )
            .expect("hash inner");
    });

    println!();
    println!("  ----- shape: orders ⋈ customers WHERE amount > 500 (filter on join) -----");
    timed("hash-inner-with-filter", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT o.id, c.name \
                 FROM orders o \
                 JOIN customers c ON o.customer_id = c.id \
                 WHERE o.amount > 500",
            )
            .expect("hash filter");
    });

    println!();
    println!(
        "  ----- shape: customers LEFT JOIN orders ON id = customer_id  (large probe side) -----"
    );
    timed("hash-left-customers-orders", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT c.id, COUNT(o.id) \
                 FROM customers c \
                 LEFT JOIN orders o ON c.id = o.customer_id \
                 GROUP BY c.id",
            )
            .expect("hash left group");
    });

    println!();
    println!("  ----- shape: orders self-join on customer_id  (high duplication) -----");
    timed("hash-self-join", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT a.id, b.id FROM orders a JOIN orders b ON a.customer_id = b.customer_id LIMIT 10000",
            )
            .expect("self join");
    });

    println!();
    println!("  ----- shape: NL with index lookup (orders, customers via PK) -----");
    timed("nl-index-orders-customers", || {
        let _ = engine
            .execute_sql(
                &s,
                "SELECT o.id, c.name FROM orders o, customers c \
                 WHERE c.id = o.customer_id AND o.id < 100",
            )
            .expect("nl idx");
    });
}
