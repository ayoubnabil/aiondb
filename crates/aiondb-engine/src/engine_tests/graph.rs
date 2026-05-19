use super::*;
use aiondb_core::SqlState;
mod cypher_regression;
mod implicit_column_rewrite;
// ===================================================================
// CREATE NODE LABEL
// ===================================================================

#[test]
fn create_node_label() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT)",
        )
        .expect("create table");

    let results = engine
        .execute_sql(&session, "CREATE NODE LABEL person ON persons")
        .expect("create node label");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE NODE LABEL".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn create_node_label_case_insensitive() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT)",
        )
        .expect("create table");

    let results = engine
        .execute_sql(&session, "CREATE NODE LABEL Person ON Persons")
        .expect("create node label case insensitive");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE NODE LABEL".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn create_node_and_edge_labels_resolve_tables_from_later_search_path_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.people_path (id INT NOT NULL, name TEXT); \
             CREATE TABLE analytics.knows_edges_path (source_id INT NOT NULL, target_id INT NOT NULL); \
             SET search_path TO public, analytics; \
             CREATE NODE LABEL person_path ON people_path; \
             CREATE EDGE LABEL knows_path ON knows_edges_path SOURCE person_path TARGET person_path",
        )
        .expect("create graph labels via later search_path schema");

    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "CREATE SCHEMA".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "SET".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "CREATE NODE LABEL".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "CREATE EDGE LABEL".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

// ===================================================================
// CREATE EDGE LABEL
// ===================================================================

#[test]
fn create_edge_label() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person ON persons",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE EDGE LABEL knows ON knows_edges SOURCE person TARGET person",
        )
        .expect("create edge label");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE EDGE LABEL".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn create_edge_label_different_source_and_target() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT); \
             CREATE TABLE companies (id INT NOT NULL, name TEXT); \
             CREATE TABLE works_at_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person ON persons; \
             CREATE NODE LABEL company ON companies",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE EDGE LABEL works_at ON works_at_edges SOURCE person TARGET company",
        )
        .expect("create edge label with different endpoints");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE EDGE LABEL".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn create_edge_label_can_project_existing_fk_columns() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE employees (id INT NOT NULL, name TEXT); \
             CREATE TABLE tickets (id INT NOT NULL, assigned_to INT, title TEXT); \
             INSERT INTO employees VALUES (10, 'alice'), (20, 'bob'); \
             INSERT INTO tickets VALUES (1, 10, 'login'), (2, 20, 'billing'), (3, NULL, 'triage'); \
             CREATE NODE LABEL Employee ON employees; \
             CREATE NODE LABEL Ticket ON tickets; \
             CREATE EDGE LABEL handled_by ON tickets SOURCE Ticket KEY (id) TARGET Employee KEY (assigned_to)",
        )
        .expect("create FK-backed edge label");

    let outgoing = query_rows(
        &engine,
        &session,
        "SELECT employee_id FROM graph_neighbors('handled_by', 1) AS g(employee_id)",
    );
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].values[0], Value::BigInt(10));

    let incoming = query_rows(
        &engine,
        &session,
        "SELECT ticket_id FROM graph_neighbors('handled_by', 20, 'incoming') AS g(ticket_id)",
    );
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].values[0], Value::BigInt(2));

    let cypher = query_rows(
        &engine,
        &session,
        "MATCH (t:Ticket {id: 1})-[:handled_by]->(e:Employee) RETURN e.id",
    );
    assert_eq!(cypher.len(), 1);
    assert_eq!(cypher[0].values[0], Value::Int(10));

    engine
        .execute_sql(
            &session,
            "CREATE (:Ticket {id: 4, assigned_to: 10, title: 'new'}) RETURN 1",
        )
        .expect("FK-backed edge table remains writable as a node row");
    let created = query_rows(
        &engine,
        &session,
        "SELECT employee_id FROM graph_neighbors('handled_by', 4) AS g(employee_id)",
    );
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].values[0], Value::BigInt(10));
}

#[test]
fn explain_match_includes_graph_access_lines() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain (source_id INT NOT NULL, target_id INT NOT NULL); \
             INSERT INTO people_explain VALUES (1, 'Alice'), (2, 'Bob'); \
             INSERT INTO knows_explain VALUES (1, 2); \
             CREATE NODE LABEL person_explain ON people_explain; \
             CREATE EDGE LABEL knows_explain ON knows_explain SOURCE person_explain TARGET person_explain",
        )
        .expect("setup graph explain tables");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN MATCH (a:person_explain)-[:knows_explain]->(b:person_explain) RETURN b.id",
        )
        .expect("execute explain match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines
            .iter()
            .any(|line| line.contains("Graph Access [") && line.contains("pattern 0]")),
        "explain lines: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("source=Some(TraversalStore)")),
        "explain lines: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("fallback=Some(RowStore)")),
        "explain lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Access Summary:")
                && line.contains("row_store_source=0")
                && line.contains("traversal_store_source=1")
                && line.contains("row_fallback_patterns=1")
                && line.contains("row_store_traversal_patterns=0")
                && line.contains("source=inferred")
        }),
        "explain lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Access Warning:")
                && line.contains("0 relationship patterns are row-store only")
                && line.contains("1 patterns still keep a row-store fallback")
                && line.contains("source=inferred")
        }),
        "explain lines: {lines:?}"
    );
}

#[test]
fn explain_match_reports_fast_one_hop_id_lookup_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_fast_runtime (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain_fast_runtime (source_id INT NOT NULL, target_id INT NOT NULL); \
             INSERT INTO people_explain_fast_runtime VALUES (1, 'Alice'), (2, 'Bob'); \
             INSERT INTO knows_explain_fast_runtime VALUES (1, 2); \
             CREATE NODE LABEL person_explain_fast_runtime ON people_explain_fast_runtime; \
             CREATE EDGE LABEL knows_explain_fast_runtime ON knows_explain_fast_runtime SOURCE person_explain_fast_runtime TARGET person_explain_fast_runtime",
        )
        .expect("setup explain fast runtime tables");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN MATCH (a:person_explain_fast_runtime {id: 1})-[:knows_explain_fast_runtime]->(b:person_explain_fast_runtime) RETURN b.id",
        )
        .expect("execute explain fast runtime match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Query Runtime:")
                && line.contains("strategy=fast_one_hop_id_lookup")
                && line.contains("reason=anchored_start_id_to_target_id")
                && line.contains("source=inferred")
        }),
        "explain lines: {lines:?}"
    );
}

#[test]
fn explain_match_reports_fast_one_hop_endpoint_id_lookup_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_fast_endpoint (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain_fast_endpoint (source_id INT NOT NULL, target_id INT NOT NULL); \
             INSERT INTO people_explain_fast_endpoint VALUES (1, 'Alice'), (2, 'Bob'); \
             INSERT INTO knows_explain_fast_endpoint VALUES (1, 2); \
             CREATE NODE LABEL person_explain_fast_endpoint ON people_explain_fast_endpoint; \
             CREATE EDGE LABEL knows_explain_fast_endpoint ON knows_explain_fast_endpoint SOURCE person_explain_fast_endpoint TARGET person_explain_fast_endpoint",
        )
        .expect("setup explain fast endpoint tables");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN MATCH (a:person_explain_fast_endpoint)-[:knows_explain_fast_endpoint]->(b:person_explain_fast_endpoint {id: 2}) RETURN a.id ORDER BY a.id",
        )
        .expect("execute explain fast endpoint match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Query Runtime:")
                && line.contains("strategy=fast_one_hop_endpoint_id_lookup")
                && line.contains("reason=anchored_endpoint_id_lookup")
                && line.contains("source=inferred")
        }),
        "explain lines: {lines:?}"
    );
}

#[test]
fn explain_match_reports_fast_unanchored_edge_filter_limit_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_fast_edge_filter (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain_fast_edge_filter (source_id INT NOT NULL, target_id INT NOT NULL, weight INT); \
             INSERT INTO people_explain_fast_edge_filter VALUES (1, 'Alice'), (2, 'Bob'); \
             INSERT INTO knows_explain_fast_edge_filter VALUES (1, 2, 10); \
             CREATE NODE LABEL person_explain_fast_edge_filter ON people_explain_fast_edge_filter; \
             CREATE EDGE LABEL knows_explain_fast_edge_filter ON knows_explain_fast_edge_filter SOURCE person_explain_fast_edge_filter TARGET person_explain_fast_edge_filter",
        )
        .expect("setup explain fast edge-filter tables");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN MATCH (a:person_explain_fast_edge_filter)-[r:knows_explain_fast_edge_filter]->(b:person_explain_fast_edge_filter) WHERE r.weight > 5 RETURN b.id LIMIT 10",
        )
        .expect("execute explain fast edge-filter match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Query Runtime:")
                && line.contains("strategy=fast_unanchored_edge_filter_limit")
                && line.contains("reason=unanchored_edge_weight_gt_limit")
                && line.contains("source=inferred")
        }),
        "explain lines: {lines:?}"
    );
}

#[test]
fn explain_match_reports_fast_unanchored_edge_eq_filter_limit_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_fast_edge_eq (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain_fast_edge_eq (source_id INT NOT NULL, target_id INT NOT NULL, weight INT); \
             INSERT INTO people_explain_fast_edge_eq VALUES (1, 'Alice'), (2, 'Bob'); \
             INSERT INTO knows_explain_fast_edge_eq VALUES (1, 2, 10); \
             CREATE NODE LABEL person_explain_fast_edge_eq ON people_explain_fast_edge_eq; \
             CREATE EDGE LABEL knows_explain_fast_edge_eq ON knows_explain_fast_edge_eq SOURCE person_explain_fast_edge_eq TARGET person_explain_fast_edge_eq",
        )
        .expect("setup explain fast edge-eq tables");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN MATCH (a:person_explain_fast_edge_eq)-[:knows_explain_fast_edge_eq {weight: 10}]->(b:person_explain_fast_edge_eq) RETURN b.id LIMIT 10",
        )
        .expect("execute explain fast edge-eq match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Query Runtime:")
                && line.contains("strategy=fast_unanchored_edge_eq_filter_limit")
                && line.contains("reason=unanchored_edge_weight_eq_limit")
                && line.contains("source=inferred")
        }),
        "explain lines: {lines:?}"
    );
}

#[test]
fn explain_match_reports_fast_unanchored_one_hop_limit_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_fast_one_hop_limit (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain_fast_one_hop_limit (source_id INT NOT NULL, target_id INT NOT NULL); \
             INSERT INTO people_explain_fast_one_hop_limit VALUES (1, 'Alice'), (2, 'Bob'); \
             INSERT INTO knows_explain_fast_one_hop_limit VALUES (1, 2); \
             CREATE NODE LABEL person_explain_fast_one_hop_limit ON people_explain_fast_one_hop_limit; \
             CREATE EDGE LABEL knows_explain_fast_one_hop_limit ON knows_explain_fast_one_hop_limit SOURCE person_explain_fast_one_hop_limit TARGET person_explain_fast_one_hop_limit",
        )
        .expect("setup explain fast one-hop limit tables");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN MATCH (a:person_explain_fast_one_hop_limit)-[:knows_explain_fast_one_hop_limit]->(b:person_explain_fast_one_hop_limit) RETURN b.name LIMIT 10",
        )
        .expect("execute explain fast one-hop limit match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Query Runtime:")
                && line.contains("strategy=fast_unanchored_one_hop_limit")
                && line.contains("reason=single_hop_projection_limit")
                && line.contains("source=inferred")
        }),
        "explain lines: {lines:?}"
    );
}

#[test]
fn explain_analyze_match_reports_fast_multi_out_limit_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_fast_multi (id INT NOT NULL, number INT); \
             CREATE TABLE knows_explain_fast_multi_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_explain_fast_multi ON people_explain_fast_multi; \
             CREATE EDGE LABEL knows_explain_fast_multi ON knows_explain_fast_multi_edges SOURCE person_explain_fast_multi TARGET person_explain_fast_multi; \
             INSERT INTO people_explain_fast_multi VALUES (1, 0), (2, 30), (3, 10), (4, 40); \
             INSERT INTO knows_explain_fast_multi_edges VALUES (1, 2), (1, 3), (1, 4)",
        )
        .expect("setup explain fast multi-out tables");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN ANALYZE MATCH (a:person_explain_fast_multi)-[:knows_explain_fast_multi]->(b:person_explain_fast_multi), \
                    (a)-[:knows_explain_fast_multi]->(c:person_explain_fast_multi) \
             RETURN b.id, c.id LIMIT 4",
        )
        .expect("execute explain analyze fast multi-out match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain analyze query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Query Runtime:")
                && line.contains("strategy=fast_multi_out_limit")
                && line.contains("reason=shared_source_dual_expand_limit")
                && line.contains("source=observed")
        }),
        "explain lines: {lines:?}"
    );
}

#[test]
fn explain_analyze_match_reports_fast_unanchored_two_hop_limit_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_fast_two_hop_limit (id INT NOT NULL, number INT NOT NULL); \
             CREATE TABLE knows_explain_fast_two_hop_limit_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_explain_fast_two_hop_limit ON people_explain_fast_two_hop_limit; \
             CREATE EDGE LABEL knows_explain_fast_two_hop_limit ON knows_explain_fast_two_hop_limit_edges SOURCE person_explain_fast_two_hop_limit TARGET person_explain_fast_two_hop_limit; \
             INSERT INTO people_explain_fast_two_hop_limit VALUES (1, 0), (2, 0), (3, 0), (4, 70), (5, 80), (6, 10); \
             INSERT INTO knows_explain_fast_two_hop_limit_edges VALUES (1, 2), (1, 3), (2, 4), (3, 4), (3, 5), (6, 3)",
        )
        .expect("setup explain fast two-hop limit tables");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN ANALYZE MATCH (a:person_explain_fast_two_hop_limit)-[:knows_explain_fast_two_hop_limit]->(b:person_explain_fast_two_hop_limit)-[:knows_explain_fast_two_hop_limit]->(c:person_explain_fast_two_hop_limit) \
             RETURN c.id LIMIT 4",
        )
        .expect("execute explain analyze fast two-hop limit match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain analyze query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Query Runtime:")
                && line.contains("strategy=fast_unanchored_two_hop_limit")
                && line.contains("reason=two_hop_projection_limit")
                && line.contains("source=observed")
        }),
        "explain lines: {lines:?}"
    );
}

#[test]
fn explain_analyze_match_reports_fast_hybrid_graph_vector_rel_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users_explain_hybrid (id INT NOT NULL, name TEXT, tenant_id INT); \
             CREATE TABLE docs_explain_hybrid (id INT NOT NULL, title TEXT, embedding VECTOR(3), tenant_id INT); \
             CREATE TABLE wrote_explain_hybrid (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE TABLE cites_explain_hybrid (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE INDEX idx_users_explain_hybrid_id ON users_explain_hybrid(id); \
             CREATE INDEX idx_users_explain_hybrid_tenant ON users_explain_hybrid(tenant_id); \
             CREATE INDEX idx_docs_explain_hybrid_id ON docs_explain_hybrid(id); \
             CREATE NODE LABEL UserHybrid ON users_explain_hybrid; \
             CREATE NODE LABEL DocumentHybrid ON docs_explain_hybrid; \
             CREATE EDGE LABEL WROTE_HYBRID ON wrote_explain_hybrid SOURCE UserHybrid TARGET DocumentHybrid; \
             CREATE EDGE LABEL CITES_HYBRID ON cites_explain_hybrid SOURCE DocumentHybrid TARGET DocumentHybrid; \
             INSERT INTO users_explain_hybrid VALUES (1, 'Alice', 100), (2, 'Bob', 100), (3, 'Cara', 200); \
             INSERT INTO docs_explain_hybrid VALUES \
                 (10, 'AionDB Guide', '[0.1,0.9,0.2]', 100), \
                 (11, 'Postgres vs AionDB', '[0.2,0.8,0.1]', 100), \
                 (12, 'Secret Project X', '[0.9,0.1,0.1]', 200), \
                 (13, 'Graph Vector DBs', '[0.15,0.85,0.2]', 100); \
             INSERT INTO wrote_explain_hybrid VALUES (1, 11), (1, 13), (2, 10), (3, 12); \
             INSERT INTO cites_explain_hybrid VALUES (11, 10), (13, 10), (12, 13)",
        )
        .expect("setup explain hybrid graph-vector tables");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN ANALYZE MATCH (u:UserHybrid)-[:WROTE_HYBRID]->(s:DocumentHybrid)-[:CITES_HYBRID]->(t:DocumentHybrid) \
             WHERE u.tenant_id = 100 \
               AND t.tenant_id = 100 \
               AND l2_distance(t.embedding, '[0.1,0.8,0.2]') < 0.5 \
             RETURN u.name, s.title, t.title \
             ORDER BY u.name LIMIT 10",
        )
        .expect("execute explain analyze hybrid graph-vector match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain analyze query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Query Runtime:")
                && line.contains("strategy=fast_hybrid_graph_vector_rel")
                && line.contains("reason=graph_vector_distance_threshold")
                && line.contains("source=observed")
        }),
        "explain lines: {lines:?}"
    );
}

#[test]
fn explain_analyze_match_reports_fast_hybrid_deep_graph_vector_rel_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users_explain_hybrid_deep (id INT NOT NULL, name TEXT, tenant_id INT); \
             CREATE TABLE docs_explain_hybrid_deep (id INT NOT NULL, title TEXT, embedding VECTOR(3), tenant_id INT, popularity INT); \
             CREATE TABLE follows_explain_hybrid_deep (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE TABLE wrote_explain_hybrid_deep (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE TABLE cites_explain_hybrid_deep (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE INDEX idx_users_explain_hybrid_deep_id ON users_explain_hybrid_deep(id); \
             CREATE INDEX idx_docs_explain_hybrid_deep_id ON docs_explain_hybrid_deep(id); \
             CREATE NODE LABEL UserHybridDeep ON users_explain_hybrid_deep; \
             CREATE NODE LABEL DocumentHybridDeep ON docs_explain_hybrid_deep; \
             CREATE EDGE LABEL FOLLOWS_HYBRID_DEEP ON follows_explain_hybrid_deep SOURCE UserHybridDeep TARGET UserHybridDeep; \
             CREATE EDGE LABEL WROTE_HYBRID_DEEP ON wrote_explain_hybrid_deep SOURCE UserHybridDeep TARGET DocumentHybridDeep; \
             CREATE EDGE LABEL CITES_HYBRID_DEEP ON cites_explain_hybrid_deep SOURCE DocumentHybridDeep TARGET DocumentHybridDeep; \
             INSERT INTO users_explain_hybrid_deep VALUES (1, 'Alice', 100), (2, 'Bob', 100), (3, 'Cara', 200); \
             INSERT INTO docs_explain_hybrid_deep VALUES \
                 (10, 'AionDB Guide', '[0.1,0.9,0.2]', 100, 80), \
                 (11, 'Postgres vs AionDB', '[0.2,0.8,0.1]', 100, 70), \
                 (12, 'Secret Project X', '[0.9,0.1,0.1]', 200, 20), \
                 (13, 'Graph Vector DBs', '[0.15,0.85,0.2]', 100, 95); \
             INSERT INTO follows_explain_hybrid_deep VALUES (1, 2), (1, 3); \
             INSERT INTO wrote_explain_hybrid_deep VALUES (2, 11), (3, 12); \
             INSERT INTO cites_explain_hybrid_deep VALUES (11, 10), (12, 13)",
        )
        .expect("setup explain deep hybrid graph-vector tables");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN ANALYZE MATCH (u:UserHybridDeep)-[:FOLLOWS_HYBRID_DEEP]->(f:UserHybridDeep)-[:WROTE_HYBRID_DEEP]->(s:DocumentHybridDeep)-[:CITES_HYBRID_DEEP]->(t:DocumentHybridDeep) \
             WHERE u.id = 1 \
               AND f.tenant_id = u.tenant_id \
               AND t.tenant_id = u.tenant_id \
               AND t.popularity > 50 \
               AND l2_distance(t.embedding, '[0.1,0.8,0.2]') < 0.5 \
             RETURN f.id, s.title, t.title, t.popularity, l2_distance(t.embedding, '[0.1,0.8,0.2]') AS dist \
             ORDER BY dist, t.popularity DESC LIMIT 10",
        )
        .expect("execute explain analyze deep hybrid graph-vector match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain analyze query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Query Runtime:")
                && line.contains("strategy=fast_hybrid_deep_graph_vector_rel")
                && line.contains("reason=deep_graph_vector_distance_threshold")
                && line.contains("source=observed")
        }),
        "explain lines: {lines:?}"
    );
}

#[test]
fn explain_analyze_match_includes_graph_actual_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_analyze (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain_analyze (source_id INT NOT NULL, target_id INT NOT NULL); \
             INSERT INTO people_explain_analyze VALUES (1, 'Alice'), (2, 'Bob'); \
             INSERT INTO knows_explain_analyze VALUES (1, 2); \
             CREATE NODE LABEL person_explain_analyze ON people_explain_analyze; \
             CREATE EDGE LABEL knows_explain_analyze ON knows_explain_analyze SOURCE person_explain_analyze TARGET person_explain_analyze",
        )
        .expect("setup graph explain analyze tables");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN ANALYZE MATCH (a:person_explain_analyze)-[:knows_explain_analyze]->(b:person_explain_analyze) RETURN b.id",
        )
        .expect("execute explain analyze match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Summary Severity:")
                && line.contains("severity=watch")
                && line.contains("fragile_pivots=1")
                && line.contains("source=mixed")
        }),
        "explain analyze lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Summary JSON:")
                && line.contains("\"severity\":\"watch\"")
                && line.contains("\"fragile_pivots\":1")
                && line.contains("\"drift_metrics_source\":\"observed\"")
                && line.contains("\"join_risk_metrics_source\":\"observed\"")
                && line.contains("\"risky_join_clauses\":0")
                && line.contains("\"max_fanout\":0.0")
        }),
        "explain analyze lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Drift Summary:")
                && line.contains("source=observed")
        }),
        "explain analyze lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Detail JSON:")
                && line.contains("\"summary\":{\"query_runtime_strategy\":\"general_graph_runtime\"")
                && line.contains("\"query_runtime_source\":\"observed\"")
                && line.contains("\"severity\":\"watch\"")
                && line.contains("\"clauses\":[{\"kind\":\"PipelineMatch\"")
                && line.contains("\"actual_rows\":1")
        }),
        "explain analyze lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Clause [PipelineMatch 0]")
                && line.contains("actual_input_rows=1")
                && line.contains("actual_output_rows=1")
                && line.contains("actual_selectivity=1.000")
                && line.contains("actual_time_ms=")
        }),
        "explain analyze lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Access [PipelineMatch 0 pattern 0]")
                && line.contains("estimated_rows=")
                && line.contains("actual_rows=1")
                && line.contains("estimate_error_ratio=1.000")
                && line.contains("actual_selectivity=1.000")
                && line.contains("actual_time_ms=")
        }),
        "explain analyze lines: {lines:?}"
    );
}

#[test]
fn query_engine_explain_graph_json_helpers_return_structured_payloads() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let query_engine: &dyn crate::engine::api::QueryEngine = &engine;

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_json (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain_json (source_id INT NOT NULL, target_id INT NOT NULL); \
             INSERT INTO people_explain_json VALUES (1, 'Alice'), (2, 'Bob'); \
             INSERT INTO knows_explain_json VALUES (1, 2); \
             CREATE NODE LABEL person_explain_json ON people_explain_json; \
             CREATE EDGE LABEL knows_explain_json ON knows_explain_json SOURCE person_explain_json TARGET person_explain_json",
        )
        .expect("setup graph explain json tables");

    let summary_json = query_engine
        .execute_explain_graph_summary_json(
            &session,
            "MATCH (a:person_explain_json)-[:knows_explain_json]->(b:person_explain_json) RETURN b.id",
            true,
        )
        .expect("summary json");
    let detail_json = query_engine
        .execute_explain_graph_detail_json(
            &session,
            "MATCH (a:person_explain_json)-[:knows_explain_json]->(b:person_explain_json) RETURN b.id",
            true,
        )
        .expect("detail json");

    assert_eq!(summary_json["severity"], "watch");
    assert_eq!(summary_json["fragile_pivots"], 1);
    assert_eq!(summary_json["row_store_source"], 0);
    assert_eq!(summary_json["traversal_store_source"], 1);
    assert_eq!(summary_json["row_fallback_patterns"], 1);
    assert_eq!(summary_json["row_store_traversal_patterns"], 0);
    assert_eq!(detail_json["summary"]["severity"], "watch");
    assert_eq!(detail_json["summary"]["row_store_source"], 0);
    assert_eq!(detail_json["summary"]["traversal_store_source"], 1);
    assert_eq!(detail_json["summary"]["row_fallback_patterns"], 1);
    assert_eq!(detail_json["summary"]["row_store_traversal_patterns"], 0);
    assert_eq!(detail_json["clauses"][0]["kind"], "PipelineMatch");
    assert_eq!(detail_json["clauses"][0]["pattern_details"][0]["actual_rows"], 1);
}

#[test]
fn query_engine_explain_graph_json_helpers_surface_cbo_selected_seed_end_to_end() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let query_engine: &dyn crate::engine::api::QueryEngine = &engine;

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_cbo_pivot (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain_cbo_pivot (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE INDEX people_explain_cbo_pivot_name_idx ON people_explain_cbo_pivot (name); \
             INSERT INTO people_explain_cbo_pivot VALUES (1, 'Alice'), (2, 'Bob'); \
             INSERT INTO knows_explain_cbo_pivot VALUES (1, 2); \
             ANALYZE people_explain_cbo_pivot; \
             ANALYZE knows_explain_cbo_pivot; \
             CREATE NODE LABEL person_explain_cbo_pivot ON people_explain_cbo_pivot; \
             CREATE EDGE LABEL knows_explain_cbo_pivot ON knows_explain_cbo_pivot SOURCE person_explain_cbo_pivot TARGET person_explain_cbo_pivot",
        )
        .expect("setup graph explain cbo pivot tables");

    let detail_json = query_engine
        .execute_explain_graph_detail_json(
            &session,
            "MATCH (a:person_explain_cbo_pivot)-[:knows_explain_cbo_pivot]->(b:person_explain_cbo_pivot {name: 'Bob'}) RETURN b.id",
            true,
        )
        .expect("detail json");

    assert_eq!(detail_json["clauses"][0]["pattern_details"][0]["seed_mode"], "indexed");
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["seed_mode_source"],
        "inferred"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["seed_constraints_source"],
        "inferred"
    );
    assert!(
        detail_json["clauses"][0]["pattern_details"][0]["seed"]
            .as_str()
            .is_some_and(|seed| seed.contains("{name}")),
        "detail_json={detail_json:?}"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_strategy"],
        "left_to_right_node_seed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_strategy_source"],
        "observed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pivot_decision"],
        "retained_leftmost"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["seed_binding_state_source"],
        "inferred"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["correlated_vars_source"],
        "inferred"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["first_rel_source"],
        "inferred"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["first_rel_mode_source"],
        "inferred"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["first_rel_constraints_source"],
        "inferred"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["bound_vars_source"],
        "inferred"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["flags_source"],
        "inferred"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["shape_source"],
        "inferred"
    );
}

#[test]
fn explain_format_json_returns_single_json_payload_row() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_format_json (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain_format_json (source_id INT NOT NULL, target_id INT NOT NULL); \
             INSERT INTO people_explain_format_json VALUES (1, 'Alice'), (2, 'Bob'); \
             INSERT INTO knows_explain_format_json VALUES (1, 2); \
             CREATE NODE LABEL person_explain_format_json ON people_explain_format_json; \
             CREATE EDGE LABEL knows_explain_format_json ON knows_explain_format_json SOURCE person_explain_format_json TARGET person_explain_format_json",
        )
        .expect("setup graph explain format json tables");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN (FORMAT JSON) MATCH (a:person_explain_format_json)-[:knows_explain_format_json]->(b:person_explain_format_json) RETURN b.id",
        )
        .expect("execute explain format json");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain query result");
    };
    assert_eq!(rows.len(), 1, "rows={rows:#?}");
    let [aiondb_core::Value::Text(payload)] = rows[0].values.as_slice() else {
        panic!("expected single text json row");
    };
    let payload: serde_json::Value = serde_json::from_str(payload).expect("json payload");
    assert_eq!(payload["schema_version"], 1);
    assert_eq!(payload["format_kind"], "aiondb.explain_json");
    assert_eq!(payload["graph_summary"]["severity"], "watch");
    assert_eq!(payload["graph_summary"]["row_store_source"], 0);
    assert_eq!(payload["graph_summary"]["traversal_store_source"], 1);
    assert_eq!(payload["graph_detail"]["summary"]["severity"], "watch");
    assert_eq!(payload["graph_detail"]["summary"]["row_store_source"], 0);
    assert_eq!(payload["graph_detail"]["summary"]["traversal_store_source"], 1);
    assert_eq!(payload["graph_detail"]["clauses"][0]["kind"], "PipelineMatch");
    assert!(
        payload["query_plan_lines"]
            .as_array()
            .is_some_and(|lines| !lines.is_empty())
    );
    assert!(
        payload["graph_lines"]
            .as_array()
            .is_some_and(|lines| !lines.is_empty())
    );
    assert_eq!(payload["plan_overview"]["root_kind"], "Cypher Query");
    assert!(
        payload["plan_overview"]["graph_line_count"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert!(payload["execution_summary"]["kind"].is_null());
}

#[test]
fn explain_analyze_format_json_returns_actual_graph_metrics() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_analyze_format_json (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain_analyze_format_json (source_id INT NOT NULL, target_id INT NOT NULL); \
             INSERT INTO people_explain_analyze_format_json VALUES (1, 'Alice'), (2, 'Bob'); \
             INSERT INTO knows_explain_analyze_format_json VALUES (1, 2); \
             CREATE NODE LABEL person_explain_analyze_format_json ON people_explain_analyze_format_json; \
             CREATE EDGE LABEL knows_explain_analyze_format_json ON knows_explain_analyze_format_json SOURCE person_explain_analyze_format_json TARGET person_explain_analyze_format_json",
        )
        .expect("setup graph explain analyze format json tables");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN (ANALYZE, FORMAT JSON) MATCH (a:person_explain_analyze_format_json)-[:knows_explain_analyze_format_json]->(b:person_explain_analyze_format_json) RETURN b.id",
        )
        .expect("execute explain analyze format json");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain query result");
    };
    assert_eq!(rows.len(), 1, "rows={rows:#?}");
    let [aiondb_core::Value::Text(payload)] = rows[0].values.as_slice() else {
        panic!("expected single text json row");
    };
    let payload: serde_json::Value = serde_json::from_str(payload).expect("json payload");
    assert_eq!(payload["schema_version"], 1);
    assert_eq!(payload["format_kind"], "aiondb.explain_json");
    assert_eq!(payload["graph_summary"]["severity"], "watch");
    assert_eq!(payload["graph_summary"]["drift_metrics_source"], "observed");
    assert_eq!(payload["graph_summary"]["join_risk_metrics_source"], "observed");
    assert_eq!(payload["graph_detail"]["summary"]["severity"], "watch");
    assert_eq!(payload["graph_detail"]["summary"]["drift_metrics_source"], "observed");
    assert_eq!(payload["graph_detail"]["summary"]["join_risk_metrics_source"], "observed");
    assert_eq!(
        payload["graph_detail"]["clauses"][0]["actual_input_rows"],
        1
    );
    assert_eq!(
        payload["graph_detail"]["clauses"][0]["actual_output_rows"],
        1
    );
    assert_eq!(
        payload["graph_detail"]["clauses"][0]["pattern_details"][0]["actual_rows"],
        1
    );
    assert_eq!(payload["plan_overview"]["root_kind"], "Cypher Query");
    assert_eq!(payload["execution_summary"]["kind"], "Query");
    assert_eq!(payload["execution_summary"]["rows_returned"], 1);
    assert!(
        payload["execution_summary"]["memory_used_bytes"]
            .as_u64()
            .is_some()
    );
}

#[test]
fn explain_analyze_shared_anchor_star_includes_per_pattern_actual_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_star (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain_star (source_id INT NOT NULL, target_id INT NOT NULL); \
             INSERT INTO people_explain_star VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Carol'); \
             INSERT INTO knows_explain_star VALUES (1, 2), (1, 3); \
             CREATE NODE LABEL person_explain_star ON people_explain_star; \
             CREATE EDGE LABEL knows_explain_star ON knows_explain_star SOURCE person_explain_star TARGET person_explain_star",
        )
        .expect("setup graph explain analyze star tables");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN ANALYZE MATCH (a:person_explain_star)-[:knows_explain_star]->(b:person_explain_star), (a)-[:knows_explain_star]->(c:person_explain_star) RETURN a.id, b.id, c.id",
        )
        .expect("execute explain analyze shared anchor star");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Clause [PipelineMatch 0]:")
                && line.contains("patterns=2")
                && line.contains("runtime_strategy=pattern_by_pattern")
                && line.contains("runtime_strategy_reason=general_multi_pattern_clause")
                && line.contains("runtime_strategy_reason_source=observed")
                && line.contains("runtime_strategy_blocker=anchor_not_shared")
                && line.contains("runtime_strategy_blocker_source=observed")
                && line.contains("runtime_strategy_source=observed")
        }),
        "explain analyze star lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Access [PipelineMatch 0 pattern 0]")
                && line.contains("actual_rows=2")
                && line.contains("actual_time_ms=")
        }),
        "explain analyze star lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Access [PipelineMatch 0 pattern 1]")
                && line.contains("actual_rows=4")
                && line.contains("actual_time_ms=")
        }),
        "explain analyze star lines: {lines:?}"
    );

    let detail_json = engine
        .execute_explain_graph_detail_json(
            &session,
            "MATCH (a:person_explain_star)-[:knows_explain_star]->(b:person_explain_star), (a)-[:knows_explain_star]->(c:person_explain_star) RETURN a.id, b.id, c.id",
            true,
        )
        .expect("graph detail json");
    assert_eq!(
        detail_json["clauses"][0]["runtime_strategy"],
        "pattern_by_pattern"
    );
    assert_eq!(
        detail_json["clauses"][0]["runtime_strategy_reason"],
        "general_multi_pattern_clause"
    );
    assert_eq!(
        detail_json["clauses"][0]["runtime_strategy_reason_source"],
        "observed"
    );
    assert_eq!(
        detail_json["clauses"][0]["runtime_strategy_blocker"],
        "anchor_not_shared"
    );
    assert_eq!(
        detail_json["clauses"][0]["runtime_strategy_blocker_source"],
        "observed"
    );
    assert_eq!(
        detail_json["clauses"][0]["runtime_strategy_source"],
        "observed"
    );
}

#[test]
fn explain_analyze_independent_multi_scan_reports_risk_severity() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_independent (id INT NOT NULL, name TEXT); \
             INSERT INTO people_explain_independent VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Carol'); \
             CREATE NODE LABEL person_explain_independent ON people_explain_independent",
        )
        .expect("setup independent explain graph");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN ANALYZE MATCH (a:person_explain_independent), (b:person_explain_independent) RETURN a.id, b.id",
        )
        .expect("execute explain analyze independent multi scan");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Summary Severity:")
                && line.contains("severity=risk")
                && line.contains("high_risk_join_clauses=1")
        }),
        "explain analyze independent lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Summary JSON:")
                && line.contains("\"severity\":\"risk\"")
                && line.contains("\"independent_multi_scan\":1")
                && line.contains("\"risky_join_clauses\":1")
                && line.contains("\"high_risk_join_clauses\":1")
                && line.contains("\"max_fanout\":9.0")
        }),
        "explain analyze independent lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Join Risk [PipelineMatch 0]:")
                && line.contains("severity=high")
                && line.contains("join_risk_source=observed")
                && line.contains("correlated_source=inferred")
                && line.contains("shared_anchor_source=inferred")
                && line.contains("join_shape_source=inferred")
                && line.contains("correlated=false")
                && line.contains("join_shape=independent_multi_scan")
        }),
        "explain analyze independent lines: {lines:?}"
    );

    let detail_json = engine
        .execute_explain_graph_detail_json(
            &session,
            "MATCH (a:person_explain_independent), (b:person_explain_independent) RETURN a.id, b.id",
            true,
        )
        .expect("graph detail json");
    assert_eq!(
        detail_json["clauses"][0]["join_risk"]["join_risk_source"],
        "observed"
    );
    assert_eq!(
        detail_json["clauses"][0]["join_risk"]["correlated_source"],
        "inferred"
    );
    assert_eq!(
        detail_json["clauses"][0]["join_risk"]["shared_anchor_source"],
        "inferred"
    );
    assert_eq!(
        detail_json["clauses"][0]["join_risk"]["join_shape_source"],
        "inferred"
    );
}

#[test]
fn explain_analyze_distinct_star_semijoin_includes_per_pattern_actual_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_distinct_star (id INT NOT NULL, number INT NOT NULL); \
             CREATE TABLE knows_explain_distinct_star_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_explain_distinct_star ON people_explain_distinct_star; \
             CREATE EDGE LABEL knows_explain_distinct_star ON knows_explain_distinct_star_edges SOURCE person_explain_distinct_star TARGET person_explain_distinct_star; \
             INSERT INTO people_explain_distinct_star VALUES \
                (1, 0), (2, 30), (3, 10), (4, 40), (5, 50); \
             INSERT INTO knows_explain_distinct_star_edges VALUES \
                (1, 2), (1, 3), (1, 4), \
                (2, 3), (2, 4), (2, 5)",
        )
        .expect("seed distinct explain star graph");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN ANALYZE MATCH (a:person_explain_distinct_star)-[:knows_explain_distinct_star]->(b:person_explain_distinct_star), \
                   (a)-[:knows_explain_distinct_star]->(c:person_explain_distinct_star) \
             WHERE b.number > 20 AND b.id <> c.id RETURN count(DISTINCT c.id)",
        )
        .expect("execute explain analyze distinct shared anchor star");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Access [PipelineMatch 0 pattern 0]")
                && line.contains("actual_rows=")
        }),
        "explain analyze distinct star lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Access [PipelineMatch 0 pattern 1]")
                && line.contains("actual_rows=")
        }),
        "explain analyze distinct star lines: {lines:?}"
    );
}

#[test]
fn explain_graph_procedure_includes_projection_lines() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let query_engine: &dyn crate::engine::api::QueryEngine = &engine;

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN CALL graph.pageRank() YIELD nodeId, score RETURN nodeId, score",
        )
        .expect("execute explain graph procedure");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    let summary_json = query_engine
        .execute_explain_graph_summary_json(
            &session,
            "CALL graph.pageRank() YIELD nodeId, score RETURN nodeId, score",
            false,
        )
        .expect("procedure summary json");
    let detail_json = query_engine
        .execute_explain_graph_detail_json(
            &session,
            "CALL graph.pageRank() YIELD nodeId, score RETURN nodeId, score",
            false,
        )
        .expect("procedure detail json");

    assert!(
        lines
            .iter()
            .any(|line| line.contains("Graph Projection [ProcedureCall 0]")),
        "explain lines: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("procedure=graph.pageRank")),
        "explain lines: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("source=Some(ProjectionStore)")),
        "explain lines: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("projection=cypher.native.graph")),
        "explain lines: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("node_count=unknown") && line.contains("edge_count=unknown")),
        "explain lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Procedure Summary:")
                && line.contains("total_procedures=1")
                && line.contains("projection_store_source=1")
                && line.contains("row_fallback_procedures=1")
                && line.contains("weighted_projection=0")
                && line.contains("source=inferred")
        }),
        "explain lines: {lines:?}"
    );
    assert_eq!(summary_json["total_procedures"], 1);
    assert_eq!(summary_json["procedure_projection_store_source"], 1);
    assert_eq!(summary_json["row_fallback_procedures"], 1);
    assert_eq!(summary_json["weighted_projection_procedures"], 0);
    assert_eq!(detail_json["summary"]["total_procedures"], 1);
    assert_eq!(detail_json["summary"]["procedure_projection_store_source"], 1);
    assert_eq!(detail_json["summary"]["row_fallback_procedures"], 1);
    assert_eq!(detail_json["summary"]["weighted_projection_procedures"], 0);
}

#[test]
fn fk_backed_edge_label_rejects_cypher_relationship_create() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE employees (id INT NOT NULL, name TEXT); \
             CREATE TABLE tickets (id INT NOT NULL, assigned_to INT, title TEXT); \
             INSERT INTO employees VALUES (10, 'alice'); \
             INSERT INTO tickets VALUES (1, NULL, 'login'); \
             CREATE NODE LABEL Employee ON employees; \
             CREATE NODE LABEL Ticket ON tickets; \
             CREATE EDGE LABEL handled_by ON tickets SOURCE Ticket KEY (id) TARGET Employee KEY (assigned_to)",
        )
        .expect("create FK-backed edge label");

    let err = engine
        .execute_sql(
            &session,
            "MATCH (t:Ticket {id: 1}), (e:Employee {id: 10}) CREATE (t)-[:handled_by]->(e) RETURN 1",
        )
        .expect_err("FK-backed edge labels should be read/traversal-only");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        err.to_string().contains("FK-backed edge label"),
        "unexpected error: {err}"
    );
}

#[test]
fn multiple_fk_backed_edge_labels_can_share_one_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE employees (id INT NOT NULL, name TEXT); \
             CREATE TABLE tickets (id INT NOT NULL, assigned_to INT, reporter_id INT, title TEXT); \
             INSERT INTO employees VALUES (10, 'alice'), (20, 'bob'); \
             INSERT INTO tickets VALUES (1, 10, 20, 'login'), (2, 20, 10, 'billing'); \
             CREATE NODE LABEL Employee ON employees; \
             CREATE NODE LABEL Ticket ON tickets; \
             CREATE EDGE LABEL handled_by ON tickets SOURCE Ticket KEY (id) TARGET Employee KEY (assigned_to); \
             CREATE EDGE LABEL reported_by ON tickets SOURCE Ticket KEY (id) TARGET Employee KEY (reporter_id)",
        )
        .expect("create multiple FK-backed labels on one table");

    let assignee = query_rows(
        &engine,
        &session,
        "SELECT employee_id FROM graph_neighbors('handled_by', 1) AS g(employee_id)",
    );
    assert_eq!(assignee.len(), 1);
    assert_eq!(assignee[0].values[0], Value::BigInt(10));

    let reporter = query_rows(
        &engine,
        &session,
        "SELECT employee_id FROM graph_neighbors('reported_by', 1) AS g(employee_id)",
    );
    assert_eq!(reporter.len(), 1);
    assert_eq!(reporter[0].values[0], Value::BigInt(20));

    let second_assignee = query_rows(
        &engine,
        &session,
        "SELECT employee_id FROM graph_neighbors('handled_by', 2) AS g(employee_id)",
    );
    assert_eq!(second_assignee.len(), 1);
    assert_eq!(second_assignee[0].values[0], Value::BigInt(20));

    let cypher = query_rows(
        &engine,
        &session,
        "MATCH (t:Ticket {id: 1})-[:reported_by]->(e:Employee) RETURN e.id",
    );
    assert_eq!(cypher.len(), 1);
    assert_eq!(cypher[0].values[0], Value::Int(20));

    let undirected = query_rows(
        &engine,
        &session,
        "MATCH (e:Employee {id: 20})-[:reported_by]-(t:Ticket) RETURN t.id",
    );
    assert_eq!(undirected.len(), 1);
    assert_eq!(undirected[0].values[0], Value::Int(1));

    let shortest = query_rows(
        &engine,
        &session,
        "MATCH shortestPath((t:Ticket {id: 1})-[:reported_by*..1]->(e:Employee {id: 20})) RETURN 1",
    );
    assert_eq!(shortest.len(), 1);
    assert_eq!(shortest[0].values[0], Value::BigInt(1));
}

#[test]
fn cypher_named_variable_length_path_exposes_materialized_elements() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id INT NOT NULL, target_id INT NOT NULL, since INT); \
             INSERT INTO people VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO knows_edges VALUES (1, 2, 2020), (2, 3, 2021); \
             CREATE NODE LABEL Person ON people; \
             CREATE EDGE LABEL KNOWS ON knows_edges SOURCE Person TARGET Person",
        )
        .expect("setup named variable-length path data");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH p = (a:Person {id: 1})-[:KNOWS*2]->(b:Person {id: 3}) \
         RETURN length(p), nodes(p), relationships(p), p",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(2));
    assert_eq!(
        rows[0].values[1],
        Value::Array(vec![
            Value::Text("(:Person {name: 'alice'})".to_owned()),
            Value::Text("(:Person {name: 'bob'})".to_owned()),
            Value::Text("(:Person {name: 'carol'})".to_owned()),
        ])
    );
    assert_eq!(
        rows[0].values[2],
        Value::Array(vec![
            Value::Text("[:KNOWS {since: 2020}]".to_owned()),
            Value::Text("[:KNOWS {since: 2021}]".to_owned()),
        ])
    );
    assert_eq!(
        rows[0].values[3],
        Value::Text(
            "(:Person {name: 'alice'})-[:KNOWS {since: 2020}]->(:Person {name: 'bob'})-[:KNOWS {since: 2021}]->(:Person {name: 'carol'})".to_owned()
        )
    );
}

#[test]
fn cypher_relationship_type_alternatives_match_and_traverse() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE TABLE likes_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             INSERT INTO people VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO knows_edges VALUES (1, 2); \
             INSERT INTO likes_edges VALUES (1, 3), (2, 3); \
             CREATE NODE LABEL Person ON people; \
             CREATE EDGE LABEL KNOWS ON knows_edges SOURCE Person TARGET Person; \
             CREATE EDGE LABEL LIKES ON likes_edges SOURCE Person TARGET Person",
        )
        .expect("setup relationship type alternatives graph data");

    let one_hop = query_rows(
        &engine,
        &session,
        "MATCH (a:Person {id: 1})-[:KNOWS|LIKES]->(b:Person) \
         RETURN b.name ORDER BY b.name",
    );
    assert_eq!(one_hop.len(), 2);
    assert_eq!(one_hop[0].values[0], Value::Text("bob".to_owned()));
    assert_eq!(one_hop[1].values[0], Value::Text("carol".to_owned()));

    let varlen = query_rows(
        &engine,
        &session,
        "MATCH p = (a:Person {id: 1})-[:KNOWS|LIKES*2]->(b:Person {id: 3}) \
         RETURN length(p), relationships(p)",
    );
    assert_eq!(varlen.len(), 1);
    assert_eq!(varlen[0].values[0], Value::BigInt(2));
    assert_eq!(
        varlen[0].values[1],
        Value::Array(vec![
            Value::Text("[:KNOWS]".to_owned()),
            Value::Text("[:likes]".to_owned()),
        ])
    );
}

#[test]
fn cypher_list_comprehension_runs_in_native_pipeline() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id INT NOT NULL, name TEXT); \
             INSERT INTO people VALUES (1, 'alice'); \
             CREATE NODE LABEL Person ON people",
        )
        .expect("setup list comprehension graph data");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (n:Person {id: 1}) RETURN [x IN [1, 2, 3] WHERE x > 1 | x]",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values[0],
        Value::Array(vec![Value::BigInt(2), Value::BigInt(3)])
    );
}

#[test]
fn cypher_quantifiers_run_in_native_pipeline() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id INT NOT NULL, name TEXT); \
             INSERT INTO people VALUES (1, 'alice'); \
             CREATE NODE LABEL Person ON people",
        )
        .expect("setup quantifier graph data");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (n:Person {id: 1}) \
         RETURN any(x IN [1, 2, 3] WHERE x = 2), \
                all(x IN [1, 2, 3] WHERE x > 0), \
                none(x IN [1, 2, 3] WHERE x < 0), \
                single(x IN [1, 2, 3] WHERE x = 2)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Boolean(true));
    assert_eq!(rows[0].values[1], Value::Boolean(true));
    assert_eq!(rows[0].values[2], Value::Boolean(true));
    assert_eq!(rows[0].values[3], Value::Boolean(true));
}

#[test]
fn cypher_graph_element_introspection_functions_use_native_bindings() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id INT NOT NULL, target_id INT NOT NULL, since INT); \
             INSERT INTO people VALUES (1, 'alice'), (2, 'bob'); \
             INSERT INTO knows_edges VALUES (1, 2, 2020); \
             CREATE NODE LABEL Person ON people; \
             CREATE EDGE LABEL KNOWS ON knows_edges SOURCE Person TARGET Person",
        )
        .expect("setup graph element function data");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:Person {id: 1})-[r:KNOWS]->(b:Person {id: 2}) \
         RETURN id(a), elementId(a), labels(a), type(r), properties(a), properties(r), keys(a), keys(r), startNode(r), endNode(r)",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(1));
    assert_eq!(rows[0].values[1], Value::Text("1".to_owned()));
    assert_eq!(
        rows[0].values[2],
        Value::Array(vec![Value::Text("Person".to_owned())])
    );
    assert_eq!(rows[0].values[3], Value::Text("KNOWS".to_owned()));
    assert_eq!(
        rows[0].values[4],
        Value::Jsonb(serde_json::json!({"name": "alice"}))
    );
    assert_eq!(
        rows[0].values[5],
        Value::Jsonb(serde_json::json!({"since": 2020}))
    );
    assert_eq!(
        rows[0].values[6],
        Value::Array(vec![Value::Text("name".to_owned())])
    );
    assert_eq!(
        rows[0].values[7],
        Value::Array(vec![Value::Text("since".to_owned())])
    );
    assert_eq!(rows[0].values[8], Value::Int(1));
    assert_eq!(rows[0].values[9], Value::Int(2));
}

#[test]
fn cypher_properties_survive_binding_compaction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id INT NOT NULL, name TEXT, born INT); \
             INSERT INTO people VALUES (1, 'alice', 1984), (2, 'bob', 1990); \
             CREATE NODE LABEL Person ON people",
        )
        .expect("setup graph property compaction data");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (n:Person) \
         RETURN properties(n) \
         ORDER BY n.id \
         LIMIT 1",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values[0],
        Value::Jsonb(serde_json::json!({"name": "alice", "born": 1984}))
    );
}

#[test]
fn cypher_map_projection_returns_native_graph_properties() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id INT NOT NULL, name TEXT, born INT); \
             INSERT INTO people VALUES (1, 'alice', 1984); \
             CREATE NODE LABEL Person ON people",
        )
        .expect("setup map projection graph data");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (n:Person {id: 1}) RETURN n {.name, born: n.born, .*}, keys(n {.name})",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values[0],
        Value::Jsonb(serde_json::json!({"name": "alice", "born": 1984}))
    );
    assert_eq!(
        rows[0].values[1],
        Value::Array(vec![Value::Text("name".to_owned())])
    );
}

// ===================================================================
// DROP NODE LABEL
// ===================================================================

#[test]
fn drop_node_label() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT); \
             CREATE NODE LABEL person ON persons",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "DROP NODE LABEL person")
        .expect("drop node label");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "DROP NODE LABEL".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn drop_node_label_table_still_exists() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT); \
             INSERT INTO persons VALUES (1, 'alice'); \
             CREATE NODE LABEL person ON persons; \
             DROP NODE LABEL person",
        )
        .expect("setup and drop label");

    // The underlying table should still be usable after dropping the label.
    let results = engine
        .execute_sql(&session, "SELECT name FROM persons")
        .expect("select after label drop");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "name".to_owned(),
                data_type: aiondb_core::DataType::Text,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "alice".to_owned()
            )])],
        }]
    );
}

// ===================================================================
// DROP EDGE LABEL
// ===================================================================

#[test]
fn drop_edge_label() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person ON persons; \
             CREATE EDGE LABEL knows ON knows_edges SOURCE person TARGET person",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "DROP EDGE LABEL knows")
        .expect("drop edge label");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "DROP EDGE LABEL".to_owned(),
            rows_affected: 0,
        }]
    );
}

// ===================================================================
// Error cases
// ===================================================================

#[test]
fn error_create_node_label_table_not_found() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "CREATE NODE LABEL person ON nonexistent")
        .expect_err("should fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("does not exist"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn error_create_node_label_duplicate() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT); \
             CREATE NODE LABEL person ON persons",
        )
        .expect("setup");

    let err = engine
        .execute_sql(&session, "CREATE NODE LABEL person ON persons")
        .expect_err("duplicate should fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("already exists"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn error_create_edge_label_source_label_not_found() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE edges (source_id INT NOT NULL, target_id INT NOT NULL)",
        )
        .expect("create table");

    let err = engine
        .execute_sql(
            &session,
            "CREATE EDGE LABEL knows ON edges SOURCE nonexistent TARGET nonexistent",
        )
        .expect_err("should fail with missing source label");
    let msg = format!("{err}");
    assert!(
        msg.contains("does not exist"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn error_drop_node_label_not_found() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "DROP NODE LABEL nonexistent")
        .expect_err("should fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("does not exist"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn error_drop_edge_label_not_found() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "DROP EDGE LABEL nonexistent")
        .expect_err("should fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("does not exist"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn error_create_edge_label_duplicate() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person ON persons; \
             CREATE EDGE LABEL knows ON knows_edges SOURCE person TARGET person",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "CREATE EDGE LABEL knows ON knows_edges SOURCE person TARGET person",
        )
        .expect_err("duplicate should fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("already exists"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn error_create_node_label_requires_id_first_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE events (slug TEXT NOT NULL, id INT NOT NULL)",
        )
        .expect("setup");

    let err = engine
        .execute_sql(&session, "CREATE NODE LABEL event ON events")
        .expect_err("node table without first-column id should fail");

    assert_eq!(err.sqlstate(), SqlState::InvalidTableDefinition);
    assert!(format!("{err}").contains("\"id\" as its first column"));
}

#[test]
fn error_create_edge_label_requires_canonical_endpoint_columns() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT); \
             CREATE TABLE works_at_edges (person_id INT NOT NULL, company_id INT NOT NULL); \
             CREATE NODE LABEL person ON persons",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "CREATE EDGE LABEL works_at ON works_at_edges SOURCE person TARGET person",
        )
        .expect_err("edge table without canonical endpoints should fail");

    assert_eq!(err.sqlstate(), SqlState::InvalidTableDefinition);
    assert!(format!("{err}").contains("\"source_id\" and \"target_id\""));
}

#[test]
fn error_create_second_node_label_on_same_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT); \
             CREATE NODE LABEL person ON persons",
        )
        .expect("setup");

    let err = engine
        .execute_sql(&session, "CREATE NODE LABEL person_v2 ON persons")
        .expect_err("same backing table should not accept a second node label");

    assert_eq!(err.sqlstate(), SqlState::DuplicateObject);
    assert!(format!("{err}").contains("already registered as node label"));
}

#[test]
fn error_drop_node_label_with_dependent_edge_label() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person ON persons; \
             CREATE EDGE LABEL knows ON knows_edges SOURCE person TARGET person",
        )
        .expect("setup");

    // Dropping the node label should fail because the edge label depends on it.
    let err = engine
        .execute_sql(&session, "DROP NODE LABEL person")
        .expect_err("should fail with dependent edge label");
    let msg = format!("{err}");
    assert!(
        msg.contains("depends on it") || msg.contains("2BP01"),
        "error should mention dependency: {msg}"
    );
}

#[test]
fn drop_node_label_after_dropping_dependent_edge_label() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person ON persons; \
             CREATE EDGE LABEL knows ON knows_edges SOURCE person TARGET person",
        )
        .expect("setup");

    // First drop the edge label, then the node label should succeed.
    engine
        .execute_sql(&session, "DROP EDGE LABEL knows")
        .expect("drop edge label");
    engine
        .execute_sql(&session, "DROP NODE LABEL person")
        .expect("drop node label after edge removed");
}

// ===================================================================
// Full lifecycle: create labels, use tables, drop labels
// ===================================================================

#[test]
fn full_graph_lifecycle() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Create backing tables
    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id INT NOT NULL, target_id INT NOT NULL)",
        )
        .expect("create tables");

    // Create labels
    engine
        .execute_sql(&session, "CREATE NODE LABEL person ON persons")
        .expect("create node label");
    engine
        .execute_sql(
            &session,
            "CREATE EDGE LABEL knows ON knows_edges SOURCE person TARGET person",
        )
        .expect("create edge label");

    // Insert data into backing tables (labels are metadata, data goes in tables)
    engine
        .execute_sql(
            &session,
            "INSERT INTO persons VALUES (1, 'alice'), (2, 'bob'); \
             INSERT INTO knows_edges VALUES (1, 2)",
        )
        .expect("insert data");

    // Query backing tables
    let results = engine
        .execute_sql(
            &session,
            "SELECT persons.name FROM persons \
             INNER JOIN knows_edges ON persons.id = knows_edges.source_id \
             ORDER BY persons.name",
        )
        .expect("query graph data via SQL");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "name".to_owned(),
                data_type: aiondb_core::DataType::Text,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "alice".to_owned()
            )])],
        }]
    );

    // Drop labels (tables remain intact)
    engine
        .execute_sql(&session, "DROP EDGE LABEL knows")
        .expect("drop edge label");
    engine
        .execute_sql(&session, "DROP NODE LABEL person")
        .expect("drop node label");

    // Tables still usable after dropping labels
    let results = engine
        .execute_sql(&session, "SELECT COUNT(*) FROM persons")
        .expect("count after label drop");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "count".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(2)])],
        }]
    );
}

#[test]
fn cypher_create_auto_label_uses_bound_property_value_for_schema_inference() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id BIGINT NOT NULL, score BIGINT); \
             INSERT INTO people VALUES (1, 7); \
             CREATE NODE LABEL Person ON people",
        )
        .expect("setup");

    engine
        .execute_sql(
            &session,
            "MATCH (m:Person {id: 1}) CREATE (n:Derived {copied: m.score}) RETURN 1",
        )
        .expect("create derived node from bound property");

    let results = engine
        .execute_sql(&session, "SELECT copied FROM derived")
        .expect("select derived rows");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "copied".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(7)])],
        }]
    );
}

#[test]
fn cypher_create_auto_label_uses_search_path_schema_for_backing_objects() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             SET search_path TO analytics, public; \
             CREATE TABLE people (id BIGINT NOT NULL, score BIGINT); \
             INSERT INTO people VALUES (1, 7); \
             CREATE NODE LABEL Person ON people",
        )
        .expect("setup analytics graph schema");

    engine
        .execute_sql(
            &session,
            "MATCH (m:Person {id: 1}) CREATE (n:Derived {copied: m.score}) RETURN 1",
        )
        .expect("auto-create derived label in analytics schema");

    engine
        .execute_sql(&session, "SET search_path TO public")
        .expect("move search_path away from analytics");
    engine
        .execute_sql(&session, "CREATE (n:Derived {copied: 9}) RETURN 1")
        .expect("existing label insert should keep using analytics-owned sequence");

    let analytics_rows = query_rows(
        &engine,
        &session,
        "SELECT id, copied FROM analytics.derived ORDER BY id",
    );
    assert_eq!(
        analytics_rows,
        vec![
            Row::new(vec![Value::BigInt(1), Value::BigInt(7)]),
            Row::new(vec![Value::BigInt(2), Value::BigInt(9)]),
        ]
    );

    let public_rows = engine
        .execute_sql(&session, "SELECT id, copied FROM public.derived")
        .expect_err("public schema should not own auto-created derived table");
    assert_eq!(
        public_rows.sqlstate(),
        aiondb_core::SqlState::UndefinedTable
    );
}

#[test]
fn cypher_create_auto_label_uses_default_user_search_path_schema_when_present() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA alice; \
             CREATE (:Person {name: 'alice'}) RETURN 1",
        )
        .expect("auto-create graph label in current user schema");

    let alice_rows = query_rows(&engine, &session, "SELECT id, name FROM alice.person");
    assert_eq!(alice_rows.len(), 1);
    assert_eq!(alice_rows[0].values[1], Value::Text("alice".to_owned()));
    assert!(!matches!(alice_rows[0].values[0], Value::Null));

    let public_rows = engine
        .execute_sql(&session, "SELECT id, name FROM public.person")
        .expect_err("public schema should not own auto-created person table");
    assert_eq!(
        public_rows.sqlstate(),
        aiondb_core::SqlState::UndefinedTable
    );
}

#[test]
fn cypher_create_auto_edge_uses_search_path_schema_for_backing_objects() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
             "CREATE SCHEMA analytics; \
             SET search_path TO analytics, public; \
             CREATE (:Person {id: 1, name: 'alice'}) RETURN 1; \
             CREATE (:Person {id: 2, name: 'bob'}) RETURN 1; \
             MATCH (a:Person {id: 1}), (b:Person {id: 2}) CREATE (a:Person)-[:KNOWS {since: 2024}]->(b:Person) RETURN 1",
        )
        .expect("create graph objects in analytics schema");

    engine
        .execute_sql(&session, "SET search_path TO public")
        .expect("move search_path away from analytics");
    engine
        .execute_sql(
            &session,
            "MATCH (a:Person {id: 1}), (b:Person {id: 2}) CREATE (a:Person)-[:KNOWS {since: 2025}]->(b:Person) RETURN 1",
        )
        .expect("existing edge label should keep using analytics backing table");

    let edge_rows = query_rows(
        &engine,
        &session,
        "SELECT source_id, target_id, since FROM analytics.knows ORDER BY since",
    );
    assert_eq!(edge_rows.len(), 2);
    assert_eq!(edge_rows[0].values[2], Value::BigInt(2024));
    assert_eq!(edge_rows[1].values[2], Value::BigInt(2025));
    assert!(!matches!(edge_rows[0].values[0], Value::Null));
    assert!(!matches!(edge_rows[0].values[1], Value::Null));
    assert!(!matches!(edge_rows[1].values[0], Value::Null));
    assert!(!matches!(edge_rows[1].values[1], Value::Null));

    let public_rows = engine
        .execute_sql(
            &session,
            "SELECT source_id, target_id, since FROM public.knows",
        )
        .expect_err("public schema should not own auto-created knows table");
    assert_eq!(
        public_rows.sqlstate(),
        aiondb_core::SqlState::UndefinedTable
    );
}

#[test]
fn cypher_create_inline_nodes_populates_edge_endpoints() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             CREATE TABLE others (id BIGINT NOT NULL, name TEXT); \
             CREATE TABLE knows (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL, since BIGINT); \
             CREATE NODE LABEL Person ON people; \
             CREATE NODE LABEL OtherPerson ON others; \
             CREATE EDGE LABEL KNOWS ON knows SOURCE Person TARGET OtherPerson; \
             CREATE (:Person {id: 1, name: 'alice'})-[:KNOWS {since: 2024}]->(:OtherPerson {id: 2, name: 'bob'}) RETURN 1",
        )
        .expect("create inline graph pattern");

    let edge_rows = query_rows(
        &engine,
        &session,
        "SELECT source_id, target_id, since FROM knows",
    );
    assert_eq!(edge_rows.len(), 1);
    assert_eq!(edge_rows[0].values[2], Value::BigInt(2024));
    assert!(!matches!(edge_rows[0].values[0], Value::Null));
    assert!(!matches!(edge_rows[0].values[1], Value::Null));
}

#[test]
fn cypher_create_inline_same_label_auto_creates_nodes_and_edge() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE (:Person {name: 'alice'})-[:KNOWS {since: 2024}]->(:Person {name: 'bob'}) RETURN 1",
        )
        .expect("create fully auto-created inline graph pattern");

    let node_rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM person ORDER BY name",
    );
    assert_eq!(node_rows.len(), 2);
    assert_eq!(node_rows[0].values[1], Value::Text("alice".to_owned()));
    assert_eq!(node_rows[1].values[1], Value::Text("bob".to_owned()));
    assert!(!matches!(node_rows[0].values[0], Value::Null));
    assert!(!matches!(node_rows[1].values[0], Value::Null));

    let edge_rows = query_rows(
        &engine,
        &session,
        "SELECT source_id, target_id, since FROM knows",
    );
    assert_eq!(edge_rows.len(), 1);
    assert_eq!(edge_rows[0].values[2], Value::BigInt(2024));
    assert!(!matches!(edge_rows[0].values[0], Value::Null));
    assert!(!matches!(edge_rows[0].values[1], Value::Null));
}

#[test]
fn cypher_merge_read_committed_rechecks_after_waiting_writer() {
    let engine = std::sync::Arc::new(EngineBuilder::for_testing().build().unwrap());
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(
            &sa,
            "CREATE TABLE merge_people_atomic (id BIGINT NOT NULL, name TEXT); \
             CREATE NODE LABEL PersonMergeAtomic ON merge_people_atomic",
        )
        .expect("setup merge label");

    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .execute_sql(
            &sa,
            "MERGE (n:PersonMergeAtomic {id: 1}) \
             ON CREATE SET n.name = 'alice' \
             RETURN n.id",
        )
        .expect("A merge creates");

    engine
        .begin_transaction(&sb, IsolationLevel::ReadCommitted)
        .expect("begin B");

    let (sender, receiver) = std::sync::mpsc::channel();
    let engine_b = engine.clone();
    let sb_thread = sb.clone();
    let worker = std::thread::spawn(move || {
        let result = engine_b
            .execute_sql(
                &sb_thread,
                "MERGE (n:PersonMergeAtomic {id: 1}) \
                 ON CREATE SET n.name = 'duplicate' \
                 ON MATCH SET n.name = 'seen' \
                 RETURN n.id",
            )
            .map(|_| ());
        sender.send(result).expect("send B result");
    });

    std::thread::sleep(std::time::Duration::from_millis(100));
    engine.commit_transaction(&sa).expect("commit A");

    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("B merge should unblock")
        .expect("B merge should match committed row");
    engine.commit_transaction(&sb).expect("commit B");
    worker.join().expect("join B");

    let rows = query_rows(
        engine.as_ref(),
        &sa,
        "SELECT id, name FROM merge_people_atomic ORDER BY id",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(1));
    assert_eq!(rows[0].values[1], Value::Text("seen".to_owned()));
}

#[test]
fn cypher_call_subquery_can_return_correlated_match_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE (:Person {name: 'alice'})-[:KNOWS]->(:Person {name: 'bob'})",
        )
        .expect("seed graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (n:Person {name: 'alice'}) \
         CALL { WITH n MATCH (n)-[:KNOWS]->(m) RETURN m.name AS friend } \
         RETURN friend",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("bob".to_owned()));
}

#[test]
fn cypher_call_subquery_union_all_returns_combined_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CALL { RETURN 1 AS x UNION ALL RETURN 2 AS x } RETURN x ORDER BY x",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::BigInt(1));
    assert_eq!(rows[1].values[0], Value::BigInt(2));
}

#[test]
fn cypher_call_subquery_union_deduplicates_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CALL { RETURN 1 AS x UNION RETURN 1 AS x } RETURN x",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(1));
}

#[test]
fn cypher_call_subquery_union_all_can_reference_outer_binding() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE (:Person {name: 'alice'}); CREATE (:Person {name: 'bob'})",
        )
        .expect("seed graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (n:Person) \
         CALL { WITH n RETURN n.name AS x UNION ALL WITH n RETURN n.name AS x } \
         RETURN x ORDER BY x",
    );

    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values[0], Value::Text("alice".to_owned()));
    assert_eq!(rows[1].values[0], Value::Text("alice".to_owned()));
    assert_eq!(rows[2].values[0], Value::Text("bob".to_owned()));
    assert_eq!(rows[3].values[0], Value::Text("bob".to_owned()));
}

#[test]
fn cypher_call_subquery_union_can_deduplicate_outer_binding_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE (:Person {name: 'alice'}); CREATE (:Person {name: 'bob'})",
        )
        .expect("seed graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (n:Person) \
         CALL { WITH n RETURN n.name AS x UNION WITH n RETURN n.name AS x } \
         RETURN x ORDER BY x",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("alice".to_owned()));
    assert_eq!(rows[1].values[0], Value::Text("bob".to_owned()));
}

#[test]
fn cypher_call_subquery_union_can_mix_correlated_match_and_passthrough() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE (:Person {name: 'alice'})-[:KNOWS]->(:Person {name: 'bob'})",
        )
        .expect("seed graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (n:Person {name: 'alice'}) \
         CALL { WITH n MATCH (n)-[:KNOWS]->(m) RETURN m.name AS friend \
                UNION \
                WITH n RETURN n.name AS friend } \
         RETURN friend ORDER BY friend",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("alice".to_owned()));
    assert_eq!(rows[1].values[0], Value::Text("bob".to_owned()));
}

#[test]
fn cypher_exists_subquery_filters_with_correlated_match() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE (:Person {name: 'alice'})-[:KNOWS]->(:Person {name: 'bob'}); \
             CREATE (:Person {name: 'carol'})",
        )
        .expect("seed graph");

    let exists_rows = query_rows(
        &engine,
        &session,
        "MATCH (n:Person) \
         WHERE EXISTS { MATCH (n)-[:KNOWS]->(m) } \
         RETURN n.name ORDER BY n.name",
    );
    assert_eq!(exists_rows.len(), 1);
    assert_eq!(exists_rows[0].values[0], Value::Text("alice".to_owned()));

    let not_exists_rows = query_rows(
        &engine,
        &session,
        "MATCH (n:Person) \
         WHERE NOT EXISTS { MATCH (n)-[:KNOWS]->(m) } \
         RETURN n.name ORDER BY n.name",
    );
    assert_eq!(
        not_exists_rows
            .into_iter()
            .map(|row| row.values[0].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Text("bob".to_owned()),
            Value::Text("carol".to_owned())
        ],
    );
}

#[test]
fn cypher_exists_subquery_can_use_union_branches() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE (:Person {name: 'alice'})-[:KNOWS]->(:Person {name: 'bob'}); \
             CREATE (:Person {name: 'carol'}); \
             CREATE (:Person {name: 'dave'})",
        )
        .expect("seed graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (n:Person) \
         WHERE EXISTS { \
           WITH n MATCH (n)-[:KNOWS]->(m) RETURN m.name AS hit \
           UNION \
           WITH n WHERE n.name = 'carol' RETURN n.name AS hit \
         } \
         RETURN n.name ORDER BY n.name",
    );

    assert_eq!(
        rows.into_iter()
            .map(|row| row.values[0].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Text("alice".to_owned()),
            Value::Text("carol".to_owned())
        ]
    );
}

#[test]
fn cypher_pattern_comprehension_returns_correlated_projection_list() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE (:Person {name: 'alice'})-[:KNOWS]->(:Person {name: 'bob'}); \
             MATCH (a:Person {name: 'alice'}) CREATE (a)-[:KNOWS]->(:Person {name: 'carol'})",
        )
        .expect("seed graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (n:Person {name: 'alice'}) \
         RETURN [(n)-[:KNOWS]->(m) WHERE m.name <> 'carol' | m.name] AS friends",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values[0],
        Value::Array(vec![Value::Text("bob".to_owned())])
    );
}

#[test]
fn cypher_nested_foreach_updates_each_matched_row_once() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE (:Person {name: 'alice'}), (:Person {name: 'bob'})",
        )
        .expect("seed graph");

    engine
        .execute_sql(
            &session,
            "MATCH (n:Person) \
             FOREACH (x IN [1,2] | \
               FOREACH (y IN [10,20] | \
                 SET n.name = 'nested')) \
             RETURN 1",
        )
        .expect("nested foreach update");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (n:Person) RETURN n.name ORDER BY n.name",
    );
    assert_eq!(
        rows.into_iter().map(|row| row.values[0].clone()).collect::<Vec<_>>(),
        vec![
            Value::Text("nested".to_owned()),
            Value::Text("nested".to_owned()),
        ],
    );
}

#[test]
fn cypher_reused_target_variable_must_match_edge_endpoint_marker() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE (:Person {name: 'alice'})-[:KNOWS]->(:Person {name: 'bob'}); \
             CREATE (:Person {name: 'carol'})-[:KNOWS]->(:Person {name: 'dave'})",
        )
        .expect("seed graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:Person {name: 'alice'})-[:KNOWS]->(b), \
               (c:Person {name: 'carol'})-[:KNOWS]->(b) \
         RETURN b.name",
    );

    assert!(rows.is_empty());
}

#[test]
fn cypher_return_groups_by_non_aggregate_projection_after_traversal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE (:Person {name: 'alice'})-[:KNOWS]->(:Person {kind: 'dev'}); \
             MATCH (a:Person {name: 'alice'}) CREATE (a)-[:KNOWS]->(:Person {kind: 'dev'}); \
             MATCH (a:Person {name: 'alice'}) CREATE (a)-[:KNOWS]->(:Person {kind: 'ops'})",
        )
        .expect("seed graph");

    let mut rows = query_rows(
        &engine,
        &session,
        "MATCH (a:Person {name: 'alice'})-[:KNOWS]->(b) \
         RETURN b.kind, count(b)",
    );
    rows.sort_by(|left, right| match (&left.values[0], &right.values[0]) {
        (Value::Text(left), Value::Text(right)) => left.cmp(right),
        _ => std::cmp::Ordering::Equal,
    });

    assert_eq!(
        rows,
        vec![
            Row::new(vec![Value::Text("dev".to_owned()), Value::BigInt(2)]),
            Row::new(vec![Value::Text("ops".to_owned()), Value::BigInt(1)]),
        ],
    );
}

#[test]
fn cypher_order_by_after_aggregation_sorts_by_alias_and_key() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE (:Person {name: 'alice'})-[:KNOWS]->(:Person {kind: 'dev'}); \
             MATCH (a:Person {name: 'alice'}) CREATE (a)-[:KNOWS]->(:Person {kind: 'dev'}); \
             MATCH (a:Person {name: 'alice'}) CREATE (a)-[:KNOWS]->(:Person {kind: 'ops'})",
        )
        .expect("seed graph");

    // ORDER BY an aggregation alias, descending: dev(2) before ops(1).
    let by_count_desc = query_rows(
        &engine,
        &session,
        "MATCH (a:Person {name: 'alice'})-[:KNOWS]->(b) \
         RETURN b.kind AS kind, count(*) AS c ORDER BY c DESC",
    );
    assert_eq!(
        by_count_desc,
        vec![
            Row::new(vec![Value::Text("dev".to_owned()), Value::BigInt(2)]),
            Row::new(vec![Value::Text("ops".to_owned()), Value::BigInt(1)]),
        ],
    );

    // Ascending must reverse it — proves the sort is real, not incidental.
    let by_count_asc = query_rows(
        &engine,
        &session,
        "MATCH (a:Person {name: 'alice'})-[:KNOWS]->(b) \
         RETURN b.kind AS kind, count(*) AS c ORDER BY c ASC",
    );
    assert_eq!(
        by_count_asc,
        vec![
            Row::new(vec![Value::Text("ops".to_owned()), Value::BigInt(1)]),
            Row::new(vec![Value::Text("dev".to_owned()), Value::BigInt(2)]),
        ],
    );

    // ORDER BY a grouping-key alias also works after aggregation.
    let by_key_asc = query_rows(
        &engine,
        &session,
        "MATCH (a:Person {name: 'alice'})-[:KNOWS]->(b) \
         RETURN b.kind AS kind, count(*) AS c ORDER BY kind ASC",
    );
    assert_eq!(
        by_key_asc,
        vec![
            Row::new(vec![Value::Text("dev".to_owned()), Value::BigInt(2)]),
            Row::new(vec![Value::Text("ops".to_owned()), Value::BigInt(1)]),
        ],
    );
}

#[test]
fn cypher_where_on_traversal_target_filters_target_node_property() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE (:Person {name: 'alice'})-[:KNOWS]->(:Person {kind: 'dev'}); \
             MATCH (a:Person {name: 'alice'}) CREATE (a)-[:KNOWS]->(:Person {kind: 'ops'}); \
             MATCH (a:Person {name: 'alice'}) CREATE (a)-[:KNOWS]->(:Person {kind: 'dev'})",
        )
        .expect("seed graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:Person {name: 'alice'})-[:KNOWS]->(b) \
         WHERE b.kind = 'dev' \
         RETURN b.kind",
    );

    assert_eq!(
        rows.into_iter()
            .map(|row| row.values[0].clone())
            .collect::<Vec<_>>(),
        vec![Value::Text("dev".to_owned()), Value::Text("dev".to_owned())],
    );
}

#[test]
fn cypher_one_hop_target_id_limit_uses_prefix_result_semantics() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_limit (id INT NOT NULL, kind TEXT); \
             CREATE TABLE knows_limit_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_limit ON people_limit; \
             CREATE EDGE LABEL knows_limit ON knows_limit_edges SOURCE person_limit TARGET person_limit; \
             INSERT INTO people_limit VALUES (1, 'src'), (2, 'dev'), (3, 'ops'), (4, 'qa'); \
             INSERT INTO knows_limit_edges VALUES (1, 2), (1, 3), (1, 4)",
        )
        .expect("seed graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (:person_limit)-[:knows_limit]->(b) RETURN b.id LIMIT 2",
    );

    assert_eq!(
        rows,
        vec![Row::new(vec![Value::Int(2)]), Row::new(vec![Value::Int(3)]),],
    );
}

#[test]
fn cypher_unanchored_target_filter_limit_returns_filtered_prefix() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_limit_filter (id INT NOT NULL, number INT); \
             CREATE TABLE knows_limit_filter_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_limit_filter ON people_limit_filter; \
             CREATE EDGE LABEL knows_limit_filter ON knows_limit_filter_edges SOURCE person_limit_filter TARGET person_limit_filter; \
             INSERT INTO people_limit_filter VALUES (1, 0), (2, 30), (3, 10), (4, 40), (5, 50); \
             INSERT INTO knows_limit_filter_edges VALUES (1, 2), (1, 3), (1, 4), (2, 5)",
        )
        .expect("seed graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:person_limit_filter)-[:knows_limit_filter]->(b:person_limit_filter) \
         WHERE b.number > 20 \
         RETURN b.id LIMIT 3",
    );

    assert_eq!(
        rows,
        vec![
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(4)]),
            Row::new(vec![Value::Int(5)]),
        ],
    );
}

#[test]
fn cypher_endpoint_id_lookups_cover_incoming_and_bidirectional() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_direction_fast (id INT NOT NULL, category TEXT); \
             CREATE TABLE knows_direction_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_direction_fast ON people_direction_fast; \
             CREATE EDGE LABEL knows_direction_fast ON knows_direction_fast_edges SOURCE person_direction_fast TARGET person_direction_fast; \
             INSERT INTO people_direction_fast VALUES (1, 'src'), (2, 'pivot'), (3, 'src'), (4, 'out'); \
             INSERT INTO knows_direction_fast_edges VALUES (1, 2), (3, 2), (2, 4)",
        )
        .expect("seed graph");

    let incoming = query_rows(
        &engine,
        &session,
        "MATCH (a:person_direction_fast)-[:knows_direction_fast]->(b:person_direction_fast {id: 2}) \
         RETURN a.id ORDER BY a.id",
    );
    assert_eq!(
        incoming,
        vec![Row::new(vec![Value::Int(1)]), Row::new(vec![Value::Int(3)]),],
    );

    let bidirectional = query_rows(
        &engine,
        &session,
        "MATCH (a:person_direction_fast)-[:knows_direction_fast]-(b:person_direction_fast {id: 2}) \
         RETURN a.id ORDER BY a.id",
    );
    assert_eq!(
        bidirectional,
        vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(3)]),
            Row::new(vec![Value::Int(4)]),
        ],
    );
}

#[test]
fn cypher_three_hop_id_lookup_uses_adjacency_chain() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_depth3_fast (id INT NOT NULL); \
             CREATE TABLE knows_depth3_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_depth3_fast ON people_depth3_fast; \
             CREATE EDGE LABEL knows_depth3_fast ON knows_depth3_fast_edges SOURCE person_depth3_fast TARGET person_depth3_fast; \
             INSERT INTO people_depth3_fast VALUES (1), (2), (3), (4), (5), (6); \
             INSERT INTO knows_depth3_fast_edges VALUES (1, 2), (2, 3), (3, 4), (2, 5), (5, 6)",
        )
        .expect("seed graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:person_depth3_fast {id: 1})-[:knows_depth3_fast]->(b:person_depth3_fast)-[:knows_depth3_fast]->(c:person_depth3_fast)-[:knows_depth3_fast]->(d:person_depth3_fast) \
         RETURN d.id ORDER BY d.id",
    );

    assert_eq!(
        rows,
        vec![Row::new(vec![Value::Int(4)]), Row::new(vec![Value::Int(6)]),],
    );
}

#[test]
fn cypher_deep_id_lookup_uses_adjacency_chain() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_depth5_fast (id INT NOT NULL, payload INT NOT NULL); \
             CREATE TABLE knows_depth5_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_depth5_fast ON people_depth5_fast; \
             CREATE EDGE LABEL knows_depth5_fast ON knows_depth5_fast_edges SOURCE person_depth5_fast TARGET person_depth5_fast; \
             INSERT INTO people_depth5_fast VALUES \
                (1, 0), (2, 0), (3, 0), (4, 0), (5, 1), \
                (6, 0), (7, 0), (8, 3), (9, 2), (10, 4); \
             INSERT INTO knows_depth5_fast_edges VALUES \
                (1, 2), (2, 3), (3, 4), (4, 5), \
                (2, 6), (6, 7), (7, 8), \
                (5, 9), (8, 10)",
        )
        .expect("seed graph");

    let four_hop = query_rows(
        &engine,
        &session,
        "MATCH (a:person_depth5_fast {id: 1})-[:knows_depth5_fast]->(b:person_depth5_fast)-[:knows_depth5_fast]->(c:person_depth5_fast)-[:knows_depth5_fast]->(d:person_depth5_fast)-[:knows_depth5_fast]->(e:person_depth5_fast) \
         RETURN e.id ORDER BY e.id",
    );
    assert_eq!(
        four_hop,
        vec![Row::new(vec![Value::Int(5)]), Row::new(vec![Value::Int(8)])],
    );

    let four_hop_desc = query_rows(
        &engine,
        &session,
        "MATCH (a:person_depth5_fast {id: 1})-[:knows_depth5_fast]->(b:person_depth5_fast)-[:knows_depth5_fast]->(c:person_depth5_fast)-[:knows_depth5_fast]->(d:person_depth5_fast)-[:knows_depth5_fast]->(e:person_depth5_fast) \
         RETURN e.id ORDER BY e.id DESC",
    );
    assert_eq!(
        four_hop_desc,
        vec![Row::new(vec![Value::Int(8)]), Row::new(vec![Value::Int(5)])],
    );

    let five_hop = query_rows(
        &engine,
        &session,
        "MATCH (a:person_depth5_fast {id: 1})-[:knows_depth5_fast]->(b:person_depth5_fast)-[:knows_depth5_fast]->(c:person_depth5_fast)-[:knows_depth5_fast]->(d:person_depth5_fast)-[:knows_depth5_fast]->(e:person_depth5_fast)-[:knows_depth5_fast]->(f:person_depth5_fast) \
         RETURN f.id ORDER BY f.id",
    );
    assert_eq!(
        five_hop,
        vec![
            Row::new(vec![Value::Int(9)]),
            Row::new(vec![Value::Int(10)])
        ],
    );

    let four_hop_payload_count = query_rows(
        &engine,
        &session,
        "MATCH (a:person_depth5_fast {id: 1})-[:knows_depth5_fast]->(b:person_depth5_fast)-[:knows_depth5_fast]->(c:person_depth5_fast)-[:knows_depth5_fast]->(d:person_depth5_fast)-[:knows_depth5_fast]->(e:person_depth5_fast) \
         WHERE e.payload = 1 RETURN count(e)",
    );
    assert_eq!(
        four_hop_payload_count,
        vec![Row::new(vec![Value::BigInt(1)])]
    );

    let five_hop_payload_count = query_rows(
        &engine,
        &session,
        "MATCH (a:person_depth5_fast {id: 1})-[:knows_depth5_fast]->(b:person_depth5_fast)-[:knows_depth5_fast]->(c:person_depth5_fast)-[:knows_depth5_fast]->(d:person_depth5_fast)-[:knows_depth5_fast]->(e:person_depth5_fast)-[:knows_depth5_fast]->(f:person_depth5_fast) \
         WHERE f.payload = 2 RETURN count(f)",
    );
    assert_eq!(
        five_hop_payload_count,
        vec![Row::new(vec![Value::BigInt(1)])]
    );
}

#[test]
fn cypher_anchored_edge_property_count_uses_adjacency_edges() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_edge_weight_fast (id INT NOT NULL); \
             CREATE TABLE knows_edge_weight_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL, weight INT NOT NULL); \
             CREATE NODE LABEL person_edge_weight_fast ON people_edge_weight_fast; \
             CREATE EDGE LABEL knows_edge_weight_fast ON knows_edge_weight_fast_edges SOURCE person_edge_weight_fast TARGET person_edge_weight_fast; \
             INSERT INTO people_edge_weight_fast VALUES (1), (2), (3), (4), (5); \
             INSERT INTO knows_edge_weight_fast_edges VALUES \
                (1, 2, 7), (1, 3, 9), (1, 4, 7), (2, 5, 7)",
        )
        .expect("seed weighted graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:person_edge_weight_fast {id: 1})-[r:knows_edge_weight_fast]->(b:person_edge_weight_fast) \
         WHERE r.weight = 7 RETURN count(b)",
    );
    assert_eq!(rows, vec![Row::new(vec![Value::BigInt(2)])]);

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:person_edge_weight_fast {id: 1})-[r:knows_edge_weight_fast]->(b:person_edge_weight_fast) \
         WHERE r.weight = 9 RETURN count(b)",
    );
    assert_eq!(rows, vec![Row::new(vec![Value::BigInt(1)])]);
}

#[test]
fn cypher_anchored_first_edge_property_path_count() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_first_edge_path_fast (id INT NOT NULL); \
             CREATE TABLE knows_first_edge_path_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL, weight INT NOT NULL); \
             CREATE NODE LABEL person_first_edge_path_fast ON people_first_edge_path_fast; \
             CREATE EDGE LABEL knows_first_edge_path_fast ON knows_first_edge_path_fast_edges SOURCE person_first_edge_path_fast TARGET person_first_edge_path_fast; \
             INSERT INTO people_first_edge_path_fast VALUES (1), (2), (3), (4), (5), (6), (7), (8); \
             INSERT INTO knows_first_edge_path_fast_edges VALUES \
                (1, 2, 7), (1, 3, 7), (1, 4, 9), \
                (2, 5, 1), (2, 6, 1), (3, 6, 1), (4, 7, 1), \
                (5, 8, 1), (6, 8, 1), (7, 8, 1)",
        )
        .expect("seed first-edge weighted graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:person_first_edge_path_fast {id: 1})-[r:knows_first_edge_path_fast]->(b:person_first_edge_path_fast)-[:knows_first_edge_path_fast]->(c:person_first_edge_path_fast) \
         WHERE r.weight = 7 RETURN count(c)",
    );
    assert_eq!(rows, vec![Row::new(vec![Value::BigInt(3)])]);

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:person_first_edge_path_fast {id: 1})-[:knows_first_edge_path_fast {weight: 7}]->(b:person_first_edge_path_fast)-[:knows_first_edge_path_fast]->(c:person_first_edge_path_fast)-[:knows_first_edge_path_fast]->(d:person_first_edge_path_fast) \
         RETURN count(d)",
    );
    assert_eq!(rows, vec![Row::new(vec![Value::BigInt(3)])]);

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:person_first_edge_path_fast {id: 1})-[r:knows_first_edge_path_fast]->(b:person_first_edge_path_fast)-[:knows_first_edge_path_fast]->(c:person_first_edge_path_fast) \
         WHERE r.weight = 9 RETURN count(c)",
    );
    assert_eq!(rows, vec![Row::new(vec![Value::BigInt(1)])]);
}

#[test]
fn cypher_anchored_path_counts_use_adjacency_chain() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_count_fast (id INT NOT NULL); \
             CREATE TABLE knows_count_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_count_fast ON people_count_fast; \
             CREATE EDGE LABEL knows_count_fast ON knows_count_fast_edges SOURCE person_count_fast TARGET person_count_fast; \
             INSERT INTO people_count_fast VALUES \
                (1), (2), (3), (4), (5), (6), (7), (8), (9), (10), (11), (12), (13), (14); \
             INSERT INTO knows_count_fast_edges VALUES \
                (1, 2), (1, 3), \
                (2, 4), (2, 5), (3, 6), \
                (4, 7), (5, 8), (6, 9), \
                (7, 10), (8, 11), (9, 12), \
                (10, 13), (11, 14)",
        )
        .expect("seed graph");

    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (a:person_count_fast {id: 1})-[:knows_count_fast]->(b:person_count_fast) \
             RETURN count(b)",
        ),
        2,
    );
    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (a:person_count_fast {id: 1})-[:knows_count_fast]->(b:person_count_fast)-[:knows_count_fast]->(c:person_count_fast) \
             RETURN count(c)",
        ),
        3,
    );
    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (a:person_count_fast {id: 1})-[:knows_count_fast]->(b:person_count_fast)-[:knows_count_fast]->(c:person_count_fast)-[:knows_count_fast]->(d:person_count_fast) \
             RETURN count(d)",
        ),
        3,
    );
    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (a:person_count_fast {id: 1})-[:knows_count_fast]->(b:person_count_fast)-[:knows_count_fast]->(c:person_count_fast)-[:knows_count_fast]->(d:person_count_fast)-[:knows_count_fast]->(e:person_count_fast) \
             RETURN count(e)",
        ),
        3,
    );
    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (a:person_count_fast {id: 1})-[:knows_count_fast]->(b:person_count_fast)-[:knows_count_fast]->(c:person_count_fast)-[:knows_count_fast]->(d:person_count_fast)-[:knows_count_fast]->(e:person_count_fast)-[:knows_count_fast]->(f:person_count_fast) \
             RETURN count(f)",
        ),
        2,
    );
}

#[test]
fn cypher_anchored_variable_path_count_uses_adjacency_edges() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_var_count_fast (id INT NOT NULL); \
             CREATE TABLE knows_var_count_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_var_count_fast ON people_var_count_fast; \
             CREATE EDGE LABEL knows_var_count_fast ON knows_var_count_fast_edges SOURCE person_var_count_fast TARGET person_var_count_fast; \
             INSERT INTO people_var_count_fast VALUES (1), (2), (3), (4); \
             INSERT INTO knows_var_count_fast_edges VALUES \
                (1, 2), (2, 1), (2, 3), (3, 4)",
        )
        .expect("seed variable path count graph");

    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (a:person_var_count_fast {id: 1})-[:knows_var_count_fast*..2]->(b:person_var_count_fast) \
             RETURN count(b)",
        ),
        3,
    );
    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (a:person_var_count_fast {id: 1})-[:knows_var_count_fast*2]->(b:person_var_count_fast) \
             RETURN count(b)",
        ),
        2,
    );
}

#[test]
fn cypher_anchored_distinct_path_counts_use_adjacency_chain() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_distinct_count_fast (id INT NOT NULL); \
             CREATE TABLE knows_distinct_count_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_distinct_count_fast ON people_distinct_count_fast; \
             CREATE EDGE LABEL knows_distinct_count_fast ON knows_distinct_count_fast_edges SOURCE person_distinct_count_fast TARGET person_distinct_count_fast; \
             INSERT INTO people_distinct_count_fast VALUES (1), (2), (3), (4), (5), (6), (7); \
             INSERT INTO knows_distinct_count_fast_edges VALUES \
                (1, 2), (1, 3), \
                (2, 4), (3, 4), (3, 5), \
                (4, 6), (5, 6), (5, 7)",
        )
        .expect("seed graph");

    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (a:person_distinct_count_fast {id: 1})-[:knows_distinct_count_fast]->(b:person_distinct_count_fast)-[:knows_distinct_count_fast]->(c:person_distinct_count_fast) \
             RETURN count(c)",
        ),
        3,
    );
    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (a:person_distinct_count_fast {id: 1})-[:knows_distinct_count_fast]->(b:person_distinct_count_fast)-[:knows_distinct_count_fast]->(c:person_distinct_count_fast) \
             RETURN count(DISTINCT c.id)",
        ),
        2,
    );
    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (a:person_distinct_count_fast {id: 1})-[:knows_distinct_count_fast]->(b:person_distinct_count_fast)-[:knows_distinct_count_fast]->(c:person_distinct_count_fast)-[:knows_distinct_count_fast]->(d:person_distinct_count_fast) \
             RETURN count(DISTINCT d.id)",
        ),
        2,
    );
}

#[test]
fn cypher_unanchored_group_count_uses_target_property() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_group_fast (id INT NOT NULL, category TEXT, number INT); \
             CREATE TABLE knows_group_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_group_fast ON people_group_fast; \
             CREATE EDGE LABEL knows_group_fast ON knows_group_fast_edges SOURCE person_group_fast TARGET person_group_fast; \
             INSERT INTO people_group_fast VALUES (1, 'src', 0), (2, 'dev', 30), (3, 'dev', 10), (4, 'ops', 40); \
             INSERT INTO knows_group_fast_edges VALUES (1, 2), (1, 3), (1, 4)",
        )
        .expect("seed graph");

    let mut rows = query_rows(
        &engine,
        &session,
        "MATCH (a:person_group_fast)-[:knows_group_fast]->(b:person_group_fast) \
         RETURN b.category, count(b)",
    );
    rows.sort_by(|left, right| match (&left.values[0], &right.values[0]) {
        (Value::Text(left), Value::Text(right)) => left.cmp(right),
        _ => std::cmp::Ordering::Equal,
    });

    assert_eq!(
        rows,
        vec![
            Row::new(vec![Value::Text("dev".to_owned()), Value::BigInt(2)]),
            Row::new(vec![Value::Text("ops".to_owned()), Value::BigInt(1)]),
        ],
    );

    let mut filtered_rows = query_rows(
        &engine,
        &session,
        "MATCH (a:person_group_fast)-[:knows_group_fast]->(b:person_group_fast) \
         WHERE b.number > 20 \
         RETURN b.category, count(b)",
    );
    filtered_rows.sort_by(|left, right| match (&left.values[0], &right.values[0]) {
        (Value::Text(left), Value::Text(right)) => left.cmp(right),
        _ => std::cmp::Ordering::Equal,
    });

    assert_eq!(
        filtered_rows,
        vec![
            Row::new(vec![Value::Text("dev".to_owned()), Value::BigInt(1)]),
            Row::new(vec![Value::Text("ops".to_owned()), Value::BigInt(1)]),
        ],
    );

    let ordered_rows = query_rows(
        &engine,
        &session,
        "MATCH (a:person_group_fast)-[:knows_group_fast]->(b:person_group_fast) \
         WHERE b.number > 20 \
         RETURN b.category, count(b) ORDER BY b.category",
    );
    assert_eq!(
        ordered_rows,
        vec![
            Row::new(vec![Value::Text("dev".to_owned()), Value::BigInt(1)]),
            Row::new(vec![Value::Text("ops".to_owned()), Value::BigInt(1)]),
        ],
    );
}

#[test]
fn cypher_unanchored_edge_property_count_uses_projected_edge_scan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_edge_count_fast (id INT NOT NULL); \
             CREATE TABLE knows_edge_count_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL, weight INT NOT NULL); \
             CREATE NODE LABEL person_edge_count_fast ON people_edge_count_fast; \
             CREATE EDGE LABEL knows_edge_count_fast ON knows_edge_count_fast_edges SOURCE person_edge_count_fast TARGET person_edge_count_fast; \
             CREATE INDEX knows_edge_count_fast_weight_idx ON knows_edge_count_fast_edges (weight); \
             INSERT INTO people_edge_count_fast VALUES (1), (2), (3), (4); \
             INSERT INTO knows_edge_count_fast_edges VALUES \
                (1, 2, 5), (1, 3, 15), (2, 3, 20), (3, 4, 1)",
        )
        .expect("seed edge filter count graph");

    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (:person_edge_count_fast)-[e:knows_edge_count_fast]->(b:person_edge_count_fast) \
             WHERE e.weight > 10 RETURN count(b)",
        ),
        2,
    );
}

#[test]
fn cypher_unanchored_incoming_edge_property_count_uses_projected_edge_scan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_incoming_edge_count_fast (id INT NOT NULL); \
             CREATE TABLE knows_incoming_edge_count_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL, weight INT NOT NULL); \
             CREATE NODE LABEL person_incoming_edge_count_fast ON people_incoming_edge_count_fast; \
             CREATE EDGE LABEL knows_incoming_edge_count_fast ON knows_incoming_edge_count_fast_edges SOURCE person_incoming_edge_count_fast TARGET person_incoming_edge_count_fast; \
             CREATE INDEX knows_incoming_edge_count_fast_weight_idx ON knows_incoming_edge_count_fast_edges (weight); \
             INSERT INTO people_incoming_edge_count_fast VALUES (1), (2), (3), (4); \
             INSERT INTO knows_incoming_edge_count_fast_edges VALUES \
                (1, 2, 5), (1, 3, 15), (2, 3, 20), (3, 4, 1)",
        )
        .expect("seed incoming edge filter count graph");

    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (a:person_incoming_edge_count_fast)<-[e:knows_incoming_edge_count_fast]-(:person_incoming_edge_count_fast) \
             WHERE e.weight >= 15 RETURN count(a)",
        ),
        2,
    );
}

#[test]
fn cypher_unanchored_edge_property_and_endpoint_filter_count_uses_projected_edge_scan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_edge_endpoint_filter_fast (id INT NOT NULL, number INT NOT NULL); \
             CREATE TABLE knows_edge_endpoint_filter_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL, weight INT NOT NULL); \
             CREATE NODE LABEL person_edge_endpoint_filter_fast ON people_edge_endpoint_filter_fast; \
             CREATE EDGE LABEL knows_edge_endpoint_filter_fast ON knows_edge_endpoint_filter_fast_edges SOURCE person_edge_endpoint_filter_fast TARGET person_edge_endpoint_filter_fast; \
             CREATE INDEX knows_edge_endpoint_filter_fast_weight_idx ON knows_edge_endpoint_filter_fast_edges (weight); \
             INSERT INTO people_edge_endpoint_filter_fast VALUES (1, 10), (2, 20), (3, 80), (4, 90), (5, 15); \
             INSERT INTO knows_edge_endpoint_filter_fast_edges VALUES \
                (1, 2, 5), (1, 3, 15), (2, 3, 20), (3, 4, 1), (5, 4, 30)",
        )
        .expect("seed edge and endpoint filter graph");

    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (:person_edge_endpoint_filter_fast)-[e:knows_edge_endpoint_filter_fast]->(b:person_edge_endpoint_filter_fast) \
             WHERE e.weight >= 15 AND b.number >= 80 RETURN count(b)",
        ),
        3,
    );
}

#[test]
fn explain_analyze_match_reports_relation_seeded_pattern_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_relation_seeded (id INT NOT NULL, number INT NOT NULL); \
             CREATE TABLE knows_explain_relation_seeded_edges (source_id INT NOT NULL, target_id INT NOT NULL, weight INT NOT NULL); \
             CREATE NODE LABEL person_explain_relation_seeded ON people_explain_relation_seeded; \
             CREATE EDGE LABEL knows_explain_relation_seeded ON knows_explain_relation_seeded_edges SOURCE person_explain_relation_seeded TARGET person_explain_relation_seeded; \
             CREATE INDEX knows_explain_relation_seeded_weight_idx ON knows_explain_relation_seeded_edges (weight); \
             INSERT INTO people_explain_relation_seeded VALUES (1, 10), (2, 20), (3, 80), (4, 90), (5, 15); \
             INSERT INTO knows_explain_relation_seeded_edges VALUES \
                (1, 2, 5), (1, 3, 15), (2, 3, 20), (3, 4, 1), (5, 4, 30)",
        )
        .expect("seed relation-seeded explain graph");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN ANALYZE MATCH (a:person_explain_relation_seeded)-[e:knows_explain_relation_seeded]->(b:person_explain_relation_seeded) \
             WHERE e.weight >= 15 AND b.number >= 80 \
             RETURN a.id, b.id ORDER BY a.id, b.id",
        )
        .expect("execute explain analyze relation-seeded match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain analyze query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Access [PipelineMatch 0 pattern 0]")
                && line.contains("pattern_runtime_strategy=relation_seeded")
                && line.contains("pattern_runtime_strategy_source=observed")
                && line.contains("pattern_runtime_reason=relationship_filter_seed")
                && line.contains("pattern_runtime_reason_source=observed")
        }),
        "explain analyze lines: {lines:?}"
    );

    let detail_json = engine
        .execute_explain_graph_detail_json(
            &session,
            "MATCH (a:person_explain_relation_seeded)-[e:knows_explain_relation_seeded]->(b:person_explain_relation_seeded) \
             WHERE e.weight >= 15 AND b.number >= 80 \
             RETURN a.id, b.id ORDER BY a.id, b.id",
            true,
        )
        .expect("graph detail json");
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_strategy"],
        "relation_seeded"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_strategy_source"],
        "observed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_reason"],
        "relationship_filter_seed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_reason_source"],
        "observed"
    );
}

#[test]
fn explain_analyze_match_reports_path_function_pattern_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_path_function (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain_path_function_edges (source_id INT NOT NULL, target_id INT NOT NULL, weight INT NOT NULL); \
             CREATE NODE LABEL person_explain_path_function ON people_explain_path_function; \
             CREATE EDGE LABEL knows_explain_path_function ON knows_explain_path_function_edges SOURCE person_explain_path_function TARGET person_explain_path_function; \
             INSERT INTO people_explain_path_function VALUES (10, 'Alice'), (20, 'Bob'), (30, 'Carol'); \
             INSERT INTO knows_explain_path_function_edges VALUES (10, 20, 1), (20, 30, 1)",
        )
        .expect("seed path-function explain graph");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN ANALYZE MATCH shortestPath((a:person_explain_path_function {id: 10})-[:knows_explain_path_function*1..3]->(b:person_explain_path_function {id: 30})) RETURN 1",
        )
        .expect("execute explain analyze path-function match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain analyze query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Access [PipelineMatch 0 pattern 0]")
                && line.contains("pattern_runtime_strategy=path_function")
                && line.contains("pattern_runtime_strategy_source=observed")
                && line.contains("pattern_runtime_reason=path_function_dispatch")
                && line.contains("pattern_runtime_reason_source=observed")
        }),
        "explain analyze lines: {lines:?}"
    );

    let detail_json = engine
        .execute_explain_graph_detail_json(
            &session,
            "MATCH shortestPath((a:person_explain_path_function {id: 10})-[:knows_explain_path_function*1..3]->(b:person_explain_path_function {id: 30})) RETURN 1",
            true,
        )
        .expect("graph detail json");
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_strategy"],
        "path_function"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_strategy_source"],
        "observed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_reason"],
        "path_function_dispatch"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_reason_source"],
        "observed"
    );
}

#[test]
fn explain_analyze_match_reports_left_to_right_pattern_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_left_to_right (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain_left_to_right_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_explain_left_to_right ON people_explain_left_to_right; \
             CREATE EDGE LABEL knows_explain_left_to_right ON knows_explain_left_to_right_edges SOURCE person_explain_left_to_right TARGET person_explain_left_to_right; \
             INSERT INTO people_explain_left_to_right VALUES (1, 'Alice'), (2, 'Bob'); \
             INSERT INTO knows_explain_left_to_right_edges VALUES (1, 2)",
        )
        .expect("seed left-to-right explain graph");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN ANALYZE MATCH (a:person_explain_left_to_right)-[:knows_explain_left_to_right]->(b:person_explain_left_to_right) RETURN a.id, b.id ORDER BY a.id, b.id",
        )
        .expect("execute explain analyze left-to-right match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain analyze query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Access [PipelineMatch 0 pattern 0]")
                && line.contains("pattern_runtime_strategy=left_to_right_node_seed")
                && line.contains("pattern_runtime_strategy_source=observed")
                && line.contains("pattern_runtime_reason=left_to_right_walk")
                && line.contains("pattern_runtime_reason_source=observed")
        }),
        "explain analyze lines: {lines:?}"
    );

    let detail_json = engine
        .execute_explain_graph_detail_json(
            &session,
            "MATCH (a:person_explain_left_to_right)-[:knows_explain_left_to_right]->(b:person_explain_left_to_right) RETURN a.id, b.id ORDER BY a.id, b.id",
            true,
        )
        .expect("graph detail json");
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_strategy"],
        "left_to_right_node_seed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_strategy_source"],
        "observed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_reason"],
        "left_to_right_walk"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_reason_source"],
        "observed"
    );
}

#[test]
fn explain_analyze_match_reports_pivoted_node_seed_pattern_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_pivoted_runtime (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain_pivoted_runtime_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_explain_pivoted_runtime ON people_explain_pivoted_runtime; \
             CREATE EDGE LABEL knows_explain_pivoted_runtime ON knows_explain_pivoted_runtime_edges SOURCE person_explain_pivoted_runtime TARGET person_explain_pivoted_runtime; \
             INSERT INTO people_explain_pivoted_runtime VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Carol'); \
             INSERT INTO knows_explain_pivoted_runtime_edges VALUES (1, 2), (2, 3)",
        )
        .expect("seed pivoted-runtime explain graph");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN ANALYZE MATCH (a:person_explain_pivoted_runtime)-[:knows_explain_pivoted_runtime]->(b:person_explain_pivoted_runtime {name: 'Bob'})-[:knows_explain_pivoted_runtime]->(c:person_explain_pivoted_runtime) RETURN a.id, b.id, c.id",
        )
        .expect("execute explain analyze pivoted-runtime match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain analyze query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Access [PipelineMatch 0 pattern 0]")
                && line.contains("pattern_runtime_strategy=pivoted_node_seed")
                && line.contains("pattern_runtime_strategy_source=observed")
                && line.contains("pattern_runtime_reason=pivot_seed")
                && line.contains("pattern_runtime_reason_source=observed")
                && line.contains("pivot_driver=cbo")
                && line.contains("pivot_driver_source=observed")
                && line.contains("pivot_reason=pivot_to_node_1:label_scan")
                && line.contains("pivot_reason_source=observed")
                && line.contains("pivot_decision=selected_node_1")
                && line.contains("pivot_decision_source=observed")
        }),
        "explain analyze lines: {lines:?}"
    );

    let detail_json = engine
        .execute_explain_graph_detail_json(
            &session,
            "MATCH (a:person_explain_pivoted_runtime)-[:knows_explain_pivoted_runtime]->(b:person_explain_pivoted_runtime {name: 'Bob'})-[:knows_explain_pivoted_runtime]->(c:person_explain_pivoted_runtime) RETURN a.id, b.id, c.id",
            true,
        )
        .expect("graph detail json");
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_strategy"],
        "pivoted_node_seed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_strategy_source"],
        "observed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_reason"],
        "pivot_seed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_reason_source"],
        "observed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pivot_driver"],
        "cbo"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pivot_driver_source"],
        "observed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pivot_reason"],
        "pivot_to_node_1:label_scan"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pivot_reason_source"],
        "observed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pivot_decision"],
        "selected_node_1"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pivot_decision_source"],
        "observed"
    );
    let summary_json = engine
        .execute_explain_graph_summary_json(
            &session,
            "MATCH (a:person_explain_pivoted_runtime)-[:knows_explain_pivoted_runtime]->(b:person_explain_pivoted_runtime {name: 'Bob'})-[:knows_explain_pivoted_runtime]->(c:person_explain_pivoted_runtime) RETURN a.id, b.id, c.id",
            true,
        )
        .expect("graph summary json");
    assert_eq!(summary_json["cbo_pivoted"], 1);
    assert_eq!(summary_json["heuristic_pivoted"], 0);
    assert_eq!(summary_json["selected_non_leftmost_source"], "observed");
    assert_eq!(summary_json["pivot_driver_metrics_source"], "observed");
}

#[test]
fn explain_analyze_match_reports_heuristic_pivot_driver() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_explain_heuristic_pivot (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_explain_heuristic_pivot_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_explain_heuristic_pivot ON people_explain_heuristic_pivot; \
             CREATE EDGE LABEL knows_explain_heuristic_pivot ON knows_explain_heuristic_pivot_edges SOURCE person_explain_heuristic_pivot TARGET person_explain_heuristic_pivot; \
             INSERT INTO people_explain_heuristic_pivot VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Carol'); \
             INSERT INTO knows_explain_heuristic_pivot_edges VALUES (1, 2), (2, 3)",
        )
        .expect("seed heuristic-pivot explain graph");

    let results = engine
        .execute_sql(
            &session,
            "EXPLAIN ANALYZE MATCH (a:person_explain_heuristic_pivot)-[:knows_explain_heuristic_pivot]-(b:person_explain_heuristic_pivot {name: 'Bob'})-[:knows_explain_heuristic_pivot]-(c:person_explain_heuristic_pivot) RETURN a.id, b.id, c.id",
        )
        .expect("execute explain analyze heuristic-pivot match");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain analyze query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains("Graph Access [PipelineMatch 0 pattern 0]")
                && line.contains("pattern_runtime_strategy=pivoted_node_seed")
                && line.contains("pattern_runtime_strategy_source=observed")
                && line.contains("pivot_driver=heuristic")
                && line.contains("pivot_driver_source=observed")
                && line.contains("pivot_reason=pivot_to_node_1:label_scan")
                && line.contains("pivot_reason_source=observed")
                && line.contains("pivot_decision=selected_node_1")
                && line.contains("pivot_decision_source=observed")
        }),
        "explain analyze lines: {lines:?}"
    );

    let detail_json = engine
        .execute_explain_graph_detail_json(
            &session,
            "MATCH (a:person_explain_heuristic_pivot)-[:knows_explain_heuristic_pivot]-(b:person_explain_heuristic_pivot {name: 'Bob'})-[:knows_explain_heuristic_pivot]-(c:person_explain_heuristic_pivot) RETURN a.id, b.id, c.id",
            true,
        )
        .expect("graph detail json");
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pattern_runtime_strategy"],
        "pivoted_node_seed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pivot_driver"],
        "heuristic"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pivot_driver_source"],
        "observed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pivot_reason"],
        "pivot_to_node_1:label_scan"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pivot_reason_source"],
        "observed"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pivot_decision"],
        "selected_node_1"
    );
    assert_eq!(
        detail_json["clauses"][0]["pattern_details"][0]["pivot_decision_source"],
        "observed"
    );
    let summary_json = engine
        .execute_explain_graph_summary_json(
            &session,
            "MATCH (a:person_explain_heuristic_pivot)-[:knows_explain_heuristic_pivot]-(b:person_explain_heuristic_pivot {name: 'Bob'})-[:knows_explain_heuristic_pivot]-(c:person_explain_heuristic_pivot) RETURN a.id, b.id, c.id",
            true,
        )
        .expect("graph summary json");
    assert_eq!(summary_json["cbo_pivoted"], 0);
    assert_eq!(summary_json["heuristic_pivoted"], 1);
    assert_eq!(summary_json["selected_non_leftmost_source"], "observed");
    assert_eq!(summary_json["pivot_driver_metrics_source"], "observed");
}

#[test]
fn cypher_edge_property_filter_with_exact_endpoint_id_remains_correct() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_edge_exact_endpoint_fast (id INT NOT NULL, number INT NOT NULL); \
             CREATE TABLE knows_edge_exact_endpoint_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL, weight INT NOT NULL); \
             CREATE NODE LABEL person_edge_exact_endpoint_fast ON people_edge_exact_endpoint_fast; \
             CREATE EDGE LABEL knows_edge_exact_endpoint_fast ON knows_edge_exact_endpoint_fast_edges SOURCE person_edge_exact_endpoint_fast TARGET person_edge_exact_endpoint_fast; \
             CREATE INDEX knows_edge_exact_endpoint_fast_weight_idx ON knows_edge_exact_endpoint_fast_edges (weight); \
             INSERT INTO people_edge_exact_endpoint_fast VALUES (1, 10), (2, 20), (3, 80), (4, 90), (5, 15); \
             INSERT INTO knows_edge_exact_endpoint_fast_edges VALUES \
                (1, 2, 5), (1, 3, 15), (2, 3, 20), (3, 4, 1), (5, 4, 30)",
        )
        .expect("seed edge exact endpoint graph");

    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (:person_edge_exact_endpoint_fast)-[e:knows_edge_exact_endpoint_fast]->(b:person_edge_exact_endpoint_fast {id: 3}) \
             WHERE e.weight >= 15 RETURN count(b)",
        ),
        2,
    );
}

#[test]
fn cypher_edge_property_filter_with_small_endpoint_candidate_set_remains_correct() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_edge_small_endpoint_fast (id INT NOT NULL, grp INT NOT NULL); \
             CREATE TABLE knows_edge_small_endpoint_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL, weight INT NOT NULL); \
             CREATE NODE LABEL person_edge_small_endpoint_fast ON people_edge_small_endpoint_fast; \
             CREATE EDGE LABEL knows_edge_small_endpoint_fast ON knows_edge_small_endpoint_fast_edges SOURCE person_edge_small_endpoint_fast TARGET person_edge_small_endpoint_fast; \
             CREATE INDEX knows_edge_small_endpoint_fast_weight_idx ON knows_edge_small_endpoint_fast_edges (weight); \
             INSERT INTO people_edge_small_endpoint_fast VALUES \
                (1, 1), (2, 1), (3, 1), (4, 2), (5, 2), (6, 3), (7, 3), (8, 3), (9, 3), (10, 3); \
             INSERT INTO knows_edge_small_endpoint_fast_edges VALUES \
                (1, 4, 50), (2, 4, 60), (3, 5, 70), (6, 4, 80), (7, 5, 5), (8, 9, 90)",
        )
        .expect("seed edge small endpoint graph");

    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (:person_edge_small_endpoint_fast)-[e:knows_edge_small_endpoint_fast]->(b:person_edge_small_endpoint_fast {grp: 2}) \
             WHERE e.weight >= 50 RETURN count(b)",
        ),
        4,
    );
}

#[test]
fn cypher_multi_out_filtered_count_uses_grouped_edge_counts() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_multi_count_fast (id INT NOT NULL, number INT NOT NULL); \
             CREATE TABLE knows_multi_count_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_multi_count_fast ON people_multi_count_fast; \
             CREATE EDGE LABEL knows_multi_count_fast ON knows_multi_count_fast_edges SOURCE person_multi_count_fast TARGET person_multi_count_fast; \
             INSERT INTO people_multi_count_fast VALUES \
                (1, 0), (2, 30), (3, 10), (4, 40), (5, 50); \
             INSERT INTO knows_multi_count_fast_edges VALUES \
                (1, 2), (1, 3), (1, 4), \
                (2, 3), (2, 4), (2, 5)",
        )
        .expect("seed multi-out count graph");

    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (a:person_multi_count_fast)-[:knows_multi_count_fast]->(b:person_multi_count_fast), \
                   (a)-[:knows_multi_count_fast]->(c:person_multi_count_fast) \
             WHERE b.number > 20 AND b.id <> c.id RETURN count(*)",
        ),
        8,
    );
}

#[test]
fn cypher_multi_out_filtered_count_distinct_c_id_is_stable() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_multi_count_distinct (id INT NOT NULL, number INT NOT NULL); \
             CREATE TABLE knows_multi_count_distinct_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_multi_count_distinct ON people_multi_count_distinct; \
             CREATE EDGE LABEL knows_multi_count_distinct ON knows_multi_count_distinct_edges SOURCE person_multi_count_distinct TARGET person_multi_count_distinct; \
             INSERT INTO people_multi_count_distinct VALUES \
                (1, 0), (2, 30), (3, 10), (4, 40), (5, 50); \
             INSERT INTO knows_multi_count_distinct_edges VALUES \
                (1, 2), (1, 3), (1, 4), \
                (2, 3), (2, 4), (2, 5)",
        )
        .expect("seed multi-out distinct-count graph");

    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (a:person_multi_count_distinct)-[:knows_multi_count_distinct]->(b:person_multi_count_distinct), \
                   (a)-[:knows_multi_count_distinct]->(c:person_multi_count_distinct) \
             WHERE b.number > 20 AND b.id <> c.id RETURN count(DISTINCT c.id)",
        ),
        4,
    );
}

#[test]
fn cypher_global_count_distinct_survives_duplicate_input_bindings() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_multi_count_distinct_dups (id INT NOT NULL, number INT NOT NULL); \
             CREATE TABLE knows_multi_count_distinct_dups_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_multi_count_distinct_dups ON people_multi_count_distinct_dups; \
             CREATE EDGE LABEL knows_multi_count_distinct_dups ON knows_multi_count_distinct_dups_edges SOURCE person_multi_count_distinct_dups TARGET person_multi_count_distinct_dups; \
             INSERT INTO people_multi_count_distinct_dups VALUES \
                (1, 0), (2, 30), (3, 10), (4, 40), (5, 50); \
             INSERT INTO knows_multi_count_distinct_dups_edges VALUES \
                (1, 2), (1, 3), (1, 4), \
                (2, 3), (2, 4), (2, 5)",
        )
        .expect("seed duplicate-input distinct-count graph");

    assert_eq!(
        query_count(
            &engine,
            &session,
            "UNWIND [1, 1] AS dup \
             MATCH (a:person_multi_count_distinct_dups)-[:knows_multi_count_distinct_dups]->(b:person_multi_count_distinct_dups), \
                   (a)-[:knows_multi_count_distinct_dups]->(c:person_multi_count_distinct_dups) \
             WHERE b.number > 20 AND b.id <> c.id \
             RETURN count(DISTINCT c.id)",
        ),
        4,
    );
}

#[test]
fn cypher_unanchored_two_hop_end_filter_counts_from_reverse_adjacency() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_twohop_filter_fast (id INT NOT NULL, number INT NOT NULL); \
             CREATE TABLE knows_twohop_filter_fast_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_twohop_filter_fast ON people_twohop_filter_fast; \
             CREATE EDGE LABEL knows_twohop_filter_fast ON knows_twohop_filter_fast_edges SOURCE person_twohop_filter_fast TARGET person_twohop_filter_fast; \
             INSERT INTO people_twohop_filter_fast VALUES \
                (1, 0), (2, 0), (3, 0), (4, 70), (5, 80), (6, 10); \
             INSERT INTO knows_twohop_filter_fast_edges VALUES \
                (1, 2), (1, 3), (2, 4), (3, 4), (3, 5), (6, 3)",
        )
        .expect("seed two-hop filtered graph");

    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (a:person_twohop_filter_fast)-[:knows_twohop_filter_fast]->(b:person_twohop_filter_fast)-[:knows_twohop_filter_fast]->(c:person_twohop_filter_fast) \
             WHERE c.number > 63 RETURN count(c)",
        ),
        5,
    );
    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (a:person_twohop_filter_fast)-[:knows_twohop_filter_fast]->(b:person_twohop_filter_fast)-[:knows_twohop_filter_fast]->(c:person_twohop_filter_fast) \
             WHERE c.number > 63 RETURN count(DISTINCT c.id)",
        ),
        2,
    );
}

#[test]
fn cypher_multi_out_limit_returns_local_cartesian_prefix() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_multi (id INT NOT NULL, number INT); \
             CREATE TABLE knows_multi_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_multi ON people_multi; \
             CREATE EDGE LABEL knows_multi ON knows_multi_edges SOURCE person_multi TARGET person_multi; \
             INSERT INTO people_multi VALUES (1, 0), (2, 30), (3, 10), (4, 40); \
             INSERT INTO knows_multi_edges VALUES (1, 2), (1, 3), (1, 4)",
        )
        .expect("seed graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:person_multi)-[:knows_multi]->(b:person_multi), \
               (a)-[:knows_multi]->(c:person_multi) \
         RETURN b.id, c.id LIMIT 4",
    );

    assert_eq!(
        rows,
        vec![
            Row::new(vec![Value::Int(2), Value::Int(2)]),
            Row::new(vec![Value::Int(2), Value::Int(3)]),
            Row::new(vec![Value::Int(2), Value::Int(4)]),
            Row::new(vec![Value::Int(3), Value::Int(2)]),
        ],
    );
}

#[test]
fn cypher_multi_out_where_limit_filters_left_branch() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_multi_where (id INT NOT NULL, number INT); \
             CREATE TABLE knows_multi_where_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_multi_where ON people_multi_where; \
             CREATE EDGE LABEL knows_multi_where ON knows_multi_where_edges SOURCE person_multi_where TARGET person_multi_where; \
             INSERT INTO people_multi_where VALUES (1, 0), (2, 30), (3, 10), (4, 40); \
             INSERT INTO knows_multi_where_edges VALUES (1, 2), (1, 3), (1, 4)",
        )
        .expect("seed graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:person_multi_where)-[:knows_multi_where]->(b:person_multi_where), \
               (a)-[:knows_multi_where]->(c:person_multi_where) \
         WHERE b.number > 20 \
         RETURN b.id, c.id LIMIT 4",
    );

    assert_eq!(
        rows,
        vec![
            Row::new(vec![Value::Int(2), Value::Int(2)]),
            Row::new(vec![Value::Int(2), Value::Int(3)]),
            Row::new(vec![Value::Int(2), Value::Int(4)]),
            Row::new(vec![Value::Int(4), Value::Int(2)]),
        ],
    );
}

#[test]
fn cypher_multi_out_order_by_limit_keeps_sorted_prefix() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_multi_ordered (id INT NOT NULL, number INT); \
             CREATE TABLE knows_multi_ordered_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_multi_ordered ON people_multi_ordered; \
             CREATE EDGE LABEL knows_multi_ordered ON knows_multi_ordered_edges SOURCE person_multi_ordered TARGET person_multi_ordered; \
             INSERT INTO people_multi_ordered VALUES (1, 0), (2, 30), (3, 10), (4, 40); \
             INSERT INTO knows_multi_ordered_edges VALUES (1, 2), (1, 3), (1, 4)",
        )
        .expect("seed graph");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:person_multi_ordered)-[:knows_multi_ordered]->(b:person_multi_ordered), \
               (a)-[:knows_multi_ordered]->(c:person_multi_ordered) \
         RETURN b.id, c.id ORDER BY b.id, c.id LIMIT 4",
    );

    assert_eq!(
        rows,
        vec![
            Row::new(vec![Value::Int(2), Value::Int(2)]),
            Row::new(vec![Value::Int(2), Value::Int(3)]),
            Row::new(vec![Value::Int(2), Value::Int(4)]),
            Row::new(vec![Value::Int(3), Value::Int(2)]),
        ],
    );
}

#[test]
fn cypher_inline_edge_property_equality_limit_filters_edges() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people_eq (id INT NOT NULL, number INT); \
             CREATE TABLE knows_eq_edges (source_id INT NOT NULL, target_id INT NOT NULL, weight INT NOT NULL); \
             CREATE NODE LABEL person_eq ON people_eq; \
             CREATE EDGE LABEL knows_eq ON knows_eq_edges SOURCE person_eq TARGET person_eq; \
             INSERT INTO people_eq VALUES (1, 0), (2, 0), (3, 0), (4, 0), (5, 0); \
             INSERT INTO knows_eq_edges VALUES (1, 2, 10), (1, 3, 20), (2, 4, 10), (3, 5, 10), (4, 5, 99)",
        )
        .expect("seed graph");

    // Inline `{weight: 10}` equality filter must match only edges whose weight
    // is exactly 10, preserving edge scan order, honoring LIMIT.
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:person_eq)-[:knows_eq {weight: 10}]->(b:person_eq) RETURN b.id LIMIT 10",
    );
    assert_eq!(
        rows,
        vec![
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(4)]),
            Row::new(vec![Value::Int(5)]),
        ],
    );

    let limited = query_rows(
        &engine,
        &session,
        "MATCH (a:person_eq)-[:knows_eq {weight: 10}]->(b:person_eq) RETURN b.id LIMIT 2",
    );
    assert_eq!(
        limited,
        vec![Row::new(vec![Value::Int(2)]), Row::new(vec![Value::Int(4)])],
    );

    // Reversed pattern orientation (binder may anchor on the returned node)
    // must yield the same edges.
    let reversed = query_rows(
        &engine,
        &session,
        "MATCH (b:person_eq)<-[:knows_eq {weight: 10}]-(a:person_eq) RETURN b.id LIMIT 10",
    );
    assert_eq!(reversed, rows);
}
