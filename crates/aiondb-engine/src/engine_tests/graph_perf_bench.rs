//! Micro-benchmark for the hottest graph scans from the surreal-suite.
//!
//! Run with:
//! `cargo test -p aiondb-engine --release --lib graph_perf -- --ignored --nocapture`

use std::time::Instant;

use super::*;

const ROWS: usize = 2_000;
const REPEATS: usize = 20;

fn timed<F: FnMut()>(label: &str, mut f: F) -> f64 {
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

fn build_people_seed_sql() -> String {
    let mut sql = String::from("INSERT INTO person_bench (id, number, category, words) VALUES ");
    for i in 1..=ROWS {
        if i > 1 {
            sql.push_str(", ");
        }
        let category = format!("c{}", i % 10);
        let words = format!("payload-{i}");
        sql.push_str(&format!("({i}, {}, '{category}', '{words}')", i % 100));
    }
    sql
}

fn build_knows_seed_sql() -> String {
    let mut values = Vec::with_capacity(ROWS + ROWS / 3);
    for i in 1..=ROWS {
        values.push(format!(
            "({i}, {i}, {}, {}, 'friend')",
            (i % ROWS) + 1,
            i % 50
        ));
        if i % 3 == 0 {
            values.push(format!(
                "({}, {i}, {}, {}, 'ref')",
                ROWS + i,
                ((i + 7) % ROWS) + 1,
                (i * 2) % 50
            ));
        }
    }
    format!(
        "INSERT INTO knows_bench (id, source_id, target_id, weight, relation) VALUES {}",
        values.join(", ")
    )
}

#[test]
#[ignore]
fn graph_perf_surreal_suite_shapes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &s,
            "CREATE TABLE person_bench (id INT PRIMARY KEY, number INT NOT NULL, category TEXT NOT NULL, words TEXT NOT NULL); \
             CREATE TABLE knows_bench (id INT PRIMARY KEY, source_id INT NOT NULL, target_id INT NOT NULL, weight INT NOT NULL, relation TEXT NOT NULL); \
             CREATE NODE LABEL PersonBench ON person_bench; \
             CREATE EDGE LABEL knows_bench_label ON knows_bench SOURCE PersonBench TARGET PersonBench",
        )
        .expect("create graph schema");
    engine
        .execute_sql(&s, &build_people_seed_sql())
        .expect("seed people");
    engine
        .execute_sql(&s, &build_knows_seed_sql())
        .expect("seed edges");

    println!();
    println!("graph surreal-suite bench  (rows={ROWS}, repeats={REPEATS})");

    let graph_sub_where = timed("graph_sub_where", || {
        let _ = engine
            .execute_sql(
                &s,
                "MATCH (a:PersonBench)-[:knows_bench_label]->(b:PersonBench) \
                 WHERE b.number > 20 \
                 RETURN b.id LIMIT 100",
            )
            .expect("graph_sub_where");
    });
    println!(
        "    graph_sub_where ops/s = {:>8.2}",
        1_000_000.0 / graph_sub_where.max(1.0)
    );

    let graph_multi_out_where = timed("graph_multi_out_where", || {
        let _ = engine
            .execute_sql(
                &s,
                "MATCH (a:PersonBench)-[:knows_bench_label]->(b:PersonBench), \
                       (a)-[:knows_bench_label]->(c:PersonBench) \
                 WHERE b.number > 20 \
                 RETURN b.id, c.id LIMIT 100",
            )
            .expect("graph_multi_out_where");
    });
    println!(
        "    graph_multi_out_where ops/s = {:>8.2}",
        1_000_000.0 / graph_multi_out_where.max(1.0)
    );
}
