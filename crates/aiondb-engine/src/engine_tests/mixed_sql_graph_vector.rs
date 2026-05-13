use aiondb_core::Value;

use super::*;

#[path = "mixed_sql_graph_vector/hybrid_operator_queries.rs"]
mod hybrid_operator_queries;

const KNOWLEDGE_GRAPH_DATASET: &str =
    include_str!("../../../../testing/sql/showcase/knowledge_graph_semantic_search.sql");
const CATALOG_DATASET: &str =
    include_str!("../../../../testing/sql/showcase/catalog_recommendations.sql");
const WORKSPACE_CONTEXT_DATASET: &str =
    include_str!("../../../../testing/sql/showcase/private_workspace_context.sql");

fn load_dataset(engine: &Engine, session: &SessionHandle, dataset_sql: &str) {
    engine
        .execute_sql(session, dataset_sql)
        .expect("load dataset");
}

fn setup_knowledge_graph_dataset() -> (Engine, SessionHandle) {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    load_dataset(&engine, &session, KNOWLEDGE_GRAPH_DATASET);
    (engine, session)
}

fn setup_catalog_dataset() -> (Engine, SessionHandle) {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    load_dataset(&engine, &session, CATALOG_DATASET);
    (engine, session)
}

fn setup_workspace_context_dataset() -> (Engine, SessionHandle) {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    load_dataset(&engine, &session, WORKSPACE_CONTEXT_DATASET);
    (engine, session)
}

fn setup_tenant_graph_vector_dataset() -> (Engine, SessionHandle) {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT PRIMARY KEY, name TEXT, role TEXT, tenant_id INT); \
             CREATE TABLE docs (id INT PRIMARY KEY, title TEXT, content TEXT, embedding VECTOR(3), tenant_id INT); \
             CREATE TABLE wrote (source_id INT, target_id INT); \
             CREATE TABLE cites (source_id INT, target_id INT); \
             CREATE NODE LABEL User ON users; \
             CREATE NODE LABEL Document ON docs; \
             CREATE EDGE LABEL WROTE ON wrote SOURCE User TARGET Document; \
             CREATE EDGE LABEL CITES ON cites SOURCE Document TARGET Document; \
             INSERT INTO users VALUES (1,'Alice','admin',100),(2,'Bob','user',100),(3,'Charlie','user',200); \
             INSERT INTO docs VALUES \
                 (10,'AionDB Guide','x','[0.1, 0.9, 0.2]',100), \
                 (11,'Postgres vs AionDB','x','[0.2, 0.8, 0.1]',100), \
                 (12,'Secret Project X','x','[0.9, 0.1, 0.1]',200), \
                 (13,'Graph Vector DBs','x','[0.15, 0.85, 0.2]',100); \
             INSERT INTO wrote VALUES (1,10),(2,11),(3,12),(1,13); \
             INSERT INTO cites VALUES (11,10),(13,10);",
        )
        .expect("setup tenant graph-vector dataset");
    (engine, session)
}

fn benchmark_words(i: usize) -> String {
    let base = [
        "hello", "world", "foo", "bar", "database", "query", "index", "graph",
    ];
    (0..16)
        .map(|j| base[(i + j) % base.len()])
        .collect::<Vec<_>>()
        .join(" ")
}

fn benchmark_vector(i: usize) -> String {
    let a = (i % 97) as f64 / 97.0;
    let b = ((i * 7) % 89) as f64 / 89.0;
    let c = ((i * 13) % 83) as f64 / 83.0;
    format!("[{a:.6},{b:.6},{c:.6}]")
}

fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn setup_surreal_suite_complex_dataset(rows: usize, with_hnsw: bool) -> (Engine, SessionHandle) {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE record ( \
                 id INT PRIMARY KEY, \
                 number INT NOT NULL, \
                 number2 INT NOT NULL, \
                 category TEXT NOT NULL, \
                 words TEXT NOT NULL, \
                 payload TEXT NOT NULL, \
                 tags TEXT, \
                 embedding VECTOR(3) \
             ); \
             CREATE TABLE person ( \
                 id INT PRIMARY KEY, \
                 number INT NOT NULL, \
                 category TEXT NOT NULL, \
                 words TEXT NOT NULL, \
                 embedding VECTOR(3) \
             ); \
             CREATE TABLE knows ( \
                 id INT PRIMARY KEY, \
                 source_id INT NOT NULL, \
                 target_id INT NOT NULL, \
                 weight INT NOT NULL, \
                 relation TEXT NOT NULL \
             );",
        )
        .expect("create surreal-suite complex tables");

    let mut record_values = Vec::with_capacity(rows);
    let mut person_values = Vec::with_capacity(rows);
    let mut edge_values = Vec::with_capacity(rows + rows / 3);
    for i in 1..=rows {
        let category = format!("c{}", i % 10);
        let words = benchmark_words(i);
        let payload = format!("payload-{i}-{}", "x".repeat(96));
        let tags = format!("t{},t{}", i % 5, (i + 1) % 5);
        record_values.push(format!(
            "({i},{},{},{},{},{},{},'{}')",
            i % 100,
            (i * 3) % 100,
            sql_string(&category),
            sql_string(&words),
            sql_string(&payload),
            sql_string(&tags),
            benchmark_vector(i),
        ));
        person_values.push(format!(
            "({i},{},{},{},'{}')",
            i % 100,
            sql_string(&category),
            sql_string(&words),
            benchmark_vector(i),
        ));
        edge_values.push(format!(
            "({i},{i},{},{},{})",
            (i % rows) + 1,
            i % 50,
            sql_string(if i % 2 == 0 { "ref" } else { "friend" }),
        ));
        if i % 3 == 0 {
            edge_values.push(format!(
                "({},{i},{},{},{})",
                rows + i,
                ((i + 7) % rows) + 1,
                (i * 2) % 50,
                sql_string("ref"),
            ));
        }
    }

    for chunk in record_values.chunks(250) {
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO record VALUES {}", chunk.join(",")),
            )
            .expect("insert record chunk");
    }
    for chunk in person_values.chunks(250) {
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO person VALUES {}", chunk.join(",")),
            )
            .expect("insert person chunk");
    }
    for chunk in edge_values.chunks(250) {
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO knows VALUES {}", chunk.join(",")),
            )
            .expect("insert knows chunk");
    }

    if with_hnsw {
        engine
            .execute_sql(
                &session,
                "CREATE INDEX idx_hnsw ON record USING hnsw (embedding)",
            )
            .expect("create record hnsw index");
    }

    (engine, session)
}

fn assert_int_column(rows: &[Row], column: usize, expected: &[i32]) {
    let actual: Vec<i32> = rows
        .iter()
        .map(|row| match &row.values[column] {
            Value::Int(value) => *value,
            other => panic!("expected Int in column {column}, got {other:?}"),
        })
        .collect();
    assert_eq!(actual, expected);
}

fn assert_bigint_column(rows: &[Row], column: usize, expected: &[i64]) {
    let actual: Vec<i64> = rows
        .iter()
        .map(|row| match &row.values[column] {
            Value::Int(value) => i64::from(*value),
            Value::BigInt(value) => *value,
            other => panic!("expected Int or BigInt in column {column}, got {other:?}"),
        })
        .collect();
    assert_eq!(actual, expected);
}

fn assert_text_column(rows: &[Row], column: usize, expected: &[&str]) {
    let actual: Vec<&str> = rows
        .iter()
        .map(|row| match &row.values[column] {
            Value::Text(value) => value.as_str(),
            other => panic!("expected Text in column {column}, got {other:?}"),
        })
        .collect();
    assert_eq!(actual, expected);
}

fn assert_distance_approx(row: &Row, column: usize, expected: f64) {
    let actual = match &row.values[column] {
        Value::Double(value) => *value,
        other => panic!("expected Double in column {column}, got {other:?}"),
    };
    assert!(
        (actual - expected).abs() < 1e-9,
        "expected distance {expected}, got {actual}"
    );
}

#[test]
fn cypher_one_hop_where_vector_distance_filters_expected_rows() {
    let (engine, session) = setup_tenant_graph_vector_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (u:User)-[:WROTE]->(d:Document) \
         WHERE l2_distance(d.embedding, '[0.1, 0.8, 0.2]') < 0.5 \
         RETURN u.name, d.title \
         ORDER BY u.name, d.title",
    );

    assert_text_column(&rows, 0, &["Alice", "Alice", "Bob"]);
    assert_text_column(
        &rows,
        1,
        &["AionDB Guide", "Graph Vector DBs", "Postgres vs AionDB"],
    );
}

#[test]
fn cypher_two_hop_where_start_tenant_keeps_expected_rows() {
    let (engine, session) = setup_tenant_graph_vector_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (u:User)-[:WROTE]->(s:Document)-[:CITES]->(t:Document) \
         WHERE u.tenant_id = 100 \
         RETURN u.name, s.title, t.title \
         ORDER BY u.name, s.title, t.title",
    );

    assert_text_column(&rows, 0, &["Alice", "Bob"]);
    assert_text_column(&rows, 1, &["Graph Vector DBs", "Postgres vs AionDB"]);
    assert_text_column(&rows, 2, &["AionDB Guide", "AionDB Guide"]);
}

// ---------------------------------------------------------------------------
// Official mixed query 1: entity graph filter + vector rerank
// ---------------------------------------------------------------------------

#[test]
fn official_mixed_query_01_incident_docs_reranked_by_l2() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT d.id, d.title, l2_distance(d.embedding, q.embedding) AS dist \
         FROM docs d \
         JOIN doc_mentions dm ON dm.source_id = d.id \
         JOIN concepts c ON c.id = dm.target_id \
         JOIN query_vectors q ON q.id = 1 \
         WHERE c.name = 'incident-response' \
         ORDER BY dist, d.id \
         LIMIT 3",
    );

    assert_int_column(&rows, 0, &[2, 4, 1]);
    assert_text_column(
        &rows,
        1,
        &[
            "Pager Escalation Guide",
            "Database Recovery Runbook",
            "Incident Response Playbook",
        ],
    );
    assert_distance_approx(&rows[0], 2, 0.0);
}

// ---------------------------------------------------------------------------
// Official mixed query 2: neighbors graph then rerank vector
// ---------------------------------------------------------------------------

#[test]
fn official_mixed_query_02_neighbors_graph_then_rerank_vector() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "WITH neighbor_docs AS ( \
             SELECT d2.id, d2.title, d2.embedding \
             FROM doc_links dl \
             JOIN docs d2 ON d2.id = dl.target_id \
             WHERE dl.source_id = 1 \
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

// ---------------------------------------------------------------------------
// Official mixed query 3: concept neighborhood + vector rerank
// ---------------------------------------------------------------------------

#[test]
fn official_mixed_query_03_shared_concept_docs_for_postmortem_query() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "WITH doc1_concepts AS ( \
             SELECT target_id AS concept_id \
             FROM doc_mentions \
             WHERE source_id = 1 \
         ) \
         SELECT DISTINCT d.id, d.title, l2_distance(d.embedding, q.embedding) AS dist \
         FROM docs d \
         JOIN doc_mentions dm ON dm.source_id = d.id \
         JOIN doc1_concepts dc ON dc.concept_id = dm.target_id \
         JOIN query_vectors q ON q.id = 2 \
         WHERE d.id <> 1 \
         ORDER BY dist, d.id \
         LIMIT 3",
    );

    assert_int_column(&rows, 0, &[3, 4, 2]);
    assert_text_column(
        &rows,
        1,
        &[
            "Postmortem Template",
            "Database Recovery Runbook",
            "Pager Escalation Guide",
        ],
    );
}

// ---------------------------------------------------------------------------
// Official mixed query 4: graph-constrained runbooks + cosine rerank
// ---------------------------------------------------------------------------

#[test]
fn official_mixed_query_04_database_runbooks_ranked_by_cosine_distance() {
    let (engine, session) = setup_knowledge_graph_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT d.id, d.title, cosine_distance(d.embedding, q.embedding) AS dist \
         FROM docs d \
         JOIN doc_mentions dm ON dm.source_id = d.id \
         JOIN concepts c ON c.id = dm.target_id \
         JOIN query_vectors q ON q.id = 1 \
         WHERE c.name = 'database' \
           AND d.kind = 'runbook' \
         ORDER BY dist, d.id \
         LIMIT 2",
    );

    assert_int_column(&rows, 0, &[4]);
    assert_text_column(&rows, 1, &["Database Recovery Runbook"]);
}

// ---------------------------------------------------------------------------
// Official mixed query 5: brand graph + SQL filter + vector rerank
// ---------------------------------------------------------------------------

#[test]
fn official_mixed_query_05_same_brand_candidates_for_portable_audio() {
    let (engine, session) = setup_catalog_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT p.id, p.title, l2_distance(p.embedding, q.embedding) AS dist \
         FROM product_brand_edges pbe \
         JOIN products anchor ON anchor.id = pbe.source_id \
         JOIN product_brand_edges pbe2 ON pbe2.target_id = pbe.target_id \
         JOIN products p ON p.id = pbe2.source_id \
         JOIN query_vectors q ON q.id = 1 \
         WHERE anchor.id = 1 \
           AND p.id <> anchor.id \
           AND p.price <= 250 \
         ORDER BY dist, p.id \
         LIMIT 3",
    );

    assert_int_column(&rows, 0, &[3, 2, 6]);
    assert_text_column(
        &rows,
        1,
        &["Wireless Earbuds", "Travel Carry Case", "Speaker Stand"],
    );
}

// ---------------------------------------------------------------------------
// Official mixed query 6: product neighbors via edges then rerank
// ---------------------------------------------------------------------------

#[test]
fn official_mixed_query_06_bundle_neighbors_then_vector_rerank() {
    let (engine, session) = setup_catalog_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT p.id, p.title, l2_distance(p.embedding, q.embedding) AS dist \
         FROM product_links pl \
         JOIN products p ON p.id = pl.target_id \
         JOIN query_vectors q ON q.id = 1 \
         WHERE pl.source_id = 1 \
           AND pl.relation = 'bundle' \
           AND p.price <= 100 \
         ORDER BY dist, p.id \
         LIMIT 2",
    );

    assert_int_column(&rows, 0, &[2, 6]);
    assert_text_column(&rows, 1, &["Travel Carry Case", "Speaker Stand"]);
}

// ---------------------------------------------------------------------------
// Official mixed query 7: brand neighborhood for media/living-room intent
// ---------------------------------------------------------------------------

#[test]
fn official_mixed_query_07_retro_sound_recommendations_by_brand_and_vector() {
    let (engine, session) = setup_catalog_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT p.id, p.title, l2_distance(p.embedding, q.embedding) AS dist \
         FROM product_brand_edges seed_brand \
         JOIN product_brand_edges candidates ON candidates.target_id = seed_brand.target_id \
         JOIN products p ON p.id = candidates.source_id \
         JOIN query_vectors q ON q.id = 2 \
         WHERE seed_brand.source_id = 4 \
           AND p.id <> 4 \
           AND p.price <= 250 \
         ORDER BY dist, p.id \
         LIMIT 2",
    );

    assert_int_column(&rows, 0, &[5, 7]);
    assert_text_column(&rows, 1, &["Jazz Streaming Pass", "Hi-Fi Amplifier"]);
}

// ---------------------------------------------------------------------------
// Official mixed query 8: local note neighbors reranked by query vector
// ---------------------------------------------------------------------------

#[test]
fn official_mixed_query_08_local_note_neighbors_for_debug_query() {
    let (engine, session) = setup_workspace_context_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT n.id, n.title, l2_distance(n.embedding, i.embedding) AS dist \
         FROM note_links nl \
         JOIN notes anchor ON anchor.id = nl.source_id \
         JOIN notes n ON n.id = nl.target_id \
         JOIN query_vectors i ON i.id = 1 \
         WHERE anchor.id = 1 \
           AND n.project_id = anchor.project_id \
         ORDER BY dist, n.id \
         LIMIT 2",
    );

    assert_int_column(&rows, 0, &[2, 3]);
    assert_text_column(
        &rows,
        1,
        &["Extension Debugging Guide", "Release Checklist"],
    );
}

// ---------------------------------------------------------------------------
// Official mixed query 9: open tasks linked to notes, reranked by vector
// ---------------------------------------------------------------------------

#[test]
fn official_mixed_query_09_open_tasks_from_note_graph_ranked_for_debug() {
    let (engine, session) = setup_workspace_context_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "SELECT t.id, t.title, l2_distance(t.embedding, i.embedding) AS dist \
         FROM note_task_edges nte \
         JOIN notes n ON n.id = nte.source_id \
         JOIN tasks t ON t.id = nte.target_id \
         JOIN query_vectors i ON i.id = 1 \
         WHERE n.project_id = 1 \
           AND t.status = 'open' \
         ORDER BY dist, t.id \
         LIMIT 2",
    );

    assert_int_column(&rows, 0, &[11, 10]);
    assert_text_column(&rows, 1, &["Improve Extension Logs", "Fix Startup Crash"]);
}

// ---------------------------------------------------------------------------
// Official mixed query 10: two-hop note->task neighborhood reranked
// ---------------------------------------------------------------------------

#[test]
fn official_mixed_query_10_release_neighbors_then_rerank_tasks() {
    let (engine, session) = setup_workspace_context_dataset();

    let rows = query_rows(
        &engine,
        &session,
        "WITH neighbor_notes AS ( \
             SELECT target_id AS note_id \
             FROM note_links \
             WHERE source_id = 1 \
         ) \
         SELECT t.id, t.title, l2_distance(t.embedding, i.embedding) AS dist \
         FROM neighbor_notes nn \
         JOIN note_task_edges nte ON nte.source_id = nn.note_id \
         JOIN tasks t ON t.id = nte.target_id \
         JOIN query_vectors i ON i.id = 2 \
         ORDER BY dist, t.id \
         LIMIT 2",
    );

    assert_int_column(&rows, 0, &[12, 11]);
    assert_text_column(
        &rows,
        1,
        &["Publish Release Notes", "Improve Extension Logs"],
    );
}

#[test]
fn surreal_suite_complex_vector_join_sql_executes() {
    let (engine, session) = setup_surreal_suite_complex_dataset(2_000, true);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT r.id, p.category, q.category, k.weight, \
                l2_distance(r.embedding, '[1.0,0.0,0.0]') AS distance \
         FROM record r \
         JOIN person p ON p.id = r.id \
         JOIN knows k ON k.source_id = p.id \
         JOIN person q ON q.id = k.target_id \
         WHERE p.number BETWEEN 10 AND 80 \
           AND k.weight BETWEEN 5 AND 35 \
           AND (q.category = 'c1' OR q.category = 'c2' OR q.category = 'c3') \
         ORDER BY l2_distance(r.embedding, '[1.0,0.0,0.0]'), k.weight DESC \
         LIMIT 50",
    );

    assert!(
        !rows.is_empty(),
        "expected benchmark vector join query to return rows"
    );
    assert!(rows.len() <= 50, "expected LIMIT 50 to be respected");
}

#[test]
fn surreal_suite_complex_vector_join_sql_executes_at_benchmark_scale() {
    let (engine, session) = setup_surreal_suite_complex_dataset(5_000, true);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT r.id, p.category, q.category, k.weight, \
                l2_distance(r.embedding, '[1.0,0.0,0.0]') AS distance \
         FROM record r \
         JOIN person p ON p.id = r.id \
         JOIN knows k ON k.source_id = p.id \
         JOIN person q ON q.id = k.target_id \
         WHERE p.number BETWEEN 10 AND 80 \
           AND k.weight BETWEEN 5 AND 35 \
           AND (q.category = 'c1' OR q.category = 'c2' OR q.category = 'c3') \
         ORDER BY l2_distance(r.embedding, '[1.0,0.0,0.0]'), k.weight DESC \
         LIMIT 50",
    );

    assert!(
        !rows.is_empty(),
        "expected benchmark-scale vector join query to return rows"
    );
    assert!(
        rows.len() <= 50,
        "expected LIMIT 50 to be respected at benchmark scale"
    );
}

#[test]
fn explain_surreal_suite_complex_vector_join_sql() {
    let (engine, session) = setup_surreal_suite_complex_dataset(2_000, true);

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT r.id, p.category, q.category, k.weight, \
                l2_distance(r.embedding, '[1.0,0.0,0.0]') AS distance \
         FROM record r \
         JOIN person p ON p.id = r.id \
         JOIN knows k ON k.source_id = p.id \
         JOIN person q ON q.id = k.target_id \
         WHERE p.number BETWEEN 10 AND 80 \
           AND k.weight BETWEEN 5 AND 35 \
           AND (q.category = 'c1' OR q.category = 'c2' OR q.category = 'c3') \
         ORDER BY l2_distance(r.embedding, '[1.0,0.0,0.0]'), k.weight DESC \
         LIMIT 50",
    );

    assert!(
        !lines.is_empty(),
        "expected EXPLAIN output for benchmark vector join query"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Nested Loop Index Join")),
        "expected index joins for the benchmark vector chain, got {lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line == "              ->  Nested Loop (rows=14950)"),
        "unexpected disconnected nested-loop subtree in vector benchmark plan: {lines:?}"
    );
}

#[test]
fn explain_surreal_suite_complex_vector_join_sql_at_benchmark_scale() {
    let (engine, session) = setup_surreal_suite_complex_dataset(5_000, true);

    let lines = explain_lines(
        &engine,
        &session,
        "EXPLAIN SELECT r.id, p.category, q.category, k.weight, \
                l2_distance(r.embedding, '[1.0,0.0,0.0]') AS distance \
         FROM record r \
         JOIN person p ON p.id = r.id \
         JOIN knows k ON k.source_id = p.id \
         JOIN person q ON q.id = k.target_id \
         WHERE p.number BETWEEN 10 AND 80 \
           AND k.weight BETWEEN 5 AND 35 \
           AND (q.category = 'c1' OR q.category = 'c2' OR q.category = 'c3') \
         ORDER BY l2_distance(r.embedding, '[1.0,0.0,0.0]'), k.weight DESC \
         LIMIT 50",
    );

    println!("{lines:#?}");
    assert!(
        !lines.is_empty(),
        "expected EXPLAIN output at benchmark scale"
    );
}

#[test]
fn surreal_suite_complex_relational_fanout_sql_executes() {
    let (engine, session) = setup_surreal_suite_complex_dataset(2_000, false);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT p.category, q.category AS target_category, \
                count(*) AS edges, avg(k.weight) AS avg_weight, \
                count(DISTINCT r.number) AS distinct_numbers \
         FROM person p \
         JOIN record r ON r.id = p.id \
         JOIN knows k ON k.source_id = p.id \
         JOIN person q ON q.id = k.target_id \
         JOIN record rq ON rq.id = q.id \
         WHERE r.number BETWEEN 10 AND 90 \
           AND rq.number2 BETWEEN 15 AND 85 \
         GROUP BY p.category, q.category \
         ORDER BY edges DESC, avg_weight DESC \
         LIMIT 25",
    );

    assert!(
        !rows.is_empty(),
        "expected benchmark fanout aggregate query to return rows"
    );
    assert!(rows.len() <= 25, "expected LIMIT 25 to be respected");
}

mod explain_shapes;
