use super::*;

// ---------------------------------------------------------------------------
// Hybrid operator query 1: graph-first via graph_neighbors() + vector rerank
// ---------------------------------------------------------------------------

#[test]
fn hybrid_operator_graph_neighbors_returns_ids() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT doc_id \
         FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
         ORDER BY doc_id",
    );

    assert_bigint_column(&rows, 0, &[2, 3, 4]);
}

#[test]
fn hybrid_operator_graph_neighbors_accepts_limit_argument() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT doc_id \
         FROM graph_neighbors('related_doc', 1, 2) AS g(doc_id) \
         ORDER BY doc_id",
    );

    assert_bigint_column(&rows, 0, &[2, 3]);
}

#[test]
fn hybrid_operator_graph_neighbors_accepts_direction_and_limit_arguments() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT doc_id \
         FROM graph_neighbors('related_doc', 4, 'incoming', 2) AS g(doc_id) \
         ORDER BY doc_id",
    );

    assert_bigint_column(&rows, 0, &[1, 2]);
}

#[test]
fn graph_neighbors_metadata_cache_is_cleared_by_edge_ddl() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT doc_id FROM graph_neighbors('related_doc', 1) AS g(doc_id)",
    );
    assert_eq!(rows.len(), 3);

    engine
        .execute_sql(&session, "DROP EDGE LABEL related_doc")
        .expect("drop edge label");
    let error = engine
        .execute_sql(
            &session,
            "SELECT doc_id FROM graph_neighbors('related_doc', 1) AS g(doc_id)",
        )
        .expect_err("dropped edge label should not be served from cache");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
}

#[test]
fn cypher_one_hop_id_lookup_returns_neighbor_ids() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (d:doc {id: 1})-[:related_doc]->(m:doc) \
         RETURN m.id \
         ORDER BY m.id \
         LIMIT 10",
    );

    assert_bigint_column(&rows, 0, &[2, 3, 4]);
}

#[test]
fn prepare_describes_cypher_one_hop_id_lookup() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let desc = engine
        .prepare(
            &session,
            "cypher_one_hop".to_owned(),
            "MATCH (d:doc {id: 1})-[:related_doc]->(m:doc) RETURN m.id LIMIT 10".to_owned(),
        )
        .expect("prepare cypher");

    assert_eq!(desc.result_columns.len(), 1);
    assert_eq!(desc.result_columns[0].name, "m.id");
    assert_eq!(desc.result_columns[0].data_type, aiondb_core::DataType::Int);
}

#[test]
fn cypher_one_hop_fast_path_does_not_rewrite_other_returns() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (d:doc {id: 1})-[:related_doc]->(m:doc) \
         RETURN d.id \
         ORDER BY d.id \
         LIMIT 10",
    );

    assert_bigint_column(&rows, 0, &[1, 1, 1]);
}

#[test]
fn hybrid_operator_query_01_graph_first_then_vector_rerank() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "WITH neighbor_docs AS ( \
             SELECT doc_id \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
         ) \
         SELECT d.id, d.title, l2_distance(d.embedding, q.embedding) AS dist \
         FROM neighbor_docs nd \
         JOIN docs d ON d.id = nd.doc_id \
         JOIN query_vectors q ON q.id = 1 \
         ORDER BY dist, d.id \
         LIMIT 3",
    );

    assert_int_column(&rows, 0, &[2, 4, 3]);
    assert_text_column(
        &rows,
        1,
        &[
            "Pager Escalation Guide",
            "Database Recovery Runbook",
            "Postmortem Template",
        ],
    );
}

#[test]
fn explain_graph_first_then_vector_rerank_keeps_hash_child_and_nested_loop_parent() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN WITH neighbor_docs AS ( \
             SELECT doc_id \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
         ) \
         SELECT d.id, d.title, l2_distance(d.embedding, q.embedding) AS dist \
         FROM neighbor_docs nd \
         JOIN docs d ON d.id = nd.doc_id \
         JOIN query_vectors q ON q.id = 1 \
         ORDER BY dist, d.id \
         LIMIT 3",
    );

    assert!(
        lines.iter().any(|line| line.contains("Nested Loop")),
        "expected Nested Loop parent for graph-first rerank query, got {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("Hash Join")),
        "expected Hash Join child for graph neighbor expansion, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on graph_neighbors")),
        "expected explicit hybrid graph scan in EXPLAIN, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Filter: (q.id = 1)")),
        "expected right-only query vector predicate to stay visible on the scan side, got {lines:?}"
    );
}

// ---------------------------------------------------------------------------
// Hybrid operator query 2: vector-first via vector_top_k_ids() then graph-expand
// ---------------------------------------------------------------------------

#[test]
fn hybrid_operator_vector_top_k_ids_returns_seed_ids() {
    let (engine, session) = setup_workspace_context_dataset();
    engine
        .execute_sql(
            &session,
            "CREATE INDEX idx_notes_embedding ON notes USING hnsw (embedding)",
        )
        .expect("create notes hnsw index");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT note_id \
         FROM vector_top_k_ids( \
             'notes', \
             'embedding', \
             (SELECT embedding FROM query_vectors WHERE id = 1), \
             2 \
         ) AS seeds(note_id) \
         ORDER BY note_id",
    );

    assert_bigint_column(&rows, 0, &[1, 2]);
}

#[test]
fn hybrid_operator_query_02_vector_first_then_graph_expand() {
    let (engine, session) = setup_workspace_context_dataset();
    engine
        .execute_sql(
            &session,
            "CREATE INDEX idx_notes_embedding ON notes USING hnsw (embedding)",
        )
        .expect("create notes hnsw index");

    let rows = query_rows(
        &engine,
        &session,
        "WITH seed_notes AS ( \
             SELECT note_id \
             FROM vector_top_k_ids( \
                 'notes', \
                 'embedding', \
                 (SELECT embedding FROM query_vectors WHERE id = 1), \
                 2 \
             ) AS seeds(note_id) \
         ), \
         expanded_tasks AS ( \
             SELECT graph_neighbors('note_task', sn.note_id) AS task_id \
             FROM seed_notes sn \
         ) \
         SELECT t.id, t.title \
         FROM expanded_tasks et \
         JOIN tasks t ON t.id = et.task_id \
         WHERE t.status = 'open' \
         ORDER BY t.id",
    );

    assert_int_column(&rows, 0, &[10, 11]);
    assert_text_column(&rows, 1, &["Fix Startup Crash", "Improve Extension Logs"]);
}

// ---------------------------------------------------------------------------
// Hybrid operator query 3: relational filter -> graph -> top-k rerank
// ---------------------------------------------------------------------------

#[test]
fn hybrid_operator_query_03_relational_filter_then_graph_then_topk() {
    let (engine, session) = setup_workspace_context_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "WITH project_notes AS ( \
             SELECT id \
             FROM notes \
             WHERE project_id = 1 \
         ), \
         task_candidates AS ( \
             SELECT graph_neighbors('note_task', pn.id) AS task_id \
             FROM project_notes pn \
         ), \
         deduped_tasks AS ( \
             SELECT DISTINCT task_id \
             FROM task_candidates \
         ) \
         SELECT t.id, t.title, l2_distance(t.embedding, i.embedding) AS dist \
         FROM deduped_tasks dt \
         JOIN tasks t ON t.id = dt.task_id \
         JOIN query_vectors i ON i.id = 1 \
         WHERE t.status = 'open' \
         ORDER BY dist, t.id \
         LIMIT 2",
    );

    assert_int_column(&rows, 0, &[11, 10]);
    assert_text_column(&rows, 1, &["Improve Extension Logs", "Fix Startup Crash"]);
}

#[test]
fn hybrid_operator_query_04_grouped_vector_seed_join_preserves_non_join_columns() {
    let (engine, session) = setup_workspace_context_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT n.title, COUNT(*) \
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

    assert_text_column(&rows, 0, &["Crash Triage Checklist"]);
    assert_bigint_column(&rows, 1, &[1]);
}

#[test]
fn hybrid_operator_query_05_single_row_vector_seed_parent_join_preserves_vector_exprs() {
    let (engine, session) = setup_workspace_context_dataset();
    engine
        .execute_sql(
            &session,
            "CREATE INDEX idx_notes_embedding ON notes USING hnsw (embedding)",
        )
        .expect("create notes hnsw index");

    let rows = query_rows(
        &engine,
        &session,
        "WITH seed_notes AS ( \
             SELECT note_id \
             FROM vector_top_k_ids( \
                 'notes', \
                 'embedding', \
                 (SELECT embedding FROM query_vectors WHERE id = 1), \
                 8 \
             ) AS seeds(note_id) \
             ORDER BY note_id \
             LIMIT 1 \
         ) \
         SELECT n.id, l2_distance(n.embedding, i.embedding) AS dist \
         FROM seed_notes sn \
         JOIN notes n ON CAST(n.id AS BIGINT) = sn.note_id \
         JOIN query_vectors i ON i.id = 1 \
         ORDER BY dist, n.id",
    );

    assert_int_column(&rows, 0, &[1]);
}

#[test]
fn hybrid_operator_query_06_wrapped_graph_subquery_then_vector_rerank() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "WITH neighbor_docs AS ( \
             SELECT d.id, d.title, d.embedding \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             JOIN docs d ON d.id = g.doc_id \
         ) \
         SELECT nd.id, nd.title, l2_distance(nd.embedding, q.embedding) AS dist \
         FROM neighbor_docs nd \
         JOIN query_vectors q ON q.id = 1 \
         ORDER BY dist, nd.id \
         LIMIT 3",
    );

    assert_int_column(&rows, 0, &[2, 4, 3]);
    assert_text_column(
        &rows,
        1,
        &[
            "Pager Escalation Guide",
            "Database Recovery Runbook",
            "Postmortem Template",
        ],
    );
    assert_distance_approx(&rows[0], 2, 0.0);
}

#[test]
fn hybrid_operator_query_07_ordered_relational_wrapper_then_graph_join_pushes_filter() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT dsub.id, dsub.title \
         FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
         JOIN ( \
             SELECT id, title, kind \
             FROM docs \
             ORDER BY title \
         ) dsub ON dsub.id = g.doc_id \
         WHERE dsub.kind = 'runbook' \
         ORDER BY dsub.id",
    );

    assert_int_column(&rows, 0, &[4]);
    assert_text_column(&rows, 1, &["Database Recovery Runbook"]);
}

#[test]
fn explain_wrapped_graph_subquery_then_vector_rerank_keeps_hash_child_build_side() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN WITH neighbor_docs AS ( \
             SELECT d.id, d.title, d.embedding \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             JOIN docs d ON d.id = g.doc_id \
         ) \
         SELECT nd.id, nd.title, l2_distance(nd.embedding, q.embedding) AS dist \
         FROM neighbor_docs nd \
         JOIN query_vectors q ON q.id = 1 \
         ORDER BY dist, nd.id \
         LIMIT 3",
    );

    assert!(
        lines.iter().any(|line| line.contains("Nested Loop")),
        "expected Nested Loop parent for wrapped graph rerank query, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hash Join (rows=3200, build=right, build_rows=32)")),
        "expected wrapped graph child join to hash the hybrid side, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on graph_neighbors")),
        "expected explicit hybrid graph scan in EXPLAIN, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Filter: (q.id = 1)")),
        "expected right-only query vector predicate to stay visible on the scan side, got {lines:?}"
    );
}

#[test]
fn hybrid_operator_query_07_top_level_select_star_wrapper_over_graph_join() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT nd.id, nd.title \
         FROM ( \
             SELECT * \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             JOIN docs d ON d.id = g.doc_id \
         ) AS nd \
         ORDER BY nd.id \
         LIMIT 3",
    );

    assert_int_column(&rows, 0, &[2, 3, 4]);
    assert_text_column(
        &rows,
        1,
        &[
            "Pager Escalation Guide",
            "Postmortem Template",
            "Database Recovery Runbook",
        ],
    );
}

#[test]
fn explain_top_level_select_star_wrapper_over_graph_join_keeps_hash_child_build_side() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT nd.id, nd.title \
         FROM ( \
             SELECT * \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             JOIN docs d ON d.id = g.doc_id \
         ) AS nd \
         ORDER BY nd.id \
         LIMIT 3",
    );

    assert!(
        lines.iter().any(|line| line.contains("Sort")),
        "expected top-level sort for ordered select-star wrapper query, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hash Join (rows=3200, build=right, build_rows=32)")),
        "expected wrapped graph child join to hash the hybrid side, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on graph_neighbors")),
        "expected explicit hybrid graph scan in EXPLAIN, got {lines:?}"
    );
}

#[test]
fn hybrid_operator_query_08_top_level_ordered_limited_wrapper_over_graph_join() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT nd.id, nd.title \
         FROM ( \
             SELECT d.id, d.title \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             JOIN docs d ON d.id = g.doc_id \
             ORDER BY d.title \
             LIMIT 2 OFFSET 1 \
         ) AS nd \
         ORDER BY nd.id",
    );

    assert_int_column(&rows, 0, &[2, 3]);
    assert_text_column(&rows, 1, &["Pager Escalation Guide", "Postmortem Template"]);
}

#[test]
fn explain_top_level_ordered_limited_wrapper_over_graph_join_keeps_hash_child_build_side() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT nd.id, nd.title \
         FROM ( \
             SELECT d.id, d.title \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             JOIN docs d ON d.id = g.doc_id \
             ORDER BY d.title \
             LIMIT 2 OFFSET 1 \
         ) AS nd \
         ORDER BY nd.id",
    );

    assert!(
        lines.iter().any(|line| line.contains("Sort")),
        "expected sort nodes for ordered/limited root wrapper query, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hash Join (rows=2, build=right, build_rows=32)")),
        "expected ordered/limited root wrapper child join to hash the hybrid side, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on graph_neighbors")),
        "expected explicit hybrid graph scan in EXPLAIN, got {lines:?}"
    );
}

#[test]
fn hybrid_operator_query_08_top_level_distinct_wrapper_over_graph_join() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT DISTINCT nd.id \
         FROM ( \
             SELECT * \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             JOIN docs d ON d.id = g.doc_id \
         ) AS nd \
         ORDER BY nd.id",
    );

    assert_int_column(&rows, 0, &[2, 3, 4]);
}

#[test]
fn explain_top_level_distinct_wrapper_over_graph_join_keeps_hash_child_build_side() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT DISTINCT nd.id \
         FROM ( \
             SELECT * \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             JOIN docs d ON d.id = g.doc_id \
         ) AS nd \
         ORDER BY nd.id",
    );

    assert!(
        lines.iter().any(|line| line.contains("Unique")),
        "expected DISTINCT root wrapper to surface Unique in EXPLAIN, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Sort Key: <1 key(s)>")),
        "expected DISTINCT root wrapper to expose sort key in EXPLAIN, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hash Join (rows=3200, build=right, build_rows=32)")),
        "expected wrapped graph child join to hash the hybrid side under DISTINCT, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on graph_neighbors")),
        "expected explicit hybrid graph scan in DISTINCT EXPLAIN, got {lines:?}"
    );
}

#[test]
fn hybrid_operator_query_09_top_level_distinct_on_wrapper_over_graph_join() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT DISTINCT ON (nd.id) nd.id \
         FROM ( \
             SELECT * \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             JOIN docs d ON d.id = g.doc_id \
         ) AS nd \
         ORDER BY nd.id",
    );

    assert_int_column(&rows, 0, &[2, 3, 4]);
}

#[test]
fn hybrid_operator_query_09b_top_level_distinct_on_alias_order_by_over_lateral_graph() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "WITH seeds AS ( \
             SELECT 1 AS seed_id \
             UNION ALL \
             SELECT 2 AS seed_id \
         ) \
         SELECT DISTINCT ON (ranked.neighbor_id) ranked.neighbor_id AS neighbor_doc, \
                ranked.source_seed AS chosen_seed \
         FROM ( \
             SELECT n.doc_id AS neighbor_id, CAST(s.seed_id AS BIGINT) AS source_seed \
             FROM seeds s, \
                  LATERAL graph_neighbors('related_doc', s.seed_id) AS n(doc_id) \
         ) AS ranked \
         ORDER BY neighbor_doc, chosen_seed DESC",
    );

    assert_bigint_column(&rows, 0, &[2, 3, 4]);
    assert_bigint_column(&rows, 1, &[1, 1, 2]);
}

#[test]
fn hybrid_operator_query_09c_top_level_distinct_on_alias_order_by_over_hash_hybrid_join() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT DISTINCT ON (nd.doc_id) nd.doc_id AS neighbor_doc, \
                nd.concept_name AS winning_concept \
         FROM ( \
             SELECT g.doc_id, c.name AS concept_name \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             JOIN doc_mentions dm ON dm.source_id = g.doc_id \
             JOIN concepts c ON c.id = dm.target_id \
         ) AS nd \
         ORDER BY neighbor_doc, winning_concept DESC",
    );

    assert_bigint_column(&rows, 0, &[2, 3, 4]);
    assert_text_column(
        &rows,
        1,
        &["oncall", "incident-response", "incident-response"],
    );
}

#[test]
fn explain_top_level_distinct_on_wrapper_over_graph_join_keeps_hash_child_build_side() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT DISTINCT ON (nd.id) nd.id \
         FROM ( \
             SELECT * \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             JOIN docs d ON d.id = g.doc_id \
         ) AS nd \
         ORDER BY nd.id",
    );

    assert!(
        lines.iter().any(|line| line.contains("Unique")),
        "expected DISTINCT ON root wrapper to surface Unique in EXPLAIN, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Sort Key: <1 key(s)>")),
        "expected DISTINCT ON root wrapper to expose sort key in EXPLAIN, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hash Join (rows=3200, build=right, build_rows=32)")),
        "expected wrapped graph child join to hash the hybrid side under DISTINCT ON, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on graph_neighbors")),
        "expected explicit hybrid graph scan in DISTINCT ON EXPLAIN, got {lines:?}"
    );
}

#[test]
fn explain_top_level_distinct_on_alias_order_by_over_lateral_graph_keeps_nested_loop() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN WITH seeds AS ( \
             SELECT 1 AS seed_id \
             UNION ALL \
             SELECT 2 AS seed_id \
         ) \
         SELECT DISTINCT ON (ranked.neighbor_id) ranked.neighbor_id AS neighbor_doc, \
                ranked.source_seed AS chosen_seed \
         FROM ( \
             SELECT n.doc_id AS neighbor_id, CAST(s.seed_id AS BIGINT) AS source_seed \
             FROM seeds s, \
                  LATERAL graph_neighbors('related_doc', s.seed_id) AS n(doc_id) \
         ) AS ranked \
         ORDER BY neighbor_doc, chosen_seed DESC",
    );

    assert!(
        lines.iter().any(|line| line.contains("Unique")),
        "expected DISTINCT ON alias wrapper to surface Unique in EXPLAIN, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Sort Key: <2 key(s)>")),
        "expected DISTINCT ON alias wrapper to expose both sort keys in EXPLAIN, got {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("Nested Loop")),
        "expected lateral DISTINCT ON alias wrapper to keep Nested Loop, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on graph_neighbors")),
        "expected explicit hybrid graph scan in alias DISTINCT ON EXPLAIN, got {lines:?}"
    );
}

#[test]
fn hybrid_operator_top_level_filtered_wrapper_over_graph_join() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT nd.id, nd.title \
         FROM ( \
             SELECT * \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             JOIN docs d ON d.id = g.doc_id \
         ) AS nd \
         WHERE nd.kind = 'runbook' \
         ORDER BY nd.id",
    );

    assert_int_column(&rows, 0, &[4]);
    assert_text_column(&rows, 1, &["Database Recovery Runbook"]);
}

#[test]
fn explain_top_level_filtered_wrapper_over_graph_join_keeps_hash_child_build_side() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT nd.id, nd.title \
         FROM ( \
             SELECT * \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             JOIN docs d ON d.id = g.doc_id \
         ) AS nd \
         WHERE nd.kind = 'runbook' \
         ORDER BY nd.id",
    );

    assert!(
        lines
            .iter()
            .any(|line| line.contains("Filter:") && line.contains("kind = 'runbook'")),
        "expected filtered root wrapper to keep the kind predicate visible, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hash Join (rows=3200, build=right, build_rows=32)")),
        "expected filtered root wrapper child join to hash the hybrid side, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on graph_neighbors")),
        "expected explicit hybrid graph scan in filtered root EXPLAIN, got {lines:?}"
    );
}

#[test]
fn explain_labels_graph_neighbors_as_hybrid_function_scan() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT doc_id \
         FROM graph_neighbors('related_doc', 1) AS g(doc_id)",
    );

    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on graph_neighbors")),
        "expected hybrid graph EXPLAIN line, got {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("rows=32")),
        "expected graph hybrid row estimate in EXPLAIN, got {lines:?}"
    );
}

#[test]
fn explain_labels_limited_graph_neighbors_with_capped_row_estimate() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT doc_id \
         FROM graph_neighbors('related_doc', 1, 'outgoing', 2) AS g(doc_id)",
    );

    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on graph_neighbors")),
        "expected hybrid graph EXPLAIN line, got {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("rows=2")),
        "expected capped graph hybrid row estimate in EXPLAIN, got {lines:?}"
    );
}

#[test]
fn explain_labels_vector_top_k_ids_as_hybrid_function_scan() {
    let (engine, session) = setup_workspace_context_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT note_id \
         FROM vector_top_k_ids( \
             'notes', \
             'embedding', \
             (SELECT embedding FROM query_vectors WHERE id = 1), \
             2 \
         ) AS seeds(note_id)",
    );

    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on vector_top_k_ids")),
        "expected hybrid vector EXPLAIN line, got {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("rows=2")),
        "expected vector hybrid row estimate in EXPLAIN, got {lines:?}"
    );
}

#[test]
fn explain_filtered_graph_neighbors_shows_filtered_subquery_rows() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT doc_id \
         FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
         WHERE doc_id IN (2, 4)",
    );

    assert!(
        lines
            .iter()
            .any(|line| line.contains("Subquery Scan (rows=6.40)")),
        "expected filtered subquery row estimate in EXPLAIN, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on graph_neighbors")),
        "expected hybrid graph scan to remain explicit in EXPLAIN, got {lines:?}"
    );
}

#[test]
fn explain_pushes_relational_filter_through_hybrid_join_tree() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT d.id \
         FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
         JOIN docs d ON d.id = g.doc_id \
         WHERE d.id >= 2 \
         ORDER BY d.id",
    );

    assert!(
        lines.iter().any(|line| line.contains("Seq Scan on docs")),
        "expected docs scan in EXPLAIN, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Filter:") && line.contains("d.id >= 2")),
        "expected pushed relational filter on docs scan, got {lines:?}"
    );
}

#[test]
fn explain_pushes_relational_filter_through_ordered_wrapper_side_of_hybrid_join_tree() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT dsub.id \
         FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
         JOIN ( \
             SELECT id, title, kind \
             FROM docs \
             ORDER BY title \
         ) dsub ON dsub.id = g.doc_id \
         WHERE dsub.kind = 'runbook' \
         ORDER BY dsub.id",
    );

    assert!(
        lines.iter().any(|line| line.contains("Seq Scan on docs")),
        "expected docs scan in EXPLAIN, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Filter:") && line.contains("dsub.kind = 'runbook'")),
        "expected pushed relational filter on ordered docs scan, got {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Hybrid Function Scan on graph_neighbors")),
        "expected hybrid graph scan to remain explicit in EXPLAIN, got {lines:?}"
    );
}

#[test]
fn hybrid_function_scan_supports_lateral_outer_refs() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "WITH seeds AS ( \
             SELECT 1 AS seed_id \
             UNION ALL \
             SELECT 2 AS seed_id \
         ) \
         SELECT CAST(s.seed_id AS BIGINT), n.doc_id \
         FROM seeds s, \
              LATERAL graph_neighbors('related_doc', s.seed_id) AS n(doc_id) \
         ORDER BY s.seed_id, n.doc_id",
    );

    assert_bigint_column(&rows, 0, &[1, 1, 1, 2]);
    assert_bigint_column(&rows, 1, &[2, 3, 4, 4]);
}
