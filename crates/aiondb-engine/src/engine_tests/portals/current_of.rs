use super::*;

#[test]
fn current_of_missing_cursor_reports_invalid_cursor_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE current_of_missing_cursor (id INT); \
             INSERT INTO current_of_missing_cursor VALUES (1)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");

    let error = engine
        .execute_sql(
            &session,
            "UPDATE current_of_missing_cursor SET id = 2 WHERE CURRENT OF missing_cursor",
        )
        .expect_err("missing cursor should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::InvalidCursorName);
}

#[test]
fn current_of_unpositioned_cursor_reports_invalid_cursor_state() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE current_of_unpositioned_cursor (id INT); \
             INSERT INTO current_of_unpositioned_cursor VALUES (1)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM current_of_unpositioned_cursor ORDER BY id",
        )
        .expect("declare cursor");

    let error = engine
        .execute_sql(
            &session,
            "UPDATE current_of_unpositioned_cursor SET id = 2 WHERE CURRENT OF c",
        )
        .expect_err("unpositioned cursor should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::InvalidCursorState);
}

#[test]
fn current_of_updates_positioned_row_without_visible_ctid() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE current_of_hidden_cursor (id INT); \
             INSERT INTO current_of_hidden_cursor VALUES (1), (2)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM current_of_hidden_cursor ORDER BY id",
        )
        .expect("declare cursor");

    let fetch_results = engine
        .execute_sql(&session, "FETCH NEXT IN c")
        .expect("fetch first row");
    assert_eq!(
        fetch_results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(1)])],
        }]
    );

    let update_results = engine
        .execute_sql(
            &session,
            "UPDATE current_of_hidden_cursor SET id = 20 WHERE CURRENT OF c",
        )
        .expect("update current row");
    assert_eq!(
        update_results,
        vec![StatementResult::Command {
            tag: "UPDATE".to_owned(),
            rows_affected: 1,
        }]
    );

    assert_eq!(
        query_rows(
            &engine,
            &session,
            "SELECT id FROM current_of_hidden_cursor ORDER BY id"
        ),
        vec![
            aiondb_core::Row::new(vec![aiondb_core::Value::Int(2)]),
            aiondb_core::Row::new(vec![aiondb_core::Value::Int(20)]),
        ]
    );
}

#[test]
fn current_of_resolves_unqualified_cursor_relation_via_search_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             SET search_path TO analytics, public; \
             CREATE TABLE current_of_search_path (id INT); \
             INSERT INTO current_of_search_path VALUES (1), (2)",
        )
        .expect("setup search_path table");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM current_of_search_path ORDER BY id",
        )
        .expect("declare cursor");
    engine
        .execute_sql(&session, "FETCH NEXT IN c")
        .expect("position cursor");

    engine
        .execute_sql(
            &session,
            "UPDATE current_of_search_path SET id = 20 WHERE CURRENT OF c",
        )
        .expect("update current of via search_path");

    assert_eq!(
        query_rows(
            &engine,
            &session,
            "SELECT id FROM current_of_search_path ORDER BY id"
        ),
        vec![
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(20)])
        ]
    );
}

#[test]
fn current_of_fetch_all_targets_last_row_without_exposing_hidden_ctid() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE current_of_fetch_all (id INT); \
             INSERT INTO current_of_fetch_all VALUES (1), (2)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM current_of_fetch_all ORDER BY id",
        )
        .expect("declare cursor");

    let fetch_results = engine
        .execute_sql(&session, "FETCH ALL IN c")
        .expect("fetch all");
    assert_eq!(
        fetch_results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![
                aiondb_core::Row::new(vec![aiondb_core::Value::Int(1)]),
                aiondb_core::Row::new(vec![aiondb_core::Value::Int(2)]),
            ],
        }]
    );

    let update_results = engine
        .execute_sql(
            &session,
            "UPDATE current_of_fetch_all SET id = 20 WHERE CURRENT OF c",
        )
        .expect("update last fetched row");
    assert_eq!(
        update_results,
        vec![StatementResult::Command {
            tag: "UPDATE".to_owned(),
            rows_affected: 1,
        }]
    );

    assert_eq!(
        query_rows(
            &engine,
            &session,
            "SELECT id FROM current_of_fetch_all ORDER BY id"
        ),
        vec![
            aiondb_core::Row::new(vec![aiondb_core::Value::Int(1)]),
            aiondb_core::Row::new(vec![aiondb_core::Value::Int(20)]),
        ]
    );
}

#[test]
fn current_of_does_not_cross_update_rows_in_other_tables() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE current_of_t1 (id INT); \
             CREATE TABLE current_of_t2 (id INT); \
             INSERT INTO current_of_t1 VALUES (1); \
             INSERT INTO current_of_t2 VALUES (10)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM current_of_t1",
        )
        .expect("declare cursor");
    engine
        .execute_sql(&session, "FETCH NEXT IN c")
        .expect("position cursor");

    let update_results = engine
        .execute_sql(
            &session,
            "UPDATE current_of_t2 SET id = 99 WHERE CURRENT OF c",
        )
        .expect("cross-table update should be a no-op");
    assert_eq!(
        update_results,
        vec![StatementResult::Command {
            tag: "UPDATE".to_owned(),
            rows_affected: 0,
        }]
    );

    assert_eq!(
        query_rows(&engine, &session, "SELECT id FROM current_of_t2"),
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(10)])]
    );
}

#[test]
fn current_of_after_move_forward_updates_last_moved_row() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE current_of_move_forward (id INT); \
             INSERT INTO current_of_move_forward VALUES (1), (2), (3)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM current_of_move_forward ORDER BY id",
        )
        .expect("declare cursor");

    let move_results = engine
        .execute_sql(&session, "MOVE 2 IN c")
        .expect("move cursor");
    assert_eq!(
        move_results,
        vec![StatementResult::Command {
            tag: "MOVE 2".to_owned(),
            rows_affected: 0,
        }]
    );

    let update_results = engine
        .execute_sql(
            &session,
            "UPDATE current_of_move_forward SET id = 20 WHERE CURRENT OF c",
        )
        .expect("update moved-to row");
    assert_eq!(
        update_results,
        vec![StatementResult::Command {
            tag: "UPDATE".to_owned(),
            rows_affected: 1,
        }]
    );

    assert_eq!(
        query_rows(
            &engine,
            &session,
            "SELECT id FROM current_of_move_forward ORDER BY id"
        ),
        vec![
            aiondb_core::Row::new(vec![aiondb_core::Value::Int(1)]),
            aiondb_core::Row::new(vec![aiondb_core::Value::Int(3)]),
            aiondb_core::Row::new(vec![aiondb_core::Value::Int(20)]),
        ]
    );
}

#[test]
fn current_of_join_cursor_reports_feature_not_supported() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE current_of_join_lhs (id INT); \
             CREATE TABLE current_of_join_rhs (id INT); \
             INSERT INTO current_of_join_lhs VALUES (1); \
             INSERT INTO current_of_join_rhs VALUES (1)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR \
             SELECT l.id FROM current_of_join_lhs l \
             JOIN current_of_join_rhs r ON l.id = r.id",
        )
        .expect("declare cursor");
    engine
        .execute_sql(&session, "FETCH NEXT IN c")
        .expect("position cursor");

    let error = engine
        .execute_sql(
            &session,
            "UPDATE current_of_join_lhs SET id = 2 WHERE CURRENT OF c",
        )
        .expect_err("join cursor should not support CURRENT OF");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(
        error
            .report()
            .message
            .contains("CURRENT OF requires a simple cursor over a base table"),
        "expected simple-cursor rejection, got: {}",
        error.report().message
    );
}

#[test]
fn current_of_delete_removes_positioned_row_without_visible_ctid() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE current_of_delete_cursor (id INT); \
             INSERT INTO current_of_delete_cursor VALUES (1), (2)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM current_of_delete_cursor ORDER BY id",
        )
        .expect("declare cursor");
    engine
        .execute_sql(&session, "FETCH NEXT IN c")
        .expect("position cursor");

    let delete_results = engine
        .execute_sql(
            &session,
            "DELETE FROM current_of_delete_cursor WHERE CURRENT OF c",
        )
        .expect("delete current row");
    assert_eq!(
        delete_results,
        vec![StatementResult::Command {
            tag: "DELETE".to_owned(),
            rows_affected: 1,
        }]
    );

    assert_eq!(
        query_rows(
            &engine,
            &session,
            "SELECT id FROM current_of_delete_cursor ORDER BY id"
        ),
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(2)])]
    );
}

#[test]
fn execute_portal_rewrites_current_of_for_prepared_update() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE current_of_portal_update (id INT); \
             INSERT INTO current_of_portal_update VALUES (1), (2)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM current_of_portal_update ORDER BY id",
        )
        .expect("declare cursor");
    engine
        .execute_sql(&session, "FETCH NEXT IN c")
        .expect("position cursor");

    engine
        .prepare(
            &session,
            "upd_current".to_owned(),
            "UPDATE current_of_portal_update SET id = $1 WHERE CURRENT OF c".to_owned(),
        )
        .expect("prepare update current of");
    engine
        .bind(
            &session,
            "upd_current_portal".to_owned(),
            "upd_current".to_owned(),
            vec![aiondb_core::Value::Int(20)],
        )
        .expect("bind update current of");

    let batch = engine
        .execute_portal(&session, "upd_current_portal", 0)
        .expect("execute update current of");
    assert_eq!(batch.tag, "UPDATE");
    assert_eq!(batch.rows_affected, 1);

    assert_eq!(
        query_rows(
            &engine,
            &session,
            "SELECT id FROM current_of_portal_update ORDER BY id"
        ),
        vec![
            aiondb_core::Row::new(vec![aiondb_core::Value::Int(2)]),
            aiondb_core::Row::new(vec![aiondb_core::Value::Int(20)]),
        ]
    );
}

#[test]
fn execute_portal_explain_current_of_restores_cursor_name_in_plan_text() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE current_of_portal_explain (id INT); \
             INSERT INTO current_of_portal_explain VALUES (1), (2)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM current_of_portal_explain ORDER BY id",
        )
        .expect("declare cursor");
    engine
        .execute_sql(&session, "FETCH NEXT IN c")
        .expect("position cursor");

    engine
        .prepare(
            &session,
            "explain_current".to_owned(),
            "EXPLAIN UPDATE current_of_portal_explain SET id = 20 WHERE CURRENT OF c".to_owned(),
        )
        .expect("prepare explain current of");
    engine
        .bind(
            &session,
            "explain_current_portal".to_owned(),
            "explain_current".to_owned(),
            vec![],
        )
        .expect("bind explain current of");

    let batch = engine
        .execute_portal(&session, "explain_current_portal", 0)
        .expect("execute explain current of");
    assert_eq!(batch.tag, "EXPLAIN");
    assert!(
        batch.rows.iter().any(|row| {
            matches!(
                row.values.as_slice(),
                [aiondb_core::Value::Text(line)] if line.contains("CURRENT OF c")
            )
        }),
        "expected plan rows to restore CURRENT OF cursor name, got {:?}",
        batch.rows
    );
}
