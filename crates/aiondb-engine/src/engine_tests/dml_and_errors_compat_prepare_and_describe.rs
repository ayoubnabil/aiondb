use super::*;

#[test]
fn compat_prepare_rejects_duplicate_statement_names() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt AS SELECT 7")
        .expect("first prepare should succeed");

    let error = engine
        .execute_sql(&session, "PREPARE stmt AS SELECT 8")
        .expect_err("duplicate compat prepare should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::DuplicateObject);
    assert!(error
        .report()
        .message
        .contains("prepared statement \"stmt\" already exists"));
}

#[test]
fn compat_prepare_rejects_name_used_by_protocol_prepare() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "stmt".to_owned(), "SELECT 7".to_owned())
        .expect("protocol prepare should succeed");

    let error = engine
        .execute_sql(&session, "PREPARE stmt AS SELECT 8")
        .expect_err("compat prepare should reject protocol statement name");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::DuplicateObject);
    assert!(error
        .report()
        .message
        .contains("prepared statement \"stmt\" already exists"));
}

#[test]
fn protocol_prepare_rejects_name_used_by_compat_prepare() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt AS SELECT 7")
        .expect("compat prepare should succeed");

    let error = engine
        .prepare(&session, "stmt".to_owned(), "SELECT 8".to_owned())
        .expect_err("protocol prepare should reject compat statement name");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::DuplicateObject);
    assert!(error
        .report()
        .message
        .contains("prepared statement \"stmt\" already exists"));
}

#[test]
fn describe_execute_on_missing_compat_prepared_statement_reports_undefined_object() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "exec_missing_stmt".to_owned(),
            "EXECUTE missing_stmt(41)".to_owned(),
        )
        .expect("prepare execute should succeed before describe");

    let error = engine
        .describe_statement(&session, "exec_missing_stmt")
        .expect_err("describe execute on missing compat prepared statement should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
    assert!(
        error
            .report()
            .message
            .contains("prepared statement \"missing_stmt\" does not exist"),
        "expected missing prepared statement error, got: {}",
        error.report().message
    );
}

#[test]
fn describe_execute_with_string_comma_arguments_reports_row_description() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "PREPARE stmt(int, text) AS SELECT $1 AS id, $2 AS note",
        )
        .expect("compat prepare should succeed");
    engine
        .prepare(
            &session,
            "exec_stmt_with_string_comma".to_owned(),
            "EXECUTE stmt(1, 'a,b')".to_owned(),
        )
        .expect("prepare execute should succeed before describe");

    let desc = engine
        .describe_statement(&session, "exec_stmt_with_string_comma")
        .expect("describe execute with string comma should succeed");
    assert_eq!(desc.result_columns.len(), 2);
    assert_eq!(desc.result_columns[0].name, "id");
    assert_eq!(desc.result_columns[0].data_type, aiondb_core::DataType::Int);
    assert_eq!(desc.result_columns[1].name, "note");
    assert_eq!(
        desc.result_columns[1].data_type,
        aiondb_core::DataType::Text
    );
}

#[test]
fn describe_execute_wrapped_missing_close_reports_invalid_cursor_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE close_stmt AS CLOSE missing_cursor")
        .expect("compat prepare close should succeed");
    engine
        .prepare(
            &session,
            "exec_close_missing_cursor".to_owned(),
            "EXECUTE close_stmt".to_owned(),
        )
        .expect("prepare execute should succeed before describe");

    let error = engine
        .describe_statement(&session, "exec_close_missing_cursor")
        .expect_err("describe execute wrapped missing close should fail");
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
fn describe_execute_wrapped_missing_deallocate_reports_undefined_object() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE dealloc_stmt AS DEALLOCATE stmt")
        .expect("compat prepare deallocate should succeed");
    engine
        .prepare(
            &session,
            "exec_dealloc_missing_stmt".to_owned(),
            "EXECUTE dealloc_stmt".to_owned(),
        )
        .expect("prepare execute should succeed before describe");

    let error = engine
        .describe_statement(&session, "exec_dealloc_missing_stmt")
        .expect_err("describe execute wrapped missing deallocate should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
    assert!(
        error
            .report()
            .message
            .contains("prepared statement \"stmt\" does not exist"),
        "expected missing prepared statement error, got: {}",
        error.report().message
    );
}

#[test]
fn describe_explain_execute_on_missing_compat_prepared_statement_reports_undefined_object() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "explain_exec_missing_stmt".to_owned(),
            "EXPLAIN EXECUTE missing_stmt(41)".to_owned(),
        )
        .expect("prepare explain execute should succeed before describe");

    let error = engine
        .describe_statement(&session, "explain_exec_missing_stmt")
        .expect_err("describe explain execute on missing compat prepared statement should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
    assert!(
        error
            .report()
            .message
            .contains("prepared statement \"missing_stmt\" does not exist"),
        "expected missing prepared statement error, got: {}",
        error.report().message
    );
}

#[test]
fn describe_explain_fetch_on_missing_cursor_reports_invalid_cursor_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "explain_fetch_missing_cursor".to_owned(),
            "EXPLAIN FETCH ALL IN missing_cursor".to_owned(),
        )
        .expect("prepare explain fetch should succeed before describe");

    let error = engine
        .describe_statement(&session, "explain_fetch_missing_cursor")
        .expect_err("describe explain fetch on missing cursor should fail");
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
fn compat_execute_enforces_declared_parameter_arity() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt(int, text) AS SELECT $1, $2")
        .expect("prepare should succeed");

    let error = engine
        .execute_sql(&session, "EXECUTE stmt(1)")
        .expect_err("arity mismatch should fail");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidParameterValue
    );
    assert!(
        error
            .report()
            .message
            .contains("expected 2 parameter(s), received 1"),
        "unexpected error: {error}"
    );
}

#[test]
fn compat_execute_uses_declared_type_casts_for_null_arguments() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt(int) AS SELECT $1 + 1")
        .expect("prepare should succeed");

    let execute_results = engine
        .execute_sql(&session, "EXECUTE stmt(NULL)")
        .expect("execute should succeed");
    assert_eq!(execute_results.len(), 1);
    match &execute_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                &vec![aiondb_core::Row::new(vec![aiondb_core::Value::Null])]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn ctas_execute_uses_declared_type_casts_for_null_arguments() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt(int) AS SELECT $1 + 1 AS v")
        .expect("prepare should succeed");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE ctas_exec_null AS EXECUTE stmt(NULL)",
        )
        .expect("ctas execute should succeed");

    let rows = engine
        .execute_sql(&session, "SELECT v FROM ctas_exec_null")
        .expect("select from ctas execute table");
    assert_eq!(
        rows,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "v".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Null])],
        }]
    );
}

#[test]
fn ctas_execute_enforces_declared_parameter_arity() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt(int, int) AS SELECT $1 + $2 AS v")
        .expect("prepare should succeed");

    let error = engine
        .execute_sql(&session, "CREATE TABLE ctas_exec_bad AS EXECUTE stmt(1)")
        .expect_err("ctas execute arity mismatch should fail");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidParameterValue
    );
    assert!(
        error
            .report()
            .message
            .contains("expected 2 parameter(s), received 1"),
        "unexpected error: {error}"
    );
}

#[test]
fn compat_explain_execute_uses_bound_prepared_statement() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt(int) AS SELECT $1 + 1 AS v")
        .expect("prepare should succeed");

    let results = engine
        .execute_sql(&session, "EXPLAIN EXECUTE stmt(41)")
        .expect("explain execute should succeed");
    let [StatementResult::Query { columns, rows }] = results.as_slice() else {
        panic!("expected explain query result");
    };

    assert_eq!(
        columns,
        &[ResultColumn {
            name: "QUERY PLAN".to_owned(),
            data_type: aiondb_core::DataType::Text,
            text_type_modifier: None,
            nullable: false,
        }]
    );

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
        lines.contains(&"Result"),
        "unexpected EXPLAIN rows: {lines:?}"
    );
}

#[test]
fn compat_explain_analyze_execute_reports_query_summary() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt(int) AS SELECT $1 + 1 AS v")
        .expect("prepare should succeed");

    let results = engine
        .execute_sql(&session, "EXPLAIN ANALYZE EXECUTE stmt(41)")
        .expect("explain analyze execute should succeed");
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
        lines.contains(&"Execution: Query"),
        "unexpected EXPLAIN rows: {lines:?}"
    );
    assert!(
        lines.contains(&"Rows Returned: 1"),
        "unexpected EXPLAIN rows: {lines:?}"
    );
}
