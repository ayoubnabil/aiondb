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
}

#[test]
fn explain_graph_procedure_includes_projection_lines() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

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
