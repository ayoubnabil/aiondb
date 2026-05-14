use super::*;

#[test]
fn compat_prepare_execute_supports_quoted_statement_names() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE \"MiXeD\"(int) AS SELECT $1 + 1 AS v")
        .expect("prepare should succeed");

    let execute_results = engine
        .execute_sql(&session, "EXECUTE \"MiXeD\"(41)")
        .expect("execute should succeed");
    let [StatementResult::Query { rows, .. }] = execute_results.as_slice() else {
        panic!("expected execute query result");
    };
    assert_eq!(
        rows,
        &vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(42)])]
    );

    let explain_results = engine
        .execute_sql(&session, "EXPLAIN EXECUTE \"MiXeD\"(41)")
        .expect("explain execute should succeed");
    let [StatementResult::Query { rows, .. }] = explain_results.as_slice() else {
        panic!("expected explain query result");
    };
    assert!(
        rows.iter().any(
            |row| matches!(row.values.as_slice(), [aiondb_core::Value::Text(line)] if line == "Result")
        ),
        "unexpected explain rows: {rows:?}"
    );

    engine
        .execute_sql(
            &session,
            "CREATE TABLE quoted_exec AS EXECUTE \"MiXeD\"(41)",
        )
        .expect("ctas execute should succeed");
    let ctas_rows = engine
        .execute_sql(&session, "SELECT v FROM quoted_exec")
        .expect("select from ctas execute");
    assert_eq!(
        ctas_rows,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "v".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(42)])],
        }]
    );

    engine
        .execute_sql(&session, "DEALLOCATE \"MiXeD\"")
        .expect("deallocate quoted name should succeed");
    let error = engine
        .execute_sql(&session, "EXECUTE \"MiXeD\"(41)")
        .expect_err("deallocated quoted prepared statement should be gone");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
}

#[test]
fn compat_deallocate_prepare_all_clears_all_statements() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt1 AS SELECT 1")
        .expect("prepare stmt1 should succeed");
    engine
        .execute_sql(&session, "PREPARE stmt2 AS SELECT 2")
        .expect("prepare stmt2 should succeed");

    engine
        .execute_sql(&session, "DEALLOCATE PREPARE ALL")
        .expect("deallocate prepare all should succeed");

    for sql in ["EXECUTE stmt1", "EXECUTE stmt2"] {
        let error = engine
            .execute_sql(&session, sql)
            .expect_err("prepared statement should be gone");
        assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
    }
}

#[test]
fn compat_execute_missing_statement_is_not_internal_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "EXECUTE missing_stmt(1)")
        .expect_err("missing compat execute target should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
    assert!(error
        .report()
        .message
        .contains("prepared statement \"missing_stmt\" does not exist"));
}

#[test]
fn compat_deallocate_prepare_requires_existing_statement() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt AS SELECT 1")
        .expect("prepare should succeed");
    engine
        .execute_sql(&session, "DEALLOCATE PREPARE stmt")
        .expect("deallocate prepare should succeed");

    let error = engine
        .execute_sql(&session, "DEALLOCATE stmt")
        .expect_err("deallocating missing compat statement should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
    assert!(error
        .report()
        .message
        .contains("prepared statement \"stmt\" does not exist"));
}

#[test]
fn compat_deallocate_removes_protocol_prepared_statement() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "stmt".to_owned(), "SELECT 1".to_owned())
        .expect("protocol prepare should succeed");

    engine
        .execute_sql(&session, "DEALLOCATE stmt")
        .expect("deallocate should remove protocol prepared statement");

    let error = engine
        .describe_statement(&session, "stmt")
        .expect_err("protocol prepared statement should be gone");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
}

#[test]
fn close_statement_removes_compat_prepared_statement() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt AS SELECT 1")
        .expect("compat prepare should succeed");

    engine
        .close_statement(&session, "stmt")
        .expect("close statement should remove compat prepared statement");

    let error = engine
        .execute_sql(&session, "EXECUTE stmt")
        .expect_err("compat prepared statement should be gone");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
}

#[test]
fn describe_deallocate_missing_statement_reports_undefined_object() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "dealloc_missing_stmt".to_owned(),
            "DEALLOCATE stmt".to_owned(),
        )
        .expect("prepare deallocate should succeed before describe");

    let error = engine
        .describe_statement(&session, "dealloc_missing_stmt")
        .expect_err("describe deallocate on missing compat statement should fail");
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
fn describe_deallocate_protocol_statement_is_allowed() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "stmt".to_owned(), "SELECT 1".to_owned())
        .expect("protocol prepare should succeed");
    engine
        .prepare(
            &session,
            "describe_dealloc_stmt".to_owned(),
            "DEALLOCATE stmt".to_owned(),
        )
        .expect("prepare deallocate should succeed before describe");

    let desc = engine
        .describe_statement(&session, "describe_dealloc_stmt")
        .expect("describe deallocate for protocol statement should succeed");
    assert!(desc.result_columns.is_empty());
    assert!(desc.param_types.is_empty());
}

#[test]
fn describe_execute_wrapped_deallocate_protocol_statement_is_allowed() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "stmt".to_owned(), "SELECT 1".to_owned())
        .expect("protocol prepare should succeed");
    engine
        .execute_sql(&session, "PREPARE dealloc_stmt AS DEALLOCATE stmt")
        .expect("compat prepare should succeed");
    engine
        .prepare(
            &session,
            "exec_dealloc_stmt".to_owned(),
            "EXECUTE dealloc_stmt".to_owned(),
        )
        .expect("protocol prepare execute wrapper should succeed");

    let desc = engine
        .describe_statement(&session, "exec_dealloc_stmt")
        .expect("describe execute wrapper for protocol deallocate should succeed");
    assert!(desc.result_columns.is_empty());
    assert!(desc.param_types.is_empty());
}

#[test]
fn compat_prepare_rejects_empty_parameter_type_entries_with_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "PREPARE stmt(,) AS SELECT 1")
        .expect_err("empty type entry should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
    assert!(error
        .report()
        .message
        .contains("parameter type list cannot contain empty entries"));
}

#[test]
fn compat_prepare_rejects_invalid_vector_dimensions_with_invalid_parameter_value() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "PREPARE stmt(vector(oops)) AS SELECT 1")
        .expect_err("invalid vector dimensions should fail");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidParameterValue
    );
    assert!(error
        .report()
        .message
        .contains("invalid PREPARE vector dimensions"));
}

#[test]
fn prepares_parsed_compatibility_noop() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let desc = engine
        .prepare(
            &session,
            "bad".to_owned(),
            "PREPARE stmt AS SELECT 1".to_owned(),
        )
        .expect("compatibility noop should prepare successfully");
    assert_eq!(
        desc,
        PreparedStatementDesc {
            name: "bad".to_owned(),
            param_types: Vec::new(),
            result_columns: Vec::new(),
            result_column_origins: Vec::new(),
        }
    );
}

#[test]
fn prepare_explain_discard_sequences_accepts_leading_whitespace() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let desc = engine
        .prepare(
            &session,
            "ok".to_owned(),
            " \n\tEXPLAIN DISCARD SEQUENCES".to_owned(),
        )
        .expect("EXPLAIN DISCARD SEQUENCES should validate inner SQL after span normalization");
    assert_eq!(
        desc,
        PreparedStatementDesc {
            name: "ok".to_owned(),
            param_types: Vec::new(),
            result_columns: vec![ResultColumn {
                name: "QUERY PLAN".to_owned(),
                data_type: aiondb_core::DataType::Text,
                text_type_modifier: None,
                nullable: false,
            }],
            result_column_origins: vec![None],
        }
    );
}

#[test]
fn explain_of_unsupported_compatibility_command_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "EXPLAIN LOCK TABLE anything")
        .expect_err("EXPLAIN should reject unsupported compatibility commands");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error.report().message.contains("LOCK"));
}

#[test]
fn explain_of_discard_all_fails_with_sql_specific_validation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "EXPLAIN DISCARD ALL")
        .expect_err("EXPLAIN DISCARD ALL should fail instead of using tag-only validation");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error.report().message.contains("DISCARD"));
}

#[test]
fn bound_if_exists_noop_still_succeeds_with_notice() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "DROP TABLE IF EXISTS missing_table")
        .expect("DROP TABLE IF EXISTS should still succeed");

    assert_eq!(results.len(), 2);
    match &results[0] {
        StatementResult::Notice { message } => {
            assert!(
                message.contains("missing_table"),
                "expected notice to mention missing table: {message}"
            );
        }
        other => panic!("expected notice result, got {other:?}"),
    }
    match &results[1] {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "DROP TABLE");
            assert_eq!(*rows_affected, 0);
        }
        other => panic!("expected command result, got {other:?}"),
    }
}

#[test]
fn rejects_unknown_create_stub_instead_of_reporting_fake_success() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "CREATE foobar thing")
        .expect_err("unknown CREATE compatibility stub must fail fast");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(
        err.to_string().contains("CREATE FOOBAR"),
        "unexpected error: {err}"
    );
}

#[test]
fn prepare_rejects_unknown_alter_stub() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .prepare(&session, "bad".to_owned(), "ALTER foobar thing".to_owned())
        .expect_err("unknown ALTER compatibility stub must fail at prepare time");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(
        err.to_string().contains("ALTER FOOBAR"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_unsupported_from_function_instead_of_returning_placeholder_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SELECT 1 FROM imaginary_srf(1)")
        .expect_err("unsupported FROM function should fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(
        format!("{error}").contains("imaginary_srf"),
        "expected function name in error message: {error}"
    );
}
