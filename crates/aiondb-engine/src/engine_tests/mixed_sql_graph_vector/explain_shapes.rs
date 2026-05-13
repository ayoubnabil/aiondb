use super::*;

#[test]
fn explain_lateral_hybrid_outer_refs_stays_nested_loop() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN WITH seeds AS ( \
             SELECT 1 AS seed_id \
             UNION ALL \
             SELECT 2 AS seed_id \
         ) \
         SELECT CAST(s.seed_id AS BIGINT), n.doc_id \
         FROM seeds s, \
              LATERAL graph_neighbors('related_doc', s.seed_id) AS n(doc_id)",
    );

    assert!(
        lines.iter().any(|line| line.contains("Nested Loop")),
        "expected Nested Loop for lateral hybrid outer refs, got {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("Hash Join")),
        "did not expect Hash Join for lateral hybrid outer refs, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on graph_neighbors")),
        "expected hybrid graph scan to remain explicit in EXPLAIN, got {lines:?}"
    );
}

#[test]
fn explain_prefers_nested_loop_for_limited_vector_seed_source() {
    let (engine, session) = setup_workspace_context_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT n.id \
         FROM ( \
             SELECT note_id \
             FROM vector_top_k_ids( \
                 'notes', \
                 'embedding', \
                 (SELECT embedding FROM query_vectors WHERE id = 1), \
                 8 \
             ) AS seeds(note_id) \
             LIMIT 1 \
         ) seeds \
         JOIN notes n ON CAST(n.id AS BIGINT) = seeds.note_id",
    );

    assert!(
        lines.iter().any(|line| line.contains("Nested Loop")),
        "expected Nested Loop for a single-row hybrid seed source, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Nested Loop (rows=100)")),
        "expected nested-loop row estimate for a single-row hybrid seed source, got {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("Hash Join")),
        "did not expect Hash Join for a single-row hybrid seed source, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on vector_top_k_ids")),
        "expected hybrid vector scan to remain explicit in EXPLAIN, got {lines:?}"
    );
}

#[test]
fn explain_prefers_hash_join_for_graph_neighbor_expansion() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT d.id \
         FROM docs d \
         JOIN graph_neighbors('related_doc', 1) AS g(doc_id) \
           ON CAST(d.id AS BIGINT) = g.doc_id",
    );

    assert!(
        lines.iter().any(|line| line.contains("Hash Join")),
        "expected Hash Join for graph neighbor expansion, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hash Join (rows=3200, build=right, build_rows=32)")),
        "expected hash-join row and build-side estimates, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on graph_neighbors")),
        "expected hybrid graph scan to remain explicit in EXPLAIN, got {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("rows=32")),
        "expected graph hybrid row estimate in EXPLAIN, got {lines:?}"
    );
}

#[test]
fn explain_prefers_nested_loop_for_native_limited_graph_neighbor_source() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT d.id \
         FROM docs d \
         JOIN graph_neighbors('related_doc', 1, 1) AS g(doc_id) \
           ON CAST(d.id AS BIGINT) = g.doc_id",
    );

    assert!(
        lines.iter().any(|line| line.contains("Nested Loop")),
        "expected Nested Loop for a native single-row graph seed source, got {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("Hash Join")),
        "did not expect Hash Join for a native single-row graph seed source, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on graph_neighbors")),
        "expected hybrid graph scan to remain explicit in EXPLAIN, got {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("rows=1")),
        "expected single-row graph hybrid row estimate in EXPLAIN, got {lines:?}"
    );
}

#[test]
fn explain_grouped_single_row_vector_seed_uses_explicit_nested_loop_plan() {
    let (engine, session) = setup_workspace_context_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT n.title, COUNT(*) \
         FROM ( \
             SELECT note_id \
             FROM vector_top_k_ids( \
                 'notes', \
                 'embedding', \
                 (SELECT embedding FROM query_vectors WHERE id = 1), \
                 8 \
             ) AS seeds(note_id) \
             ORDER BY note_id \
             LIMIT 1 \
         ) seeds \
         JOIN notes n ON CAST(n.id AS BIGINT) = seeds.note_id \
         GROUP BY n.title \
         ORDER BY n.title",
    );

    assert!(
        !lines.iter().any(|line| line.contains("Hash Join")),
        "did not expect Hash Join for a grouped single-row hybrid seed source, got {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("Nested Loop")),
        "expected Nested Loop for the grouped single-row hybrid seed source, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Nested Loop (rows=100)")),
        "expected grouped nested-loop row estimate, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on vector_top_k_ids")),
        "expected grouped EXPLAIN to keep the hybrid vector scan explicit, got {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("GroupAggregate")),
        "expected grouped EXPLAIN to keep the aggregate node explicit, got {lines:?}"
    );
}

#[test]
fn explain_grouped_hybrid_query_with_having_surfaces_aggregate_rows() {
    let (engine, session) = setup_workspace_context_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT n.title, COUNT(*) \
         FROM ( \
             SELECT note_id \
             FROM vector_top_k_ids( \
                 'notes', \
                 'embedding', \
                 (SELECT embedding FROM query_vectors WHERE id = 1), \
                 8 \
             ) AS seeds(note_id) \
             ORDER BY note_id \
             LIMIT 4 \
         ) seeds \
         JOIN notes n ON CAST(n.id AS BIGINT) = seeds.note_id \
         GROUP BY n.title \
         HAVING COUNT(*) >= 1 \
         ORDER BY n.title \
         LIMIT 1 OFFSET 1",
    );

    assert!(
        lines
            .iter()
            .any(|line| line.contains("GroupAggregate (rows=1)")),
        "expected grouped hybrid EXPLAIN to include aggregate row estimate, got {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("Having:")),
        "expected grouped hybrid EXPLAIN to surface HAVING, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on vector_top_k_ids")),
        "expected grouped hybrid EXPLAIN to keep the vector seed visible, got {lines:?}"
    );
}

#[test]
fn explain_swaps_aggregate_wrapped_hybrid_subquery_to_hash_build_side() {
    let (engine, session) = setup_workspace_context_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT n.id \
         FROM notes n \
         JOIN ( \
             SELECT note_id \
             FROM vector_top_k_ids( \
                 'notes', \
                 'embedding', \
                 (SELECT embedding FROM query_vectors WHERE id = 1), \
                 200 \
             ) AS seeds(note_id) \
             GROUP BY note_id \
             HAVING COUNT(*) <> 0 \
         ) AS neighbors \
           ON CAST(n.id AS BIGINT) = neighbors.note_id",
    );

    let seq_scan_index = lines
        .iter()
        .position(|line| line.contains("Seq Scan on notes"))
        .expect("expected notes scan in EXPLAIN");
    let hash_index = lines
        .iter()
        .position(|line| line.contains("->  Hash"))
        .expect("expected hash build node in EXPLAIN");
    let aggregate_index = lines
        .iter()
        .position(|line| line.contains("GroupAggregate (rows=18)"))
        .expect("expected aggregate wrapper in EXPLAIN");
    let hybrid_index = lines
        .iter()
        .position(|line| line.contains("Hybrid Function Scan on vector_top_k_ids"))
        .expect("expected wrapped hybrid scan in EXPLAIN");

    assert!(
        lines.iter().any(|line| line.contains("Hash Join")),
        "expected Hash Join for aggregate-wrapped vector seed expansion, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hash Join (rows=1800, build=right, build_rows=18)")),
        "expected hash-join row and aggregate build-side estimates, got {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("Having:")),
        "expected aggregate wrapper to surface HAVING in EXPLAIN, got {lines:?}"
    );
    assert!(
        seq_scan_index < hash_index
            && hash_index < aggregate_index
            && aggregate_index < hybrid_index,
        "expected notes probe side before hashed aggregate hybrid source, got {lines:?}"
    );
}

#[test]
fn explain_swaps_wrapped_hybrid_subquery_to_hash_build_side() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT d.id \
         FROM docs d \
         JOIN ( \
             SELECT CAST(doc_id AS BIGINT) AS doc_id \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
         ) AS neighbors \
           ON CAST(d.id AS BIGINT) = neighbors.doc_id",
    );

    let seq_scan_index = lines
        .iter()
        .position(|line| line.contains("Seq Scan on docs"))
        .expect("expected docs scan in EXPLAIN");
    let hash_index = lines
        .iter()
        .position(|line| line.contains("->  Hash"))
        .expect("expected hash build node in EXPLAIN");
    let subquery_index = lines
        .iter()
        .position(|line| line.contains("Subquery Scan"))
        .expect("expected wrapped hybrid subquery in EXPLAIN");
    let hybrid_index = lines
        .iter()
        .position(|line| line.contains("Hybrid Function Scan on graph_neighbors"))
        .expect("expected wrapped hybrid scan in EXPLAIN");

    assert!(
        lines.iter().any(|line| line.contains("Hash Join")),
        "expected Hash Join for wrapped graph neighbor expansion, got {lines:?}"
    );
    assert!(
        seq_scan_index < hash_index && hash_index < subquery_index && subquery_index < hybrid_index,
        "expected docs probe side before hashed wrapped hybrid source, got {lines:?}"
    );
}

#[test]
fn explain_swaps_distinct_wrapped_hybrid_subquery_to_hash_build_side() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT d.id \
         FROM docs d \
         JOIN ( \
             SELECT DISTINCT CAST(doc_id AS BIGINT) AS doc_id \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
         ) AS neighbors \
           ON CAST(d.id AS BIGINT) = neighbors.doc_id",
    );

    let seq_scan_index = lines
        .iter()
        .position(|line| line.contains("Seq Scan on docs"))
        .expect("expected docs scan in EXPLAIN");
    let hash_index = lines
        .iter()
        .position(|line| line.contains("->  Hash"))
        .expect("expected hash build node in EXPLAIN");
    let unique_index = lines
        .iter()
        .position(|line| line.contains("Unique"))
        .expect("expected DISTINCT wrapper in EXPLAIN");
    let hybrid_index = lines
        .iter()
        .position(|line| line.contains("Hybrid Function Scan on graph_neighbors"))
        .expect("expected wrapped hybrid scan in EXPLAIN");

    assert!(
        lines.iter().any(|line| line.contains("Hash Join")),
        "expected Hash Join for DISTINCT-wrapped graph neighbor expansion, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hash Join (rows=1600, build=right, build_rows=16)")),
        "expected hash-join row and DISTINCT build-side estimates, got {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("Unique (rows=16)")),
        "expected DISTINCT wrapper row estimate in EXPLAIN, got {lines:?}"
    );
    assert!(
        seq_scan_index < hash_index && hash_index < unique_index && unique_index < hybrid_index,
        "expected docs probe side before hashed DISTINCT hybrid source, got {lines:?}"
    );
}

#[test]
fn explain_swaps_distinct_on_wrapped_hybrid_subquery_to_hash_build_side() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT d.id \
         FROM docs d \
         JOIN ( \
             SELECT DISTINCT ON (doc_id) CAST(doc_id AS BIGINT) AS doc_id \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             ORDER BY doc_id \
         ) AS neighbors \
           ON CAST(d.id AS BIGINT) = neighbors.doc_id",
    );

    let seq_scan_index = lines
        .iter()
        .position(|line| line.contains("Seq Scan on docs"))
        .expect("expected docs scan in EXPLAIN");
    let hash_index = lines
        .iter()
        .position(|line| line.contains("->  Hash"))
        .expect("expected hash build node in EXPLAIN");
    let unique_index = lines
        .iter()
        .position(|line| line.contains("Unique"))
        .expect("expected DISTINCT ON wrapper in EXPLAIN");
    let hybrid_index = lines
        .iter()
        .position(|line| line.contains("Hybrid Function Scan on graph_neighbors"))
        .expect("expected wrapped hybrid scan in EXPLAIN");

    assert!(
        lines.iter().any(|line| line.contains("Hash Join")),
        "expected Hash Join for DISTINCT ON-wrapped graph neighbor expansion, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hash Join (rows=1600, build=right, build_rows=16)")),
        "expected hash-join row and DISTINCT ON build-side estimates, got {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("Unique (rows=16)")),
        "expected DISTINCT ON wrapper row estimate in EXPLAIN, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Sort Key: <1 key(s)>")),
        "expected DISTINCT ON sort key in EXPLAIN, got {lines:?}"
    );
    assert!(
        seq_scan_index < hash_index && hash_index < unique_index && unique_index < hybrid_index,
        "expected docs probe side before hashed DISTINCT ON hybrid source, got {lines:?}"
    );
}

#[test]
fn explain_swaps_ordered_limited_wrapped_hybrid_subquery_to_hash_build_side() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT d.id \
         FROM docs d \
         JOIN ( \
             SELECT CAST(doc_id AS BIGINT) AS doc_id \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             ORDER BY doc_id \
             LIMIT 5 OFFSET 3 \
         ) AS neighbors \
           ON CAST(d.id AS BIGINT) = neighbors.doc_id",
    );

    let seq_scan_index = lines
        .iter()
        .position(|line| line.contains("Seq Scan on docs"))
        .expect("expected docs scan in EXPLAIN");
    let hash_index = lines
        .iter()
        .position(|line| line.contains("->  Hash"))
        .expect("expected hash build node in EXPLAIN");
    let sort_index = lines
        .iter()
        .position(|line| line.contains("Sort (rows=5)"))
        .expect("expected ORDER BY/LIMIT/OFFSET wrapper in EXPLAIN");
    let hybrid_index = lines
        .iter()
        .position(|line| line.contains("Hybrid Function Scan on graph_neighbors"))
        .expect("expected wrapped hybrid scan in EXPLAIN");

    assert!(
        lines.iter().any(|line| line.contains("Hash Join")),
        "expected Hash Join for ORDER BY/LIMIT/OFFSET-wrapped graph neighbor expansion, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hash Join (rows=500, build=right, build_rows=5)")),
        "expected hash-join row and limited build-side estimates, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Sort Key: <1 key(s)>")),
        "expected ORDER BY wrapper to expose sort key in EXPLAIN, got {lines:?}"
    );
    assert!(
        seq_scan_index < hash_index && hash_index < sort_index && sort_index < hybrid_index,
        "expected docs probe side before hashed ORDER BY/LIMIT/OFFSET hybrid source, got {lines:?}"
    );
}
