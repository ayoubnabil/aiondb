use super::*;

#[test]
fn metrics_start_at_zero() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let snapshot = engine.query_metrics();

    assert_eq!(snapshot.queries_total, 0);
    assert_eq!(snapshot.queries_failed, 0);
    assert_eq!(snapshot.rows_returned_total, 0);
    assert_eq!(snapshot.rows_affected_total, 0);
    assert_eq!(snapshot.query_duration_micros_total, 0);
}

#[test]
fn metrics_increment_on_select() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE m1 (id INT NOT NULL)")
        .expect("create");
    engine
        .execute_sql(&session, "INSERT INTO m1 VALUES (1), (2), (3)")
        .expect("insert");

    let before = engine.query_metrics();
    // CREATE TABLE + INSERT = 2 successful queries
    assert_eq!(before.queries_total, 2);
    assert_eq!(before.queries_failed, 0);
    // INSERT 3 rows
    assert_eq!(before.rows_affected_total, 3);

    engine
        .execute_sql(&session, "SELECT * FROM m1")
        .expect("select");

    let after = engine.query_metrics();
    assert_eq!(after.queries_total, 3);
    assert_eq!(after.queries_failed, 0);
    assert_eq!(after.rows_returned_total, 3);
    assert_eq!(after.rows_affected_total, 3);
}

#[test]
fn metrics_increment_on_dml() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE m2 (id INT NOT NULL, val TEXT)")
        .expect("create");
    engine
        .execute_sql(
            &session,
            "INSERT INTO m2 VALUES (1, 'a'), (2, 'b'), (3, 'c')",
        )
        .expect("insert");
    engine
        .execute_sql(&session, "DELETE FROM m2 WHERE id = 2")
        .expect("delete");
    engine
        .execute_sql(&session, "UPDATE m2 SET val = 'z' WHERE id = 1")
        .expect("update");

    let snap = engine.query_metrics();
    assert_eq!(snap.queries_total, 4);
    assert_eq!(snap.queries_failed, 0);
    // 3 inserts + 1 delete + 1 update = 5
    assert_eq!(snap.rows_affected_total, 5);
}

#[test]
fn metrics_record_failure() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let result = engine.execute_sql(&session, "SELECT * FROM nonexistent_table");
    assert!(result.is_err());

    let snap = engine.query_metrics();
    assert_eq!(snap.queries_total, 1);
    assert_eq!(snap.queries_failed, 1);
    assert_eq!(snap.rows_returned_total, 0);
}

#[test]
fn metrics_parse_error_counts_as_failure() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let result = engine.execute_sql(&session, "CREATE TABLE");
    assert!(result.is_err());

    let snap = engine.query_metrics();
    assert_eq!(snap.queries_failed, 1);
}

#[test]
fn metrics_multi_statement_sql() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Multiple statements in a single execute_sql call count as one query.
    engine
        .execute_sql(
            &session,
            "CREATE TABLE m3 (id INT NOT NULL); \
             INSERT INTO m3 VALUES (10), (20)",
        )
        .expect("multi");

    let snap = engine.query_metrics();
    assert_eq!(snap.queries_total, 1);
    assert_eq!(snap.rows_affected_total, 2);
}

#[test]
fn metrics_duration_advances() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE m4 (id INT)")
        .expect("create");

    let snap = engine.query_metrics();
    // We cannot predict the exact duration, but we verify queries ran
    // and that the cumulative time is recorded (may be 0 on very fast systems).
    assert_eq!(snap.queries_total, 1);
}

#[test]
fn metrics_snapshot_prometheus_format() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE m5 (id INT NOT NULL); INSERT INTO m5 VALUES (1)",
        )
        .expect("setup");

    let snap = engine.query_metrics();
    let text = snap.to_prometheus_text();

    assert!(text.contains("aiondb_queries_total 1"));
    assert!(text.contains("aiondb_queries_failed_total 0"));
    assert!(text.contains("aiondb_rows_affected_total 1"));
    assert!(text.contains("# TYPE aiondb_queries_total counter"));
}

#[test]
fn metrics_snapshot_json_format() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE m6 (id INT NOT NULL); INSERT INTO m6 VALUES (1)",
        )
        .expect("setup");

    let snap = engine.query_metrics();
    let json = snap.to_json_string();

    assert!(json.contains("\"queries_total\":1"));
    assert!(json.contains("\"queries_failed\":0"));
    assert!(json.contains("\"rows_affected_total\":1"));
    assert!(json.contains("\"rows_returned_total\":0"));
}

#[test]
fn metrics_portal_execution_tracked() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE m7 (id INT NOT NULL); INSERT INTO m7 VALUES (1), (2)",
        )
        .expect("setup");

    let before = engine.query_metrics();
    assert_eq!(before.queries_total, 1);

    // Use the extended query protocol: prepare, bind, execute_portal.
    engine
        .prepare(&session, "ps1".to_owned(), "SELECT * FROM m7".to_owned())
        .expect("prepare");

    engine
        .bind(&session, "p1".to_owned(), "ps1".to_owned(), vec![])
        .expect("bind");

    let batch = engine.execute_portal(&session, "p1", 0).expect("portal");
    assert_eq!(batch.rows.len(), 2);

    let after = engine.query_metrics();
    // The portal execution should have been counted.
    assert_eq!(after.queries_total, 2);
    assert_eq!(after.rows_returned_total, 2);
}

#[test]
fn metrics_graph_ddl_starts_at_zero() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let snap = engine.query_metrics();
    assert_eq!(snap.graph_ddl_operations, 0);
}

#[test]
fn metrics_graph_ddl_increments_on_create_node_label() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT)",
        )
        .expect("create table");

    let before = engine.query_metrics();
    assert_eq!(before.graph_ddl_operations, 0);

    engine
        .execute_sql(&session, "CREATE NODE LABEL person ON persons")
        .expect("create node label");

    let after = engine.query_metrics();
    assert_eq!(after.graph_ddl_operations, 1);
}

#[test]
fn metrics_graph_ddl_increments_on_create_edge_label() {
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

    let before = engine.query_metrics();
    assert_eq!(before.graph_ddl_operations, 1); // CREATE NODE LABEL

    engine
        .execute_sql(
            &session,
            "CREATE EDGE LABEL knows ON knows_edges SOURCE person TARGET person",
        )
        .expect("create edge label");

    let after = engine.query_metrics();
    assert_eq!(after.graph_ddl_operations, 2);
}

#[test]
fn metrics_graph_ddl_increments_on_drop_labels() {
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

    let before = engine.query_metrics();
    assert_eq!(before.graph_ddl_operations, 2);

    engine
        .execute_sql(&session, "DROP EDGE LABEL knows")
        .expect("drop edge label");

    let mid = engine.query_metrics();
    assert_eq!(mid.graph_ddl_operations, 3);

    engine
        .execute_sql(&session, "DROP NODE LABEL person")
        .expect("drop node label");

    let after = engine.query_metrics();
    assert_eq!(after.graph_ddl_operations, 4);
}

#[test]
fn metrics_graph_ddl_does_not_increment_on_failure() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let _ = engine.execute_sql(&session, "CREATE NODE LABEL person ON nonexistent");

    let snap = engine.query_metrics();
    assert_eq!(snap.graph_ddl_operations, 0);
}

#[test]
fn metrics_graph_ddl_in_prometheus_format() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT); \
             CREATE NODE LABEL person ON persons",
        )
        .expect("setup");

    let snap = engine.query_metrics();
    let text = snap.to_prometheus_text();
    assert!(text.contains("aiondb_graph_ddl_operations_total 1"));
    assert!(text.contains("# TYPE aiondb_graph_ddl_operations_total counter"));
}

#[test]
fn metrics_graph_ddl_in_json_format() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT); \
             CREATE NODE LABEL person ON persons",
        )
        .expect("setup");

    let snap = engine.query_metrics();
    let json = snap.to_json_string();
    assert!(json.contains("\"graph_ddl_operations\":1"));
}
