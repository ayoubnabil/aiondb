//! Micro-benchmark for the hottest graph scans from the surreal-suite.
//!
//! Run with:
//! `cargo test -p aiondb-engine --release --lib graph_perf -- --ignored --nocapture`

use std::time::Instant;

use super::*;

const ROWS: usize = 250;
const REPEATS: usize = 2;

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

fn timed_with_prepare<P: FnMut(), F: FnMut()>(label: &str, mut prepare: P, mut f: F) -> f64 {
    prepare();
    f();
    prepare();
    f();
    let mut elapsed_micros = 0u128;
    for _ in 0..REPEATS {
        prepare();
        let start = Instant::now();
        f();
        elapsed_micros = elapsed_micros.saturating_add(start.elapsed().as_micros());
    }
    let per_iter_us = elapsed_micros as f64 / REPEATS as f64;
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
fn graph_perf_surreal_suite_shapes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &s,
            "CREATE TABLE person_bench (id INT PRIMARY KEY, number INT NOT NULL, category TEXT NOT NULL, words TEXT NOT NULL); \
             CREATE TABLE graph_aux_bench (id INT PRIMARY KEY, touched INT NOT NULL); \
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

    let graph_neighbors_lateral_filter = timed("graph_neighbors_lateral_filter", || {
        let _ = engine
            .execute_sql(
                &s,
                "WITH seeds AS ( \
                     SELECT id AS seed_id \
                     FROM person_bench \
                     WHERE id BETWEEN 1 AND 32 \
                 ) \
                 SELECT p.id \
                 FROM seeds seed \
                 CROSS JOIN LATERAL graph_neighbors('knows_bench_label', seed.seed_id) AS g(neighbor_id) \
                 JOIN person_bench p ON p.id = g.neighbor_id \
                 WHERE p.number > 20 \
                 LIMIT 100",
            )
            .expect("graph_neighbors_lateral_filter");
    });
    println!(
        "    graph_neighbors_lateral_filter ops/s = {:>8.2}",
        1_000_000.0 / graph_neighbors_lateral_filter.max(1.0)
    );

    let graph_neighbors_two_hop_lateral = timed("graph_neighbors_two_hop_lateral", || {
        let _ = engine
            .execute_sql(
                &s,
                "WITH seeds AS ( \
                     SELECT id AS seed_id \
                     FROM person_bench \
                     WHERE id BETWEEN 1 AND 24 \
                 ) \
                 SELECT c.neighbor_id \
                 FROM seeds seed \
                 CROSS JOIN LATERAL graph_neighbors('knows_bench_label', seed.seed_id, 4) AS b(neighbor_id) \
                 CROSS JOIN LATERAL graph_neighbors('knows_bench_label', b.neighbor_id, 4) AS c(neighbor_id) \
                 JOIN person_bench p ON p.id = c.neighbor_id \
                 WHERE p.number > 20 \
                 LIMIT 100",
            )
            .expect("graph_neighbors_two_hop_lateral");
    });
    println!(
        "    graph_neighbors_two_hop_lateral ops/s = {:>8.2}",
        1_000_000.0 / graph_neighbors_two_hop_lateral.max(1.0)
    );

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

    let graph_match_rel_filter_native_endpoints =
        timed("graph_match_rel_filter_native_endpoints", || {
            let _ = engine
                .execute_sql(
                    &s,
                    "MATCH (a:PersonBench)-[:knows_bench_label {weight: 10}]->(b:PersonBench) \
                     RETURN b.id LIMIT 100",
                )
                .expect("graph_match_rel_filter_native_endpoints");
        });
    println!(
        "    graph_match_rel_filter_native_endpoints ops/s = {:>8.2}",
        1_000_000.0 / graph_match_rel_filter_native_endpoints.max(1.0)
    );

    let graph_match_rel_filter_bound_edge = timed("graph_match_rel_filter_bound_edge", || {
        let _ = engine
            .execute_sql(
                &s,
                "MATCH (a:PersonBench)-[r:knows_bench_label {weight: 10}]->(b:PersonBench) \
                 RETURN b.id LIMIT 100",
            )
            .expect("graph_match_rel_filter_bound_edge");
    });
    println!(
        "    graph_match_rel_filter_bound_edge ops/s = {:>8.2}",
        1_000_000.0 / graph_match_rel_filter_bound_edge.max(1.0)
    );

    let graph_page_rank_warm = timed("graph_page_rank_warm", || {
        let _ = engine
            .execute_sql(
                &s,
                "CALL graph.pageRank() \
                 YIELD nodeId, score \
                 RETURN nodeId, score \
                 ORDER BY score DESC, nodeId \
                 LIMIT 25",
            )
            .expect("graph_page_rank_warm");
    });
    println!(
        "    graph_page_rank_warm ops/s = {:>8.2}",
        1_000_000.0 / graph_page_rank_warm.max(1.0)
    );

    let mut invalidation_row_id = 1usize;
    let graph_page_rank_cold_after_write = timed_with_prepare(
        "graph_page_rank_cold_after_write",
        || {
            let sql = format!(
                "INSERT INTO graph_aux_bench (id, touched) VALUES ({0}, {0})",
                invalidation_row_id
            );
            invalidation_row_id = invalidation_row_id.saturating_add(1);
            let _ = engine
                .execute_sql(&s, &sql)
                .expect("invalidate graph pageRank cache generation");
        },
        || {
            let _ = engine
                .execute_sql(
                    &s,
                    "CALL graph.pageRank() \
                     YIELD nodeId, score \
                     RETURN nodeId, score \
                     ORDER BY score DESC, nodeId \
                     LIMIT 25",
                )
                .expect("graph_page_rank_cold_after_write");
        },
    );
    println!(
        "    graph_page_rank_cold_after_write ops/s = {:>8.2}",
        1_000_000.0 / graph_page_rank_cold_after_write.max(1.0)
    );

    let graph_dijkstra_weighted_warm = timed("graph_dijkstra_weighted_warm", || {
        let _ = engine
            .execute_sql(
                &s,
                "CALL graph.dijkstra(1, 120, 8, 'weight') \
                 YIELD sourceNodeId, targetNodeId, totalCost, path \
                 RETURN sourceNodeId, targetNodeId, totalCost, path",
            )
            .expect("graph_dijkstra_weighted_warm");
    });
    println!(
        "    graph_dijkstra_weighted_warm ops/s = {:>8.2}",
        1_000_000.0 / graph_dijkstra_weighted_warm.max(1.0)
    );

    let graph_dijkstra_weighted_cold_after_write = timed_with_prepare(
        "graph_dijkstra_weighted_cold_after_write",
        || {
            let sql = format!(
                "INSERT INTO graph_aux_bench (id, touched) VALUES ({0}, {0})",
                invalidation_row_id
            );
            invalidation_row_id = invalidation_row_id.saturating_add(1);
            let _ = engine
                .execute_sql(&s, &sql)
                .expect("invalidate weighted graph cache generation");
        },
        || {
            let _ = engine
                .execute_sql(
                    &s,
                    "CALL graph.dijkstra(1, 120, 8, 'weight') \
                     YIELD sourceNodeId, targetNodeId, totalCost, path \
                     RETURN sourceNodeId, targetNodeId, totalCost, path",
                )
                .expect("graph_dijkstra_weighted_cold_after_write");
        },
    );
    println!(
        "    graph_dijkstra_weighted_cold_after_write ops/s = {:>8.2}",
        1_000_000.0 / graph_dijkstra_weighted_cold_after_write.max(1.0)
    );
}
