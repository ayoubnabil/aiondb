use super::*;

#[test]
fn cypher_match_denied_without_select_on_label_backing_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             INSERT INTO people VALUES (1, 'alice'); \
             CREATE NODE LABEL Person ON people; \
             CREATE ROLE reader LOGIN",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(&reader_session, "MATCH (n:Person) RETURN 1")
        .expect_err("MATCH should be denied without SELECT on backing table");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn cypher_unlabeled_match_requires_select_on_all_node_label_tables() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             CREATE TABLE companies (id BIGINT NOT NULL, name TEXT); \
             INSERT INTO people VALUES (1, 'alice'); \
             INSERT INTO companies VALUES (10, 'acme'); \
             CREATE NODE LABEL Person ON people; \
             CREATE NODE LABEL Company ON companies; \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON people TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(&reader_session, "MATCH (n) RETURN 1")
        .expect_err("unlabeled MATCH should require SELECT on all node label tables");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn cypher_create_denied_without_insert_on_label_backing_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             CREATE NODE LABEL Person ON people; \
             CREATE ROLE writer LOGIN",
        )
        .expect("setup");

    let (writer_session, _) = engine
        .startup(startup_as("writer"))
        .expect("writer startup");

    let err = engine
        .execute_sql(
            &writer_session,
            "CREATE (n:Person {id: 1, name: 'alice'}) RETURN 1",
        )
        .expect_err("CREATE should be denied without INSERT on backing table");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn cypher_create_denied_when_insert_would_add_node_property_column_under_rbac() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             CREATE NODE LABEL Person ON people; \
             CREATE ROLE writer LOGIN; \
             GRANT INSERT ON people TO writer",
        )
        .expect("setup");

    let (writer_session, _) = engine
        .startup(startup_as("writer"))
        .expect("writer startup");

    let err = engine
        .execute_sql(
            &writer_session,
            "CREATE (n:Person {id: 1, name: 'alice', nickname: 'ally'}) RETURN 1",
        )
        .expect_err("CREATE should be denied when it would add a missing column");
    let msg = format!("{err}");
    assert!(
        msg.contains("cannot add missing property column") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );
}

#[test]
fn cypher_create_denied_when_insert_would_add_edge_property_column_under_rbac() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL); \
             CREATE TABLE knows_edges (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             CREATE NODE LABEL Person ON people; \
             CREATE EDGE LABEL KNOWS ON knows_edges SOURCE Person TARGET Person; \
             CREATE ROLE writer LOGIN; \
             GRANT INSERT ON people TO writer; \
             GRANT INSERT ON knows_edges TO writer",
        )
        .expect("setup");

    let (writer_session, _) = engine
        .startup(startup_as("writer"))
        .expect("writer startup");

    let err = engine
        .execute_sql(
            &writer_session,
            "CREATE (:Person {id: 1})-[:KNOWS {weight: 1.0}]->(:Person {id: 2}) RETURN 1",
        )
        .expect_err("CREATE should be denied when edge properties would add a missing column");
    let msg = format!("{err}");
    assert!(
        msg.contains("cannot add missing property column") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );
}

#[test]
fn cypher_hybrid_function_expression_denied_without_execute_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT, score BIGINT); \
             CREATE TABLE items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]'); \
             CREATE NODE LABEL Person ON people; \
             CREATE ROLE reader LOGIN; \
             GRANT INSERT ON people TO reader; \
             GRANT SELECT ON items TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(
            &reader_session,
            "CREATE (n:Person {id: 1, name: 'alice', score: vector_top_k_ids('public.items', 'embedding', '[1.0,0.0]', 1)}) RETURN 1",
        )
        .expect_err("Cypher expression should require EXECUTE on protected hybrid function");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn cypher_hybrid_function_expression_denied_without_select_on_hybrid_target() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT, score BIGINT); \
             CREATE TABLE items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]'); \
             CREATE NODE LABEL Person ON people; \
             CREATE ROLE reader LOGIN; \
             GRANT INSERT ON people TO reader; \
             GRANT EXECUTE ON FUNCTION vector_top_k_ids(text,text,text,integer) TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(
            &reader_session,
            "CREATE (n:Person {id: 1, name: 'alice', score: vector_top_k_ids('public.items', 'embedding', '[1.0,0.0]', 1)}) RETURN 1",
        )
        .expect_err("Cypher expression should require SELECT on the hybrid target table");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn cypher_hybrid_function_case_wrapper_denied_without_select_on_hybrid_target() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT, score BIGINT); \
             CREATE TABLE items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]'); \
             CREATE NODE LABEL Person ON people; \
             CREATE ROLE reader LOGIN; \
             GRANT INSERT ON people TO reader; \
             GRANT EXECUTE ON FUNCTION vector_top_k_ids(text,text,text,integer) TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(
            &reader_session,
            "CREATE (n:Person { \
                id: 1, \
                name: 'alice', \
                score: CASE \
                    WHEN TRUE THEN vector_top_k_ids('public.items', 'embedding', '[1.0,0.0]', 1) \
                    ELSE vector_top_k_ids('public.items', 'embedding', '[1.0,0.0]', 1) \
                END \
            }) RETURN 1",
        )
        .expect_err("wrapped Cypher function should still require SELECT on the hybrid target");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn cypher_graph_neighbors_case_wrapper_denied_without_select_on_hybrid_target() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT, neighbors TEXT); \
             CREATE TABLE docs (id BIGINT NOT NULL); \
             CREATE TABLE doc_links (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             INSERT INTO docs VALUES (1); \
             INSERT INTO docs VALUES (2); \
             INSERT INTO doc_links VALUES (1, 2); \
             CREATE NODE LABEL Person ON people; \
             CREATE NODE LABEL doc ON docs; \
             CREATE EDGE LABEL related_doc ON doc_links SOURCE doc TARGET doc; \
             CREATE ROLE reader LOGIN; \
             GRANT INSERT ON people TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(
            &reader_session,
            "CREATE (n:Person { \
                id: 1, \
                name: 'alice', \
                neighbors: CASE \
                    WHEN TRUE THEN graph_neighbors('related_doc', 1) \
                    ELSE graph_neighbors('related_doc', 1) \
                END \
            }) RETURN 1",
        )
        .expect_err("wrapped graph_neighbors should still require SELECT on backing edge table");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn cypher_hybrid_function_array_wrapper_denied_without_select_on_hybrid_target() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT, score TEXT[]); \
             CREATE TABLE items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]'); \
             CREATE NODE LABEL Person ON people; \
             CREATE ROLE reader LOGIN; \
             GRANT INSERT ON people TO reader; \
             GRANT EXECUTE ON FUNCTION vector_top_k_ids(text,text,text,integer) TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(
            &reader_session,
            "CREATE (n:Person { \
                id: 1, \
                name: 'alice', \
                score: [vector_top_k_ids('public.items', 'embedding', '[1.0,0.0]', 1)] \
            }) RETURN 1",
        )
        .expect_err(
            "wrapped array Cypher function should still require SELECT on the hybrid target",
        );
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn cypher_graph_neighbors_array_wrapper_denied_without_select_on_hybrid_target() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT, neighbors TEXT[]); \
             CREATE TABLE docs (id BIGINT NOT NULL); \
             CREATE TABLE doc_links (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             INSERT INTO docs VALUES (1); \
             INSERT INTO docs VALUES (2); \
             INSERT INTO doc_links VALUES (1, 2); \
             CREATE NODE LABEL Person ON people; \
             CREATE NODE LABEL doc ON docs; \
             CREATE EDGE LABEL related_doc ON doc_links SOURCE doc TARGET doc; \
             CREATE ROLE reader LOGIN; \
             GRANT INSERT ON people TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(
            &reader_session,
            "CREATE (n:Person { \
                id: 1, \
                name: 'alice', \
                neighbors: [graph_neighbors('related_doc', 1)] \
            }) RETURN 1",
        )
        .expect_err(
            "wrapped graph_neighbors array should still require SELECT on backing edge table",
        );
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn cypher_merge_on_match_set_denied_without_update() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             INSERT INTO people VALUES (1, 'alice'); \
             CREATE NODE LABEL Person ON people; \
             CREATE ROLE merger LOGIN; \
             GRANT SELECT ON people TO merger; \
             GRANT INSERT ON people TO merger",
        )
        .expect("setup");

    let (merger_session, _) = engine
        .startup(startup_as("merger"))
        .expect("merger startup");

    let err = engine
        .execute_sql(
            &merger_session,
            "MERGE (n:Person {id: 1}) ON MATCH SET n.name = 'bob' RETURN 1",
        )
        .expect_err("MERGE ON MATCH SET should be denied without UPDATE");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn cypher_merge_on_create_set_denied_without_update() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             CREATE NODE LABEL Person ON people; \
             CREATE ROLE merger LOGIN; \
             GRANT SELECT ON people TO merger; \
             GRANT INSERT ON people TO merger",
        )
        .expect("setup");

    let (merger_session, _) = engine
        .startup(startup_as("merger"))
        .expect("merger startup");

    let err = engine
        .execute_sql(
            &merger_session,
            "MERGE (n:Person {id: 1}) ON CREATE SET n.name = 'bob' RETURN 1",
        )
        .expect_err("MERGE ON CREATE SET should be denied without UPDATE");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn cypher_with_alias_set_succeeds_with_select_and_update_grants() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             INSERT INTO people VALUES (1, 'alice'); \
             CREATE NODE LABEL Person ON people; \
             CREATE ROLE editor LOGIN; \
             GRANT SELECT ON people TO editor; \
             GRANT UPDATE ON people TO editor",
        )
        .expect("setup");

    let (editor_session, _) = engine
        .startup(startup_as("editor"))
        .expect("editor startup");

    engine
        .execute_sql(
            &editor_session,
            "MATCH (n:Person {id: 1}) WITH n AS m SET m.name = 'bob' RETURN 1",
        )
        .expect("WITH alias SET should succeed with SELECT+UPDATE grants");
}

#[test]
fn cypher_with_alias_set_denied_without_update_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             INSERT INTO people VALUES (1, 'alice'); \
             CREATE NODE LABEL Person ON people; \
             CREATE ROLE editor LOGIN; \
             GRANT SELECT ON people TO editor",
        )
        .expect("setup");

    let (editor_session, _) = engine
        .startup(startup_as("editor"))
        .expect("editor startup");

    let err = engine
        .execute_sql(
            &editor_session,
            "MATCH (n:Person {id: 1}) WITH n AS m SET m.name = 'bob' RETURN 1",
        )
        .expect_err("WITH alias SET should require UPDATE on backing table");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn cypher_with_alias_delete_succeeds_with_select_and_delete_grants() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             INSERT INTO people VALUES (1, 'alice'); \
             CREATE NODE LABEL Person ON people; \
             CREATE ROLE deleter LOGIN; \
             GRANT SELECT ON people TO deleter; \
             GRANT DELETE ON people TO deleter",
        )
        .expect("setup");

    let (deleter_session, _) = engine
        .startup(startup_as("deleter"))
        .expect("deleter startup");

    engine
        .execute_sql(
            &deleter_session,
            "MATCH (n:Person {id: 1}) WITH n AS m DELETE m RETURN 1",
        )
        .expect("WITH alias DELETE should succeed with SELECT+DELETE grants");
}

#[test]
fn cypher_detach_delete_denied_without_delete_on_connected_edge_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             INSERT INTO people VALUES (1, 'alice'); \
             INSERT INTO people VALUES (2, 'bob'); \
             INSERT INTO knows_edges VALUES (1, 2); \
             CREATE NODE LABEL Person ON people; \
             CREATE EDGE LABEL KNOWS ON knows_edges SOURCE Person TARGET Person; \
             CREATE ROLE deleter LOGIN; \
             GRANT SELECT ON people TO deleter; \
             GRANT SELECT ON knows_edges TO deleter; \
             GRANT DELETE ON people TO deleter",
        )
        .expect("setup");

    let (deleter_session, _) = engine
        .startup(startup_as("deleter"))
        .expect("deleter startup");

    let err = engine
        .execute_sql(&deleter_session, "MATCH (n:Person {id: 1}) DETACH DELETE n")
        .expect_err("DETACH DELETE should require DELETE on connected edge tables");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn cypher_detach_delete_ignores_unrelated_edge_labels_for_acl() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             CREATE TABLE companies (id BIGINT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             CREATE TABLE partner_edges (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             INSERT INTO people VALUES (1, 'alice'); \
             INSERT INTO people VALUES (2, 'bob'); \
             INSERT INTO companies VALUES (10, 'acme'); \
             INSERT INTO companies VALUES (20, 'globex'); \
             INSERT INTO knows_edges VALUES (1, 2); \
             INSERT INTO partner_edges VALUES (10, 20); \
             CREATE NODE LABEL Person ON people; \
             CREATE NODE LABEL Company ON companies; \
             CREATE EDGE LABEL KNOWS ON knows_edges SOURCE Person TARGET Person; \
             CREATE EDGE LABEL PARTNER ON partner_edges SOURCE Company TARGET Company; \
             CREATE ROLE deleter LOGIN; \
             GRANT SELECT ON people TO deleter; \
             GRANT DELETE ON people TO deleter; \
             GRANT DELETE ON knows_edges TO deleter",
        )
        .expect("setup");

    let (deleter_session, _) = engine
        .startup(startup_as("deleter"))
        .expect("deleter startup");

    engine
        .execute_sql(&deleter_session, "MATCH (n:Person {id: 1}) DETACH DELETE n")
        .expect("DETACH DELETE should not require DELETE on unrelated edge labels");
}

#[test]
fn cypher_detach_delete_does_not_touch_non_incident_bound_edges() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             CREATE TABLE companies (id BIGINT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             CREATE TABLE partner_edges (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             INSERT INTO people VALUES (1, 'alice'); \
             INSERT INTO people VALUES (2, 'bob'); \
             INSERT INTO companies VALUES (10, 'acme'); \
             INSERT INTO companies VALUES (20, 'globex'); \
             INSERT INTO knows_edges VALUES (1, 2); \
             INSERT INTO partner_edges VALUES (10, 20); \
             CREATE NODE LABEL Person ON people; \
             CREATE NODE LABEL Company ON companies; \
             CREATE EDGE LABEL KNOWS ON knows_edges SOURCE Person TARGET Person; \
             CREATE EDGE LABEL PARTNER ON partner_edges SOURCE Company TARGET Company; \
             CREATE ROLE deleter LOGIN; \
             GRANT SELECT ON people TO deleter; \
             GRANT SELECT ON companies TO deleter; \
             GRANT SELECT ON partner_edges TO deleter; \
             GRANT DELETE ON people TO deleter; \
             GRANT DELETE ON knows_edges TO deleter",
        )
        .expect("setup");

    let (deleter_session, _) = engine
        .startup(startup_as("deleter"))
        .expect("deleter startup");

    engine
        .execute_sql(
            &deleter_session,
            "MATCH (n:Person {id: 1}), (c1:Company {id: 10})-[e:PARTNER]->(c2:Company {id: 20}) \
             WITH n, e DETACH DELETE n",
        )
        .expect("DETACH DELETE should not try to delete unrelated bound edge variables");

    let remaining_people = query_rows(&engine, &admin, "SELECT COUNT(*) FROM people WHERE id = 1");
    assert_eq!(
        remaining_people,
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(0)])]
    );

    let remaining_partner_edges = query_rows(&engine, &admin, "SELECT COUNT(*) FROM partner_edges");
    assert_eq!(
        remaining_partner_edges,
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(1)])]
    );
}

#[test]
fn cypher_detach_delete_removes_incident_edges_without_touching_unrelated_labels() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             CREATE TABLE companies (id BIGINT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             CREATE TABLE partner_edges (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             INSERT INTO people VALUES (1, 'alice'); \
             INSERT INTO people VALUES (2, 'bob'); \
             INSERT INTO companies VALUES (1, 'acme'); \
             INSERT INTO companies VALUES (2, 'globex'); \
             INSERT INTO knows_edges VALUES (1, 2); \
             INSERT INTO partner_edges VALUES (1, 2); \
             CREATE NODE LABEL Person ON people; \
             CREATE NODE LABEL Company ON companies; \
             CREATE EDGE LABEL KNOWS ON knows_edges SOURCE Person TARGET Person; \
             CREATE EDGE LABEL PARTNER ON partner_edges SOURCE Company TARGET Company; \
             CREATE ROLE deleter LOGIN; \
             GRANT SELECT ON people TO deleter; \
             GRANT DELETE ON people TO deleter; \
             GRANT DELETE ON knows_edges TO deleter",
        )
        .expect("setup");

    let (deleter_session, _) = engine
        .startup(startup_as("deleter"))
        .expect("deleter startup");

    engine
        .execute_sql(&deleter_session, "MATCH (n:Person {id: 1}) DETACH DELETE n")
        .expect("DETACH DELETE should remove incident edges for the deleted node");

    let remaining_people = query_rows(&engine, &admin, "SELECT COUNT(*) FROM people WHERE id = 1");
    assert_eq!(
        remaining_people,
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(0)])]
    );

    let remaining_knows_edges = query_rows(&engine, &admin, "SELECT COUNT(*) FROM knows_edges");
    assert_eq!(
        remaining_knows_edges,
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(0)])]
    );

    let remaining_partner_edges = query_rows(&engine, &admin, "SELECT COUNT(*) FROM partner_edges");
    assert_eq!(
        remaining_partner_edges,
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(1)])]
    );
}

#[test]
fn cypher_detach_delete_edge_variable_does_not_require_unrelated_edge_delete_grants() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             CREATE TABLE companies (id BIGINT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             CREATE TABLE partner_edges (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             INSERT INTO people VALUES (1, 'alice'); \
             INSERT INTO people VALUES (2, 'bob'); \
             INSERT INTO companies VALUES (1, 'acme'); \
             INSERT INTO companies VALUES (2, 'globex'); \
             INSERT INTO knows_edges VALUES (1, 2); \
             INSERT INTO partner_edges VALUES (1, 2); \
             CREATE NODE LABEL Person ON people; \
             CREATE NODE LABEL Company ON companies; \
             CREATE EDGE LABEL KNOWS ON knows_edges SOURCE Person TARGET Person; \
             CREATE EDGE LABEL PARTNER ON partner_edges SOURCE Company TARGET Company; \
             CREATE ROLE deleter LOGIN; \
             GRANT SELECT ON knows_edges TO deleter; \
             GRANT SELECT ON people TO deleter; \
             GRANT DELETE ON knows_edges TO deleter",
        )
        .expect("setup");

    let (deleter_session, _) = engine
        .startup(startup_as("deleter"))
        .expect("deleter startup");

    engine
        .execute_sql(
            &deleter_session,
            "MATCH (a:Person)-[e:KNOWS]->(b:Person) DETACH DELETE e",
        )
        .expect(
            "DETACH DELETE on edge variable should not require DELETE on unrelated edge labels",
        );

    let remaining_knows = query_rows(&engine, &admin, "SELECT COUNT(*) FROM knows_edges");
    assert_eq!(
        remaining_knows,
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(0)])]
    );

    let remaining_partner = query_rows(&engine, &admin, "SELECT COUNT(*) FROM partner_edges");
    assert_eq!(
        remaining_partner,
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(1)])]
    );
}

#[test]
fn cypher_detach_delete_requires_all_connected_edge_labels_for_acl() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             CREATE TABLE friends_edges (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             INSERT INTO people VALUES (1, 'alice'); \
             INSERT INTO people VALUES (2, 'bob'); \
             INSERT INTO knows_edges VALUES (1, 2); \
             INSERT INTO friends_edges VALUES (2, 1); \
             CREATE NODE LABEL Person ON people; \
             CREATE EDGE LABEL KNOWS ON knows_edges SOURCE Person TARGET Person; \
             CREATE EDGE LABEL FRIENDS ON friends_edges SOURCE Person TARGET Person; \
             CREATE ROLE deleter LOGIN; \
             GRANT SELECT ON people TO deleter; \
             GRANT DELETE ON people TO deleter; \
             GRANT DELETE ON knows_edges TO deleter",
        )
        .expect("setup");

    let (deleter_session, _) = engine
        .startup(startup_as("deleter"))
        .expect("deleter startup");

    let err = engine
        .execute_sql(&deleter_session, "MATCH (n:Person {id: 1}) DETACH DELETE n")
        .expect_err("DETACH DELETE should require DELETE on every edge label connected to Person");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn cypher_create_unknown_label_denied_when_roles_are_active() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE writer LOGIN",
        )
        .expect("setup");

    let (writer_session, _) = engine
        .startup(startup_as("writer"))
        .expect("writer startup");

    let err = engine
        .execute_sql(
            &writer_session,
            "CREATE (n:MissingLabel {id: 1, name: 'alice'}) RETURN 1",
        )
        .expect_err("unresolved CREATE label must be denied when RBAC is active");
    let msg = format!("{err}");
    assert!(
        msg.contains("cannot determine backing table") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );
}

#[test]
fn vector_top_k_ids_unqualified_name_denied_with_search_path_schema_collision() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE SCHEMA analytics; \
             CREATE TABLE public.items (id INT NOT NULL, embedding VECTOR(2)); \
             CREATE TABLE analytics.items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO public.items VALUES (1, '[1.0,0.0]'); \
             INSERT INTO analytics.items VALUES (2, '[0.0,1.0]'); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON public.items TO reader; \
             GRANT SELECT ON analytics.items TO reader; \
             GRANT EXECUTE ON FUNCTION vector_top_k_ids(text,text,text,integer) TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    engine
        .execute_sql(&reader_session, "SET search_path TO analytics, public")
        .expect("set search_path");

    let err = engine
        .execute_sql(
            &reader_session,
            "SELECT item_id FROM vector_top_k_ids('items', 'embedding', '[1.0,0.0]', 1) AS v(item_id)",
        )
        .expect_err("unqualified vector_top_k_ids target must be denied when RBAC is active");
    let msg = format!("{err}");
    assert!(msg.contains("schema-qualified"), "unexpected error: {msg}");
}

// SECURITY: Native Cypher MATCH must honor row-level security policies on
// the backing table. Before the fix, the graph executor scanned the table
// directly (scan_table_locked) without invoking the RLS predicate, so a
// reader role with SELECT could see EVERY row through MATCH while the
// equivalent SELECT applied the policy and hid forbidden rows.
#[test]
fn cypher_match_respects_row_level_security_on_backing_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE bob LOGIN; \
             CREATE TABLE secrets (id BIGINT NOT NULL, owner TEXT, secret TEXT); \
             INSERT INTO secrets VALUES (1, 'bob', 'public-info'), \
                                        (2, 'alice', 'top-secret'); \
             CREATE NODE LABEL Secret ON secrets; \
             GRANT SELECT ON secrets TO bob; \
             ALTER TABLE secrets ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY p_owner ON secrets FOR SELECT TO bob \
                 USING (owner = current_user)",
        )
        .expect("setup");

    let (bob_session, _) = engine.startup(startup_as("bob")).expect("bob startup");

    // Sanity: SQL SELECT is correctly filtered by the RLS policy.
    let sql_rows = query_rows(&engine, &bob_session, "SELECT id FROM secrets ORDER BY id");
    assert_eq!(
        sql_rows.len(),
        1,
        "RLS must hide alice's row from bob via SQL"
    );

    // Native Cypher MATCH must also be filtered. Before the fix this
    // returned BOTH rows.
    let cypher_rows = query_rows(
        &engine,
        &bob_session,
        "MATCH (n:Secret) RETURN n.id ORDER BY n.id",
    );
    assert_eq!(
        cypher_rows.len(),
        1,
        "RLS must hide alice's row from bob via Cypher MATCH; \
         got {} rows: {:?}",
        cypher_rows.len(),
        cypher_rows
            .iter()
            .map(|r| r.values.clone())
            .collect::<Vec<_>>()
    );
}
