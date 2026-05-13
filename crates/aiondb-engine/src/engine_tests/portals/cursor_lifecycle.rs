use super::*;

#[test]
fn executes_portal_control_statements() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "txn".to_owned(), "BEGIN".to_owned())
        .expect("prepare");
    engine
        .bind(&session, "p1".to_owned(), "txn".to_owned(), Vec::new())
        .expect("bind");

    let batch = engine
        .execute_portal(&session, "p1", 0)
        .expect("execute portal");
    assert_eq!(
        batch,
        PortalBatch {
            columns: Vec::new(),
            rows: Vec::new(),
            tag: "BEGIN".to_owned(),
            rows_affected: 0,
            exhausted: true,
        }
    );
}

#[test]
fn commit_closes_portals_created_inside_transaction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "s1".to_owned(), "SELECT 1".to_owned())
        .expect("prepare");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .bind(&session, "p1".to_owned(), "s1".to_owned(), vec![])
        .expect("bind");

    engine.execute_sql(&session, "COMMIT").expect("commit");

    let error = engine
        .execute_portal(&session, "p1", 0)
        .expect_err("portal should be closed at transaction end");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
    assert!(
        error.report().message.contains("unknown portal"),
        "expected unknown portal error, got: {}",
        error.report().message
    );
    engine
        .describe_statement(&session, "s1")
        .expect("prepared statement should survive commit");
}

#[test]
fn rollback_closes_compat_cursors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE cursor_items_rb (id INT); \
             INSERT INTO cursor_items_rb VALUES (1), (2)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM cursor_items_rb ORDER BY id",
        )
        .expect("declare");

    engine.execute_sql(&session, "ROLLBACK").expect("rollback");

    let error = engine
        .execute_sql(&session, "FETCH ALL IN c")
        .expect_err("cursor should be closed at transaction rollback");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::InvalidCursorName);
    assert!(
        error
            .report()
            .message
            .contains("cursor \"c\" does not exist"),
        "expected missing cursor error, got: {}",
        error.report().message
    );
}

#[test]
fn portal_execution_respects_failed_transaction_state_and_commit_rolls_back() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE portal_failed_tx (id INT NOT NULL)")
        .expect("create table");
    engine.execute_sql(&session, "BEGIN").expect("begin");

    engine
        .prepare(
            &session,
            "bad_insert".to_owned(),
            "INSERT INTO portal_failed_tx VALUES (NULL)".to_owned(),
        )
        .expect("prepare bad insert");
    engine
        .bind(
            &session,
            "p_bad".to_owned(),
            "bad_insert".to_owned(),
            vec![],
        )
        .expect("bind bad insert");
    let error = engine
        .execute_portal(&session, "p_bad", 0)
        .expect_err("bad insert portal should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::NotNullViolation);

    engine
        .prepare(&session, "sel".to_owned(), "SELECT 1".to_owned())
        .expect("prepare select");
    engine
        .bind(&session, "p_sel".to_owned(), "sel".to_owned(), vec![])
        .expect("bind select");
    let aborted_error = engine
        .execute_portal(&session, "p_sel", 0)
        .expect_err("portal should be blocked while transaction is aborted");
    assert_eq!(
        aborted_error.sqlstate(),
        aiondb_core::SqlState::InFailedSqlTransaction
    );

    engine
        .prepare(&session, "commit_stmt".to_owned(), "COMMIT".to_owned())
        .expect("prepare commit");
    engine
        .bind(
            &session,
            "p_commit".to_owned(),
            "commit_stmt".to_owned(),
            vec![],
        )
        .expect("bind commit");
    let batch = engine
        .execute_portal(&session, "p_commit", 0)
        .expect("commit portal should terminate failed transaction");
    assert_eq!(batch.tag, "ROLLBACK");

    let results = engine
        .execute_sql(&session, "SELECT id FROM portal_failed_tx")
        .expect("select after portal rollback");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected query result");
    };
    assert!(rows.is_empty(), "portal commit should roll back writes");
}

#[test]
fn simple_fetch_drains_pending_notices_before_cursor_result() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE cursor_notice_items (id INT); \
             INSERT INTO cursor_notice_items VALUES (1), (2)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM cursor_notice_items ORDER BY id",
        )
        .expect("declare");
    engine
        .with_session_mut(&session, |record| {
            record
                .pending_notices
                .push("notice before fetch".to_owned());
            Ok(())
        })
        .expect("inject pending notice");

    let results = engine
        .execute_sql(&session, "FETCH ALL IN c")
        .expect("fetch should succeed");

    assert!(matches!(
        &results[0],
        StatementResult::Notice { message } if message == "notice before fetch"
    ));
    assert!(matches!(
        &results[1],
        StatementResult::Query { rows, .. }
            if rows.len() == 2
                && rows[0].values == vec![Value::Int(1)]
                && rows[1].values == vec![Value::Int(2)]
    ));
}

#[test]
fn rollback_to_savepoint_closes_portals_created_inside_savepoint() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "s1".to_owned(), "SELECT 1".to_owned())
        .expect("prepare");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("create savepoint");
    engine
        .bind(&session, "p1".to_owned(), "s1".to_owned(), vec![])
        .expect("bind portal inside savepoint");

    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("rollback to savepoint");

    let error = engine
        .execute_portal(&session, "p1", 0)
        .expect_err("portal created after savepoint should be closed");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
    assert!(
        error.report().message.contains("unknown portal"),
        "expected unknown portal error, got: {}",
        error.report().message
    );

    engine
        .describe_statement(&session, "s1")
        .expect("prepared statement should survive savepoint rollback");
}

#[test]
fn rollback_to_savepoint_closes_compat_cursors_and_releases_hidden_statement_slot() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_prepared_statements = 1;
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits = limits;
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("create savepoint");
    engine
        .execute_sql(&session, "DECLARE c1 CURSOR FOR SELECT 1")
        .expect("declare c1 inside savepoint");

    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("rollback to savepoint");

    let error = engine
        .execute_sql(&session, "FETCH NEXT IN c1")
        .expect_err("cursor created after savepoint should be closed");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::InvalidCursorName);
    assert!(
        error
            .report()
            .message
            .contains("cursor \"c1\" does not exist"),
        "expected missing cursor error, got: {}",
        error.report().message
    );
    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("recover transaction after missing cursor error");

    engine
        .execute_sql(&session, "DECLARE c2 CURSOR FOR SELECT 1")
        .expect("hidden statement slot should be available after rollback");
}

#[test]
fn compat_cursor_commit_releases_hidden_statement_slot() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_prepared_statements = 1;
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits = limits;
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin 1");
    engine
        .execute_sql(&session, "DECLARE c1 CURSOR FOR SELECT 1")
        .expect("declare c1");
    engine.execute_sql(&session, "COMMIT").expect("commit 1");

    engine.execute_sql(&session, "BEGIN").expect("begin 2");
    engine
        .execute_sql(&session, "DECLARE c2 CURSOR FOR SELECT 1")
        .expect("declare c2 after c1 cleanup");

    let error = engine
        .execute_sql(&session, "FETCH NEXT IN c1")
        .expect_err("old cursor should be gone");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::InvalidCursorName);
    assert!(
        error
            .report()
            .message
            .contains("cursor \"c1\" does not exist"),
        "expected missing cursor error, got: {}",
        error.report().message
    );
}

#[test]
fn close_missing_compat_cursor_reports_invalid_cursor_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "CLOSE missing_cursor")
        .expect_err("missing cursor close should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::InvalidCursorName);
    assert!(
        error
            .report()
            .message
            .contains("cursor \"missing_cursor\" does not exist"),
        "expected missing cursor error, got: {}",
        error.report().message
    );
}

#[test]
fn fetch_zero_returns_no_rows_and_keeps_cursor_position() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE fetch_zero_items (id INT); \
             INSERT INTO fetch_zero_items VALUES (1), (2)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM fetch_zero_items ORDER BY id",
        )
        .expect("declare cursor");

    let zero_results = engine
        .execute_sql(&session, "FETCH 0 IN c")
        .expect("fetch zero should succeed");
    assert_eq!(
        zero_results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![],
        }]
    );

    let next_results = engine
        .execute_sql(&session, "FETCH NEXT IN c")
        .expect("next fetch should still start at first row");
    assert_eq!(
        next_results,
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
}

#[test]
fn move_zero_reports_zero_and_keeps_cursor_position() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE move_zero_items (id INT); \
             INSERT INTO move_zero_items VALUES (1), (2)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM move_zero_items ORDER BY id",
        )
        .expect("declare cursor");

    let move_results = engine
        .execute_sql(&session, "MOVE 0 IN c")
        .expect("move zero should succeed");
    assert_eq!(
        move_results,
        vec![StatementResult::Command {
            tag: "MOVE 0".to_owned(),
            rows_affected: 0,
        }]
    );

    let next_results = engine
        .execute_sql(&session, "FETCH NEXT IN c")
        .expect("next fetch should still start at first row");
    assert_eq!(
        next_results,
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
}

#[test]
fn malformed_fetch_with_trailing_garbage_is_rejected_without_advancing_cursor() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE fetch_garbage_items (id INT); \
             INSERT INTO fetch_garbage_items VALUES (1), (2)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM fetch_garbage_items ORDER BY id",
        )
        .expect("declare cursor");
    engine
        .execute_sql(&session, "SAVEPOINT sp_fetch")
        .expect("savepoint");

    let error = engine
        .execute_sql(&session, "FETCH NEXT IN c trailing")
        .expect_err("malformed fetch should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: FETCH"));
    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp_fetch")
        .expect("recover transaction after malformed fetch");

    let next_results = engine
        .execute_sql(&session, "FETCH NEXT IN c")
        .expect("cursor should remain usable and unadvanced");
    assert_eq!(
        next_results,
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
}

#[test]
fn malformed_move_with_trailing_garbage_is_rejected_without_advancing_cursor() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE move_garbage_items (id INT); \
             INSERT INTO move_garbage_items VALUES (1), (2)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM move_garbage_items ORDER BY id",
        )
        .expect("declare cursor");
    engine
        .execute_sql(&session, "SAVEPOINT sp_move")
        .expect("savepoint");

    let error = engine
        .execute_sql(&session, "MOVE 1 IN c trailing")
        .expect_err("malformed move should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: MOVE"));
    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp_move")
        .expect("recover transaction after malformed move");

    let next_results = engine
        .execute_sql(&session, "FETCH NEXT IN c")
        .expect("cursor should remain usable and unadvanced");
    assert_eq!(
        next_results,
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
}

#[test]
fn malformed_close_with_trailing_garbage_is_rejected_without_closing_cursor() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE close_garbage_items (id INT); \
             INSERT INTO close_garbage_items VALUES (1), (2)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM close_garbage_items ORDER BY id",
        )
        .expect("declare cursor");
    engine
        .execute_sql(&session, "SAVEPOINT sp_close")
        .expect("savepoint");

    let error = engine
        .execute_sql(&session, "CLOSE c trailing")
        .expect_err("malformed close should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: CLOSE"));
    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp_close")
        .expect("recover transaction after malformed close");

    let next_results = engine
        .execute_sql(&session, "FETCH NEXT IN c")
        .expect("cursor should remain open");
    assert_eq!(
        next_results,
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
}

#[test]
fn malformed_declare_is_rejected_instead_of_succeeding_as_noop() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DECLARE c CURSOR")
        .expect_err("malformed declare should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DECLARE"));
}

#[test]
fn declare_cursor_outside_transaction_is_rejected_immediately() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DECLARE c CURSOR FOR SELECT 1")
        .expect_err("declare outside transaction should fail immediately");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::NoActiveSqlTransaction
    );
    assert!(
        error
            .report()
            .message
            .contains("DECLARE CURSOR can only be used in transaction block"),
        "expected transaction block error, got: {}",
        error.report().message
    );
}

#[test]
fn execute_portal_declare_cursor_outside_transaction_is_rejected_immediately() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "decl_outside_txn".to_owned(),
            "DECLARE c CURSOR FOR SELECT 1".to_owned(),
        )
        .expect("prepare declare");
    engine
        .bind(
            &session,
            "p_decl_outside_txn".to_owned(),
            "decl_outside_txn".to_owned(),
            Vec::new(),
        )
        .expect("bind declare");

    let error = engine
        .execute_portal(&session, "p_decl_outside_txn", 0)
        .expect_err("portal declare outside transaction should fail immediately");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::NoActiveSqlTransaction
    );
    assert!(
        error
            .report()
            .message
            .contains("DECLARE CURSOR can only be used in transaction block"),
        "expected transaction block error, got: {}",
        error.report().message
    );
}

#[test]
fn describe_fetch_on_missing_compat_cursor_reports_invalid_cursor_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "fetch_missing_cursor".to_owned(),
            "FETCH ALL IN missing_cursor".to_owned(),
        )
        .expect("prepare fetch should succeed before describe");

    let error = engine
        .describe_statement(&session, "fetch_missing_cursor")
        .expect_err("describe fetch on missing cursor should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::InvalidCursorName);
    assert!(
        error
            .report()
            .message
            .contains("cursor \"missing_cursor\" does not exist"),
        "expected missing cursor error, got: {}",
        error.report().message
    );
}

#[test]
fn describe_close_on_missing_compat_cursor_reports_invalid_cursor_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "close_missing_cursor".to_owned(),
            "CLOSE missing_cursor".to_owned(),
        )
        .expect("prepare close should succeed before describe");

    let error = engine
        .describe_statement(&session, "close_missing_cursor")
        .expect_err("describe close on missing cursor should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::InvalidCursorName);
    assert!(
        error
            .report()
            .message
            .contains("cursor \"missing_cursor\" does not exist"),
        "expected missing cursor error, got: {}",
        error.report().message
    );
}

#[test]
fn describe_move_on_missing_compat_cursor_reports_invalid_cursor_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "move_missing_cursor".to_owned(),
            "MOVE 1 IN missing_cursor".to_owned(),
        )
        .expect("prepare move should succeed before describe");

    let error = engine
        .describe_statement(&session, "move_missing_cursor")
        .expect_err("describe move on missing cursor should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::InvalidCursorName);
    assert!(
        error
            .report()
            .message
            .contains("cursor \"missing_cursor\" does not exist"),
        "expected missing cursor error, got: {}",
        error.report().message
    );
}

#[test]
fn describe_fetch_forward_count_reports_row_description() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE fetch_forward_desc (id INT);
             INSERT INTO fetch_forward_desc VALUES (1), (2), (3)",
        )
        .expect("seed fetch forward table");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM fetch_forward_desc ORDER BY id",
        )
        .expect("declare cursor");
    engine
        .prepare(
            &session,
            "fetch_forward_cursor".to_owned(),
            "FETCH FORWARD 2 IN c".to_owned(),
        )
        .expect("prepare fetch forward should succeed");

    let desc = engine
        .describe_statement(&session, "fetch_forward_cursor")
        .expect("describe fetch forward should succeed");
    assert_eq!(desc.result_columns.len(), 1);
    assert_eq!(desc.result_columns[0].name, "id");
    assert_eq!(desc.result_columns[0].data_type, aiondb_core::DataType::Int);
}

#[test]
fn execute_portal_with_missing_backing_statement_reports_unknown_portal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "s_stale".to_owned(), "SELECT 1".to_owned())
        .expect("prepare");
    engine
        .bind(&session, "p_stale".to_owned(), "s_stale".to_owned(), vec![])
        .expect("bind");

    engine
        .with_session_mut(&session, |record| {
            record.prepared_statements.remove("s_stale");
            Ok(())
        })
        .expect("corrupt prepared state");

    let error = engine
        .execute_portal(&session, "p_stale", 0)
        .expect_err("stale portal should fail cleanly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
    assert!(
        error.report().message.contains("unknown portal"),
        "expected unknown portal error, got: {}",
        error.report().message
    );
}

#[test]
fn describe_portal_with_missing_backing_statement_reports_unknown_portal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "s_stale_desc".to_owned(), "SELECT 1".to_owned())
        .expect("prepare");
    engine
        .bind(
            &session,
            "p_stale_desc".to_owned(),
            "s_stale_desc".to_owned(),
            vec![],
        )
        .expect("bind");

    engine
        .with_session_mut(&session, |record| {
            record.prepared_statements.remove("s_stale_desc");
            Ok(())
        })
        .expect("corrupt prepared state");

    let error = engine
        .describe_portal(&session, "p_stale_desc")
        .expect_err("stale portal describe should fail cleanly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
    assert!(
        error.report().message.contains("unknown portal"),
        "expected unknown portal error, got: {}",
        error.report().message
    );
}

#[test]
fn compat_cursor_with_missing_backing_statement_reports_invalid_cursor_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "DECLARE c_stale CURSOR FOR SELECT 1")
        .expect("declare");

    engine
        .with_session_mut(&session, |record| {
            let statement_name = record
                .portals
                .get("c_stale")
                .expect("cursor portal should exist")
                .statement_name
                .clone();
            record.prepared_statements.remove(&statement_name);
            Ok(())
        })
        .expect("corrupt cursor prepared state");

    let error = engine
        .execute_sql(&session, "FETCH ALL IN c_stale")
        .expect_err("stale cursor should fail cleanly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::InvalidCursorName);
    assert!(
        error
            .report()
            .message
            .contains("cursor \"c_stale\" does not exist"),
        "expected missing cursor error, got: {}",
        error.report().message
    );
}

#[test]
fn describe_declare_cursor_with_non_rowset_query_reports_feature_not_supported() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE declare_non_rowset (id INT)")
        .expect("create table");
    engine.execute_sql(&session, "BEGIN").expect("begin");

    engine
        .prepare(
            &session,
            "declare_non_rowset_stmt".to_owned(),
            "DECLARE c CURSOR FOR INSERT INTO declare_non_rowset VALUES (1)".to_owned(),
        )
        .expect("prepare declare cursor should succeed before describe");

    let error = engine
        .describe_statement(&session, "declare_non_rowset_stmt")
        .expect_err("describe declare cursor should fail for non-rowset query");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(
        error
            .report()
            .message
            .contains("DECLARE CURSOR only supports row-returning statements"),
        "expected non-rowset declare error, got: {}",
        error.report().message
    );
}

#[test]
fn declare_cursor_rejects_non_rowset_statement_without_leaking_hidden_statement_slot() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_prepared_statements = 1;
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits = limits;
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE cursor_decl_non_rowset (id INT)")
        .expect("create table");
    engine.execute_sql(&session, "BEGIN").expect("begin");

    let error = engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR INSERT INTO cursor_decl_non_rowset VALUES (1)",
        )
        .expect_err("non-rowset declare should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(
        error
            .report()
            .message
            .contains("DECLARE CURSOR only supports row-returning statements"),
        "expected row-returning error, got: {}",
        error.report().message
    );
    engine
        .execute_sql(&session, "ROLLBACK")
        .expect("recover transaction after failed declare");

    engine
        .prepare(&session, "s_after_decl".to_owned(), "SELECT 1".to_owned())
        .expect("hidden statement slot should be released after failed declare");

    let fetch_error = engine
        .execute_sql(&session, "FETCH ALL IN c")
        .expect_err("failed declare should not leave cursor behind");
    assert_eq!(
        fetch_error.sqlstate(),
        aiondb_core::SqlState::InvalidCursorName
    );
}

#[test]
fn declare_cursor_bind_failure_does_not_leak_hidden_statement_slot() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_prepared_statements = 1;
    limits.max_portals = 0;
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits = limits;
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");

    let error = engine
        .execute_sql(&session, "DECLARE c CURSOR FOR SELECT 1")
        .expect_err("declare should fail when no portal slots are available");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
    assert!(
        error
            .report()
            .message
            .contains("maximum number of portals reached"),
        "expected max portals error, got: {}",
        error.report().message
    );
    engine
        .execute_sql(&session, "ROLLBACK")
        .expect("recover transaction after failed declare bind");

    engine
        .prepare(&session, "s_after_bind".to_owned(), "SELECT 1".to_owned())
        .expect("hidden statement slot should be released after failed bind");

    let fetch_error = engine
        .execute_sql(&session, "FETCH ALL IN c")
        .expect_err("failed declare should not leave cursor behind");
    assert_eq!(
        fetch_error.sqlstate(),
        aiondb_core::SqlState::InvalidCursorName
    );
}
