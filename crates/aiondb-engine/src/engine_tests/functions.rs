use super::*;

#[path = "functions_languages_and_params.rs"]
mod languages_and_params;

// ---------------------------------------------------------------------------
// CREATE FUNCTION / basic invocation
// ---------------------------------------------------------------------------

#[test]
fn create_and_call_simple_sql_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION add_one(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql",
        )
        .expect("create function");

    let results = engine
        .execute_sql(&session, "SELECT add_one(5)")
        .expect("select");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "add_one".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(6)])],
        }]
    );
}

#[test]
fn create_and_call_schema_qualified_sql_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             CREATE FUNCTION analytics.add_one(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql",
        )
        .expect("create schema-qualified function");

    let results = engine
        .execute_sql(&session, "SELECT analytics.add_one(5)")
        .expect("select schema-qualified function");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "add_one".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(6)])],
        }]
    );
}

#[test]
fn search_path_resolves_schema_qualified_sql_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             CREATE FUNCTION analytics.add_one(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql;
             SET search_path TO analytics, public",
        )
        .expect("prepare schema-qualified function");

    let results = engine
        .execute_sql(&session, "SELECT add_one(5)")
        .expect("select via search_path");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "add_one".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(6)])],
        }]
    );
}

#[test]
fn search_path_falls_back_to_public_schema_qualified_sql_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             CREATE FUNCTION public.add_one(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql;
             SET search_path TO analytics, public",
        )
        .expect("prepare public fallback function");

    let results = engine
        .execute_sql(&session, "SELECT add_one(5)")
        .expect("select via public fallback");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "add_one".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(6)])],
        }]
    );
}

#[test]
fn search_path_resolves_unqualified_sql_function_from_later_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA missing_schema;
             CREATE SCHEMA analytics;
             CREATE FUNCTION analytics.add_one(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql;
             SET search_path TO missing_schema, analytics, public",
        )
        .expect("prepare later-schema function");

    let results = engine
        .execute_sql(&session, "SELECT add_one(5)")
        .expect("select via later search_path schema");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "add_one".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(6)])],
        }]
    );
}

#[test]
fn prepare_resolves_unqualified_sql_function_from_later_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA missing_schema;
             CREATE SCHEMA analytics;
             CREATE FUNCTION analytics.add_one(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql;
             SET search_path TO missing_schema, analytics, public",
        )
        .expect("prepare later-schema function");

    let desc = engine
        .prepare(
            &session,
            "s_add_one".to_owned(),
            "SELECT add_one(5)".to_owned(),
        )
        .expect("prepare should resolve later-schema function");
    assert_eq!(desc.param_types, Vec::<aiondb_core::DataType>::new());
    assert_eq!(desc.result_columns.len(), 1);
    assert_eq!(desc.result_columns[0].name, "add_one");
    assert_eq!(desc.result_columns[0].data_type, aiondb_core::DataType::Int);
}

#[test]
fn insert_values_resolves_unqualified_sql_function_from_later_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA missing_schema;
             CREATE SCHEMA analytics;
             CREATE TABLE results (n INT);
             CREATE FUNCTION analytics.add_one(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql;
             SET search_path TO missing_schema, analytics, public",
        )
        .expect("prepare later-schema function and target table");

    engine
        .execute_sql(&session, "INSERT INTO public.results VALUES (add_one(5))")
        .expect("insert via later search_path function");

    let results = engine
        .execute_sql(&session, "SELECT n FROM public.results")
        .expect("select inserted rows");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "n".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(6)])],
        }]
    );
}

#[test]
fn insert_returning_resolves_unqualified_sql_function_from_later_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA missing_schema;
             CREATE SCHEMA analytics;
             CREATE TABLE public.results (n INT);
             CREATE FUNCTION analytics.add_one(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql;
             SET search_path TO missing_schema, analytics, public",
        )
        .expect("prepare later-schema function and target table");

    let results = engine
        .execute_sql(
            &session,
            "INSERT INTO public.results VALUES (5) RETURNING add_one(n)",
        )
        .expect("insert returning via later search_path function");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "add_one".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(6)])],
        }]
    );
}

#[test]
fn sql_function_query_body_resolves_user_function_via_later_search_path_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA missing_schema;
             CREATE SCHEMA analytics;
             CREATE TABLE public.one_row (n INT);
             INSERT INTO public.one_row VALUES (1);
             CREATE FUNCTION analytics.inner_add_one(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql;
             CREATE FUNCTION analytics.outer_add_one_query(x INT) RETURNS INT AS $$ SELECT inner_add_one($1) FROM public.one_row $$ LANGUAGE sql;
             SET search_path TO missing_schema, analytics, public",
        )
        .expect("prepare later-schema query-body function");

    let results = engine
        .execute_sql(&session, "SELECT analytics.outer_add_one_query(5)")
        .expect("select query-body function via later search_path schema");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "outer_add_one_query".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(6)])],
        }]
    );
}

#[test]
fn sql_function_body_resolves_user_function_via_search_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             CREATE FUNCTION analytics.inner_add_one(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql;
             CREATE FUNCTION analytics.outer_add_one(x INT) RETURNS INT AS $$ SELECT inner_add_one($1) $$ LANGUAGE sql;
             SET search_path TO analytics, public",
        )
        .expect("prepare nested sql functions");

    let results = engine
        .execute_sql(&session, "SELECT outer_add_one(5)")
        .expect("select nested function via search_path");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "outer_add_one".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(6)])],
        }]
    );
}

#[test]
fn sql_function_expression_body_resolves_user_function_via_search_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             CREATE FUNCTION analytics.inner_add_one(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql;
             CREATE FUNCTION analytics.outer_add_one_expr(x INT) RETURNS INT AS 'inner_add_one(x)' LANGUAGE sql;
             SET search_path TO analytics, public",
        )
        .expect("prepare nested sql expression functions");

    let results = engine
        .execute_sql(&session, "SELECT outer_add_one_expr(5)")
        .expect("select nested expression function via search_path");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "outer_add_one_expr".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(6)])],
        }]
    );
}

#[test]
fn sql_function_expression_body_resolves_user_function_via_later_search_path_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA missing_schema;
             CREATE SCHEMA analytics;
             CREATE FUNCTION analytics.inner_add_one(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql;
             CREATE FUNCTION analytics.outer_add_one_expr(x INT) RETURNS INT AS 'inner_add_one(x)' LANGUAGE sql;
             SET search_path TO missing_schema, analytics, public",
        )
        .expect("prepare nested sql expression functions with later search_path schema");

    let results = engine
        .execute_sql(&session, "SELECT analytics.outer_add_one_expr(5)")
        .expect("select nested expression function via later search_path schema");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "outer_add_one_expr".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(6)])],
        }]
    );
}

#[test]
fn sql_function_body_with_unsupported_compatibility_command_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION bad_body() RETURNS INT AS 'CREATE TRANSFORM mytrans' LANGUAGE sql",
        )
        .expect("create function");

    let error = engine
        .execute_sql(&session, "SELECT bad_body()")
        .expect_err("unsupported compatibility command in SQL function body should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error.report().message.contains("CREATE TRANSFORM"));
}

#[test]
fn sql_function_body_with_malformed_execute_compatibility_command_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION bad_exec_body() RETURNS INT AS 'EXECUTE' LANGUAGE sql",
        )
        .expect("create function");

    let error = engine
        .execute_sql(&session, "SELECT bad_exec_body()")
        .expect_err("malformed EXECUTE compatibility command in SQL function body should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error.report().message.contains("EXECUTE"));
}

#[test]
fn create_function_with_two_params() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION multiply(a INT, b INT) RETURNS INT AS 'a * b' LANGUAGE sql",
        )
        .expect("create function");

    let results = engine
        .execute_sql(&session, "SELECT multiply(3, 7)")
        .expect("select");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "multiply".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(21)])],
        }]
    );
}

// ---------------------------------------------------------------------------
// OR REPLACE
// ---------------------------------------------------------------------------

#[test]
fn create_or_replace_function_replaces_existing() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION greet(x TEXT) RETURNS TEXT AS 'x' LANGUAGE sql",
        )
        .expect("create function");

    // Replace with a different body
    engine
        .execute_sql(
            &session,
            "CREATE OR REPLACE FUNCTION greet(x TEXT) RETURNS TEXT AS 'x' LANGUAGE sql",
        )
        .expect("create or replace");

    let results = engine
        .execute_sql(&session, "SELECT greet('world')")
        .expect("select");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "greet".to_owned(),
                data_type: aiondb_core::DataType::Text,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "world".to_owned()
            )])],
        }]
    );
}

#[test]
fn create_or_replace_function_on_nonexistent_creates() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE OR REPLACE FUNCTION double_it(x INT) RETURNS INT AS 'x * 2' LANGUAGE sql",
        )
        .expect("create or replace");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE FUNCTION".to_owned(),
            rows_affected: 0,
        }]
    );

    let results = engine
        .execute_sql(&session, "SELECT double_it(4)")
        .expect("select");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "double_it".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(8)])],
        }]
    );
}

/// DROP FUNCTION targeting one specific overload signature must leave
/// sibling overloads intact. Pre-fix path went through drop-all +
/// recreate-survivors and could lose siblings on transient failure;
/// `drop_function_overload` now does the targeted drop in place.
#[test]
fn drop_function_with_arg_types_preserves_other_overloads() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION sole(a INT) RETURNS INT AS 'a' LANGUAGE sql",
        )
        .expect("create int overload");
    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION sole(a TEXT) RETURNS TEXT AS 'a' LANGUAGE sql",
        )
        .expect("create text overload");

    engine
        .execute_sql(&session, "DROP FUNCTION sole(INT)")
        .expect("drop int overload");

    let err = engine
        .execute_sql(&session, "SELECT sole(5)")
        .expect_err("INT overload must be gone");
    assert!(
        err.to_string().to_lowercase().contains("function")
            || err.to_string().to_lowercase().contains("undefined"),
        "expected undefined-function-style error, got {err}"
    );

    let text_result = engine
        .execute_sql(&session, "SELECT sole('abc')")
        .expect("TEXT overload must still resolve");
    assert!(matches!(
        &text_result[..],
        [StatementResult::Query { rows, .. }] if rows.len() == 1
    ));
}

/// CREATE OR REPLACE on one overload must preserve sibling overloads
/// with different signatures. The pre-fix implementation
/// drop-all-then-recreate-survivors loop was non-atomic in autocommit
/// mode and could lose siblings on a transient failure mid-loop. The
/// `replace_or_create_function` catalog API now does the swap in place.
#[test]
fn create_or_replace_preserves_other_overloads() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION pick(a INT) RETURNS INT AS 'a' LANGUAGE sql",
        )
        .expect("create int overload");
    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION pick(a TEXT) RETURNS TEXT AS 'a' LANGUAGE sql",
        )
        .expect("create text overload");

    // Replace only the INT overload; TEXT overload must survive.
    engine
        .execute_sql(
            &session,
            "CREATE OR REPLACE FUNCTION pick(a INT) RETURNS INT AS 'a + 1' LANGUAGE sql",
        )
        .expect("or replace int overload");

    // Both overloads must still resolve.
    let int_result = engine
        .execute_sql(&session, "SELECT pick(5)")
        .expect("call int overload");
    assert!(matches!(
        &int_result[..],
        [StatementResult::Query { rows, .. }]
            if rows.len() == 1
    ));

    let text_result = engine
        .execute_sql(&session, "SELECT pick('abc')")
        .expect("call text overload after or-replace of sibling");
    assert!(
        matches!(
            &text_result[..],
            [StatementResult::Query { rows, .. }]
                if rows.len() == 1
        ),
        "TEXT overload must still exist after OR REPLACE on the INT overload"
    );
}

#[test]
fn create_function_without_or_replace_errors_on_duplicate() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION f(x INT) RETURNS INT AS 'x' LANGUAGE sql",
        )
        .expect("create function");

    let err = engine
        .execute_sql(
            &session,
            "CREATE FUNCTION f(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql",
        )
        .expect_err("should fail on duplicate");
    assert!(
        format!("{err:?}").contains("already exists"),
        "expected 'already exists' error, got: {err:?}"
    );
}

#[test]
fn create_function_emits_notice_for_shell_types() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TYPE shell_alias")
        .expect("create shell type");

    let results = engine
        .execute_sql(
            &session,
            "CREATE FUNCTION shell_alias_out(shell_alias) RETURNS cstring \
             STRICT IMMUTABLE LANGUAGE internal AS 'int8out'",
        )
        .expect("create function with shell argument type");

    assert_eq!(
        results,
        vec![
            StatementResult::Notice {
                message: "argument type shell_alias is only a shell".to_owned(),
            },
            StatementResult::Command {
                tag: "CREATE FUNCTION".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

#[test]
fn create_function_allows_overloads_for_distinct_shell_types() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TYPE shell_left; CREATE TYPE shell_right;")
        .expect("create shell types");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION shell_eq(shell_left, shell_left) RETURNS bool \
             STRICT IMMUTABLE LANGUAGE internal AS 'int8eq'",
        )
        .expect("create first overload");

    let results = engine
        .execute_sql(
            &session,
            "CREATE FUNCTION shell_eq(shell_left, shell_right) RETURNS bool \
             STRICT IMMUTABLE LANGUAGE internal AS 'int8eq'",
        )
        .expect("create second overload");

    assert_eq!(
        results.last(),
        Some(&StatementResult::Command {
            tag: "CREATE FUNCTION".to_owned(),
            rows_affected: 0,
        })
    );
}

// ---------------------------------------------------------------------------
// DROP FUNCTION / IF EXISTS
// ---------------------------------------------------------------------------

#[test]
fn drop_schema_qualified_function_removes_it() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             CREATE FUNCTION analytics.temp_fn() RETURNS INT AS '1' LANGUAGE sql",
        )
        .expect("create schema-qualified function");

    let results = engine
        .execute_sql(&session, "DROP FUNCTION analytics.temp_fn()")
        .expect("drop schema-qualified function");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "DROP FUNCTION".to_owned(),
            rows_affected: 0,
        }]
    );

    let err = engine
        .execute_sql(&session, "SELECT analytics.temp_fn()")
        .expect_err("schema-qualified function should be gone");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UndefinedObject);
}

#[test]
fn drop_function_resolves_schema_qualified_name_via_search_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             CREATE FUNCTION analytics.temp_fn() RETURNS INT AS '1' LANGUAGE sql;
             SET search_path TO analytics, public",
        )
        .expect("prepare schema-qualified function");

    let results = engine
        .execute_sql(&session, "DROP FUNCTION temp_fn()")
        .expect("drop via search_path");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "DROP FUNCTION".to_owned(),
            rows_affected: 0,
        }]
    );

    let err = engine
        .execute_sql(&session, "SELECT analytics.temp_fn()")
        .expect_err("function should be gone");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UndefinedObject);
}

#[test]
fn drop_function_removes_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION temp_fn(x INT) RETURNS INT AS 'x' LANGUAGE sql",
        )
        .expect("create");

    let results = engine
        .execute_sql(&session, "DROP FUNCTION temp_fn")
        .expect("drop");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "DROP FUNCTION".to_owned(),
            rows_affected: 0,
        }]
    );

    let error = engine
        .execute_sql(&session, "SELECT temp_fn(1)")
        .expect_err("dropped function should error");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
}

#[test]
fn drop_function_nonexistent_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "DROP FUNCTION no_such_fn")
        .expect_err("should fail for nonexistent");
    assert!(
        format!("{err:?}").contains("does not exist"),
        "expected 'does not exist' error, got: {err:?}"
    );
}

#[test]
fn drop_function_if_exists_nonexistent_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "DROP FUNCTION IF EXISTS no_such_fn")
        .expect("drop if exists");
    assert_eq!(
        results,
        vec![
            StatementResult::Notice {
                message: "function no_such_fn() does not exist, skipping".to_owned(),
            },
            StatementResult::Command {
                tag: "DROP FUNCTION".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

#[test]
fn drop_function_if_exists_removes_existing() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION removable(x INT) RETURNS INT AS 'x' LANGUAGE sql",
        )
        .expect("create");

    let results = engine
        .execute_sql(&session, "DROP FUNCTION IF EXISTS removable")
        .expect("drop if exists");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "DROP FUNCTION".to_owned(),
            rows_affected: 0,
        }]
    );

    let error = engine
        .execute_sql(&session, "SELECT removable(1)")
        .expect_err("dropped function should error");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
}

#[test]
fn drop_function_cascade_emits_notice_for_compat_cast_dependency() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TYPE casttesttype;
             CREATE CAST (text AS casttesttype) WITHOUT FUNCTION;
             CREATE FUNCTION int4_casttesttype(int4) RETURNS casttesttype LANGUAGE SQL AS
             $$ SELECT ('foo'::text || $1::text)::casttesttype; $$;
             CREATE CAST (int4 AS casttesttype) WITH FUNCTION int4_casttesttype(int4) AS IMPLICIT;",
        )
        .expect("create compat cast dependency");

    let results = engine
        .execute_sql(&session, "DROP FUNCTION int4_casttesttype(int4) CASCADE")
        .expect("drop function cascade");
    assert_eq!(
        results,
        vec![
            StatementResult::Notice {
                message: "drop cascades to cast from integer to casttesttype".to_owned(),
            },
            StatementResult::Command {
                tag: "DROP FUNCTION".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

#[test]
fn create_cast_with_function_executes_function_body_on_explicit_cast() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION int4_to_text_double(v int4) RETURNS text LANGUAGE SQL AS \
             $$ SELECT ($1 * 2)::text $$;
             CREATE CAST (int4 AS text) WITH FUNCTION int4_to_text_double(int4) AS ASSIGNMENT;",
        )
        .expect("register cast");

    let results = engine
        .execute_sql(&session, "SELECT 21::int4::text")
        .expect("explicit cast via registered function");

    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected Query, got {:?}", results[0]);
    };
    assert_eq!(rows.len(), 1);
    match &rows[0].values[0] {
        aiondb_core::Value::Text(actual) => assert_eq!(actual, "42"),
        other => panic!("expected Text(\"42\"), got {other:?}"),
    }
}

#[test]
fn create_cast_implicit_function_applies_in_function_argument_coercion() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TYPE wrappedint;
             CREATE CAST (text AS wrappedint) WITHOUT FUNCTION;
             CREATE FUNCTION int4_to_wrappedint(v int4) RETURNS wrappedint LANGUAGE SQL AS \
             $$ SELECT ('w:' || $1::text)::wrappedint $$;
             CREATE CAST (int4 AS wrappedint) WITH FUNCTION int4_to_wrappedint(int4) AS IMPLICIT;
             CREATE FUNCTION takes_wrapped(w wrappedint) RETURNS text LANGUAGE SQL AS \
             $$ SELECT $1::text $$;",
        )
        .expect("register implicit cast and consumer");

    let results = engine
        .execute_sql(&session, "SELECT takes_wrapped(7)")
        .expect("implicit cast in fn arg");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected Query, got {:?}", results[0]);
    };
    match &rows[0].values[0] {
        aiondb_core::Value::Text(actual) => assert_eq!(actual, "w:7"),
        other => panic!("expected Text(\"w:7\"), got {other:?}"),
    }
}

#[test]
fn drop_cast_removes_registered_cast_so_explicit_cast_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TYPE droptesttype;
             CREATE CAST (text AS droptesttype) WITHOUT FUNCTION;
             CREATE FUNCTION int4_to_droptesttype(v int4) RETURNS droptesttype LANGUAGE SQL AS \
             $$ SELECT $1::text::droptesttype $$;
             CREATE CAST (int4 AS droptesttype) WITH FUNCTION int4_to_droptesttype(int4) AS IMPLICIT;",
        )
        .expect("register cast");

    engine
        .execute_sql(&session, "SELECT 1::int4::droptesttype")
        .expect("cast should succeed before drop");

    engine
        .execute_sql(&session, "DROP CAST (int4 AS droptesttype)")
        .expect("drop cast");

    let err = engine
        .execute_sql(&session, "SELECT 1::int4::droptesttype")
        .expect_err("cast should fail after drop");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::DatatypeMismatch);
}

#[test]
fn create_domain_with_check_enforces_constraint_on_insert() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE DOMAIN positive_int AS int4 CHECK (VALUE > 0);
             CREATE TABLE t (n positive_int);",
        )
        .expect("setup domain and table");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (5)")
        .expect("valid value passes domain check");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (-1)")
        .expect_err("negative value violates domain check");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

#[test]
fn create_domain_not_null_rejects_null_on_insert() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE DOMAIN required_text AS text NOT NULL;
             CREATE TABLE t (label required_text);",
        )
        .expect("setup not-null domain");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (NULL)")
        .expect_err("NULL violates NOT NULL domain");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::NotNullViolation);
}

#[test]
fn update_into_domain_column_enforces_check_constraint() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE DOMAIN positive_int AS int4 CHECK (VALUE > 0);
             CREATE TABLE t (id int4, n positive_int);
             INSERT INTO t VALUES (1, 5);",
        )
        .expect("setup");

    engine
        .execute_sql(&session, "UPDATE t SET n = 10 WHERE id = 1")
        .expect("valid update");

    let err = engine
        .execute_sql(&session, "UPDATE t SET n = -3 WHERE id = 1")
        .expect_err("update with negative violates domain");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

#[test]
fn create_enum_type_accepts_listed_labels_and_rejects_others() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');
             CREATE TABLE survey (id int4, m mood);",
        )
        .expect("setup enum table");

    engine
        .execute_sql(&session, "INSERT INTO survey VALUES (1, 'happy')")
        .expect("listed label accepted");

    let err = engine
        .execute_sql(&session, "INSERT INTO survey VALUES (2, 'angry')")
        .expect_err("unlisted label rejected");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidTextRepresentation
    );
}

#[test]
fn create_composite_type_accepts_constructor_assignment() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TYPE point2d AS (x int4, y int4);
             CREATE TABLE pts (id int4, p point2d);
             INSERT INTO pts VALUES (1, ROW(3, 4)::point2d);",
        )
        .expect("composite constructor accepted on insert");

    let results = engine
        .execute_sql(&session, "SELECT id FROM pts ORDER BY id")
        .expect("read back");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected Query, got {:?}", results[0]);
    };
    assert_eq!(rows.len(), 1);
}

#[test]
fn rls_policy_for_insert_with_check_blocks_offending_row() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN;
             CREATE ROLE bob LOGIN;
             CREATE TABLE rls_ins (id int4, owner text);
             GRANT INSERT ON rls_ins TO bob;
             ALTER TABLE rls_ins ENABLE ROW LEVEL SECURITY;
             CREATE POLICY rls_ins_p ON rls_ins FOR INSERT TO bob WITH CHECK (owner = current_user);",
        )
        .expect("setup RLS insert policy");

    engine
        .execute_sql(
            &admin,
            "SET ROLE bob; INSERT INTO rls_ins VALUES (1, 'bob')",
        )
        .expect("matching WITH CHECK passes");

    let err = engine
        .execute_sql(&admin, "INSERT INTO rls_ins VALUES (2, 'mallory')")
        .expect_err("violating WITH CHECK should fail");
    assert!(
        format!("{err}").contains("row-level security")
            || matches!(err.sqlstate(), aiondb_core::SqlState::CheckViolation),
        "expected RLS violation, got: {err}"
    );
}

#[test]
fn rls_policy_for_update_using_filters_visible_rows_for_writer() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN;
             CREATE ROLE bob LOGIN;
             CREATE TABLE rls_upd (id int4, owner text, val int4);
             INSERT INTO rls_upd VALUES (1, 'bob', 0), (2, 'mallory', 0);
             GRANT SELECT, UPDATE ON rls_upd TO bob;
             ALTER TABLE rls_upd ENABLE ROW LEVEL SECURITY;
             CREATE POLICY rls_upd_p ON rls_upd FOR UPDATE TO bob USING (owner = current_user);",
        )
        .expect("setup RLS update policy");

    engine
        .execute_sql(&admin, "SET ROLE bob; UPDATE rls_upd SET val = 100")
        .expect("update should run but only touch visible rows");

    engine
        .execute_sql(&admin, "RESET ROLE")
        .expect("reset role");
    let results = engine
        .execute_sql(&admin, "SELECT id, owner, val FROM rls_upd ORDER BY id")
        .expect("admin sees all rows");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected Query, got {:?}", results[0]);
    };
    let projected: Vec<(i32, String, i32)> = rows
        .iter()
        .map(|r| match (&r.values[0], &r.values[1], &r.values[2]) {
            (
                aiondb_core::Value::Int(id),
                aiondb_core::Value::Text(owner),
                aiondb_core::Value::Int(val),
            ) => (*id, owner.clone(), *val),
            other => panic!("unexpected row {other:?}"),
        })
        .collect();
    assert_eq!(
        projected,
        vec![(1, "bob".to_owned(), 100), (2, "mallory".to_owned(), 0),],
        "bob's row updated, mallory's row untouched"
    );
}

#[test]
fn rls_policy_for_delete_using_filters_targeted_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN;
             CREATE ROLE bob LOGIN;
             CREATE TABLE rls_del (id int4, owner text);
             INSERT INTO rls_del VALUES (1, 'bob'), (2, 'mallory'), (3, 'bob');
             GRANT SELECT, DELETE ON rls_del TO bob;
             ALTER TABLE rls_del ENABLE ROW LEVEL SECURITY;
             CREATE POLICY rls_del_p ON rls_del FOR DELETE TO bob USING (owner = current_user);",
        )
        .expect("setup RLS delete policy");

    engine
        .execute_sql(&admin, "SET ROLE bob; DELETE FROM rls_del")
        .expect("delete only touches visible rows");

    engine
        .execute_sql(&admin, "RESET ROLE")
        .expect("reset role");
    let results = engine
        .execute_sql(&admin, "SELECT id, owner FROM rls_del ORDER BY id")
        .expect("admin sees survivors");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected Query, got {:?}", results[0]);
    };
    let survivors: Vec<(i32, String)> = rows
        .iter()
        .map(|r| match (&r.values[0], &r.values[1]) {
            (aiondb_core::Value::Int(id), aiondb_core::Value::Text(owner)) => (*id, owner.clone()),
            other => panic!("unexpected row {other:?}"),
        })
        .collect();
    assert_eq!(survivors, vec![(2, "mallory".to_owned())]);
}

#[test]
fn rule_on_insert_do_instead_redirects_writes_to_underlying_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE rule_target (id int4, label text);
             CREATE VIEW rule_view AS SELECT id, label FROM rule_target;
             CREATE RULE rule_view_ins AS ON INSERT TO rule_view DO INSTEAD \
                INSERT INTO rule_target VALUES (NEW.id, NEW.label);",
        )
        .expect("setup view + INSTEAD insert rule");

    engine
        .execute_sql(&session, "INSERT INTO rule_view VALUES (1, 'one')")
        .expect("rule routes the insert");

    let results = engine
        .execute_sql(&session, "SELECT id, label FROM rule_target ORDER BY id")
        .expect("read underlying table");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected Query, got {:?}", results[0]);
    };
    assert_eq!(rows.len(), 1);
    match (&rows[0].values[0], &rows[0].values[1]) {
        (aiondb_core::Value::Int(id), aiondb_core::Value::Text(label)) => {
            assert_eq!(*id, 1);
            assert_eq!(label, "one");
        }
        other => panic!("unexpected row {other:?}"),
    }
}

#[test]
fn rule_drop_removes_rewrite_so_subsequent_dml_falls_through() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE rule_drop_t (id int4);
             CREATE RULE block_delete AS ON DELETE TO rule_drop_t DO INSTEAD NOTHING;
             INSERT INTO rule_drop_t VALUES (1), (2);
             DELETE FROM rule_drop_t WHERE id = 1;",
        )
        .expect("setup table with INSTEAD NOTHING delete rule");

    let results = engine
        .execute_sql(&session, "SELECT id FROM rule_drop_t ORDER BY id")
        .expect("rule should have suppressed the delete");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected Query, got {:?}", results[0]);
    };
    assert_eq!(rows.len(), 2, "INSTEAD NOTHING blocks the delete");

    engine
        .execute_sql(
            &session,
            "DROP RULE block_delete ON rule_drop_t;
             DELETE FROM rule_drop_t WHERE id = 1;",
        )
        .expect("after drop the delete falls through");

    let results = engine
        .execute_sql(&session, "SELECT id FROM rule_drop_t ORDER BY id")
        .expect("read survivors");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected Query, got {:?}", results[0]);
    };
    assert_eq!(rows.len(), 1, "delete now applies after rule dropped");
}

#[test]
fn drop_domain_with_dependent_column_should_error_without_cascade() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE DOMAIN bounded_int AS int4 CHECK (VALUE BETWEEN 0 AND 100);
             CREATE TABLE t (n bounded_int);",
        )
        .expect("setup domain dependency");

    let err = engine
        .execute_sql(&session, "DROP DOMAIN bounded_int")
        .expect_err("DROP DOMAIN must error when a table column depends on it");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::DependentObjectsStillExist
    );
}

#[test]
fn rls_multiple_permissive_policies_use_logical_or() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN;
             CREATE ROLE bob LOGIN;
             CREATE TABLE rls_or (id int4, owner text, public_flag bool);
             INSERT INTO rls_or VALUES \
               (1, 'bob', false), \
               (2, 'mallory', false), \
               (3, 'mallory', true);
             GRANT SELECT ON rls_or TO bob;
             ALTER TABLE rls_or ENABLE ROW LEVEL SECURITY;
             CREATE POLICY p_owned ON rls_or FOR SELECT TO bob USING (owner = current_user);
             CREATE POLICY p_public ON rls_or FOR SELECT TO bob USING (public_flag);",
        )
        .expect("setup multi-permissive policies");

    let results = engine
        .execute_sql(&admin, "SET ROLE bob; SELECT id FROM rls_or ORDER BY id")
        .expect("select with two permissive policies");
    let query_result = results
        .iter()
        .rev()
        .find(|r| matches!(r, StatementResult::Query { .. }))
        .expect("expected Query result");
    let StatementResult::Query { rows, .. } = query_result else {
        unreachable!()
    };
    let visible_ids: Vec<i32> = rows
        .iter()
        .map(|r| match &r.values[0] {
            aiondb_core::Value::Int(n) => *n,
            other => panic!("expected Int, got {other:?}"),
        })
        .collect();
    assert_eq!(
        visible_ids,
        vec![1, 3],
        "permissive policies combine with OR: bob's row OR public rows"
    );
}

#[test]
fn rls_restrictive_policy_combines_with_permissive_via_and() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN;
             CREATE ROLE bob LOGIN;
             CREATE TABLE rls_and (id int4, owner text, archived bool);
             INSERT INTO rls_and VALUES \
               (1, 'bob', false), \
               (2, 'bob', true), \
               (3, 'mallory', false);
             GRANT SELECT ON rls_and TO bob;
             ALTER TABLE rls_and ENABLE ROW LEVEL SECURITY;
             CREATE POLICY p_owned ON rls_and FOR SELECT TO bob USING (owner = current_user);
             CREATE POLICY p_not_archived ON rls_and AS RESTRICTIVE FOR SELECT TO bob USING (NOT archived);",
        )
        .expect("setup restrictive policy");

    let results = engine
        .execute_sql(&admin, "SET ROLE bob; SELECT id FROM rls_and ORDER BY id")
        .expect("select with restrictive policy");
    let query_result = results
        .iter()
        .rev()
        .find(|r| matches!(r, StatementResult::Query { .. }))
        .expect("expected Query result");
    let StatementResult::Query { rows, .. } = query_result else {
        unreachable!()
    };
    let visible_ids: Vec<i32> = rows
        .iter()
        .map(|r| match &r.values[0] {
            aiondb_core::Value::Int(n) => *n,
            other => panic!("expected Int, got {other:?}"),
        })
        .collect();
    assert_eq!(
        visible_ids,
        vec![1],
        "restrictive policy ANDs: only bob's non-archived rows"
    );
}

#[test]
fn drop_procedure_is_explicitly_unsupported() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "DROP PROCEDURE noop_proc")
        .expect_err("DROP PROCEDURE must reject explicitly");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
}

#[test]
fn drop_procedure_if_exists_is_still_explicitly_unsupported() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    for sql in [
        "DROP PROCEDURE IF EXISTS missing_schema.noop_proc()",
        "DROP ROUTINE IF EXISTS missing_schema.noop_proc()",
    ] {
        let err = engine
            .execute_sql(&session, sql)
            .expect_err("DROP PROCEDURE/ROUTINE must reject instead of returning command_ok");
        assert_eq!(err.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    }
}

#[test]
fn create_publication_no_longer_silently_accepts() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Either the upstream reference validator rejects (UndefinedTable
    // for an empty FOR clause / FOR ALL TABLES with no tables) or the
    // terminal feature-not-supported reject fires. Both are real errors;
    // a `CREATE PUBLICATION` command tag without persisting anything
    // useful.
    let err = engine
        .execute_sql(&session, "CREATE PUBLICATION pub1")
        .expect_err("CREATE PUBLICATION must not silently succeed");
    let state = err.sqlstate();
    assert!(
        matches!(
            state,
            aiondb_core::SqlState::FeatureNotSupported
                | aiondb_core::SqlState::UndefinedTable
                | aiondb_core::SqlState::SyntaxError
        ),
        "unexpected sqlstate {state:?}: {err}"
    );
}

#[test]
fn create_publication_for_existing_table_reports_feature_not_supported() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE wf_pub (id INT)")
        .expect("create table for publication workflow");

    let err = engine
        .execute_sql(&session, "CREATE PUBLICATION wf_pub_all FOR TABLE wf_pub")
        .expect_err("CREATE PUBLICATION should be explicitly unsupported");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(
        err.report()
            .message
            .contains("unsupported compatibility command: CREATE PUBLICATION"),
        "unexpected error message: {}",
        err.report().message
    );
}

#[test]
fn create_publication_for_existing_table_reports_feature_not_supported_across_sessions() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session_a, _) = engine.startup(startup_params()).expect("startup session a");
    let (session_b, _) = engine.startup(startup_params()).expect("startup session b");

    engine
        .execute_sql(&session_a, "CREATE TABLE wf_pub_remote (id INT)")
        .expect("create table for publication workflow in session a");

    let err = engine
        .execute_sql(
            &session_b,
            "CREATE PUBLICATION wf_pub_remote_all FOR TABLE wf_pub_remote",
        )
        .expect_err("CREATE PUBLICATION should be explicitly unsupported");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(
        err.report()
            .message
            .contains("unsupported compatibility command: CREATE PUBLICATION"),
        "unexpected error message: {}",
        err.report().message
    );
}

#[test]
fn create_subscription_does_not_emit_referenced_subscription_missing_message() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "CREATE SUBSCRIPTION wf_sub CONNECTION 'host=127.0.0.1 dbname=default' PUBLICATION wf_pub_all",
        )
        .expect_err("CREATE SUBSCRIPTION should error in compat mode");

    assert_ne!(
        err.report().message,
        "referenced subscription does not exist",
        "must not report the wrong missing-object kind for CREATE SUBSCRIPTION"
    );
}

#[test]
fn create_text_search_dictionary_persists_compat_metadata() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TEXT SEARCH DICTIONARY wf_dict (TEMPLATE = simple, STOPWORDS = english)",
        )
        .expect("CREATE TEXT SEARCH DICTIONARY should be accepted by compat metadata path");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE TEXT SEARCH".to_owned(),
            rows_affected: 0
        }]
    );
}

#[test]
fn create_text_search_dictionary_duplicate_reports_duplicate_object() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TEXT SEARCH DICTIONARY wf_dict_dup (TEMPLATE = simple)",
        )
        .expect("initial dictionary create should succeed");

    let duplicate = engine
        .execute_sql(
            &session,
            "CREATE TEXT SEARCH DICTIONARY wf_dict_dup (TEMPLATE = simple)",
        )
        .expect_err("second create should fail as duplicate");
    assert_eq!(duplicate.sqlstate(), aiondb_core::SqlState::DuplicateObject);
}

#[test]
fn create_foreign_table_is_explicitly_unsupported() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // First a SERVER reject (no FDW infra): the table reference can't
    // resolve since CREATE SERVER itself is rejected.
    let server_err = engine
        .execute_sql(
            &session,
            "CREATE SERVER my_srv FOREIGN DATA WRAPPER my_wrap",
        )
        .expect_err("CREATE SERVER must reject explicitly");
    assert_eq!(
        server_err.sqlstate(),
        aiondb_core::SqlState::FeatureNotSupported
    );
}

#[test]
fn create_procedure_is_explicitly_unsupported() {
    // AionDB has no SQL `CALL` execution path; accepting CREATE
    // returns `feature_not_supported` rather than half-storing the
    // procedure name in the session.
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "CREATE PROCEDURE noop_proc() LANGUAGE SQL AS $$ SELECT 1 $$",
        )
        .expect_err("CREATE PROCEDURE must reject explicitly");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(err.report().message.contains("CREATE PROCEDURE"));
}

#[test]
fn alter_type_add_enum_value_extends_label_set() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TYPE traffic AS ENUM ('green', 'yellow', 'red');
             CREATE TABLE signals (id int4, color traffic);
             INSERT INTO signals VALUES (1, 'green');",
        )
        .expect("setup enum");

    // Reject before the new label is added.
    let err = engine
        .execute_sql(&session, "INSERT INTO signals VALUES (2, 'flashing')")
        .expect_err("'flashing' not yet a valid label");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidTextRepresentation
    );

    engine
        .execute_sql(
            &session,
            "ALTER TYPE traffic ADD VALUE 'flashing' AFTER 'red'",
        )
        .expect("ALTER TYPE ADD VALUE");

    engine
        .execute_sql(&session, "INSERT INTO signals VALUES (2, 'flashing')")
        .expect("'flashing' is now valid after ALTER TYPE ADD VALUE");
}

#[test]
fn alter_policy_using_update_changes_visibility_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN;
             CREATE ROLE bob LOGIN;
             CREATE TABLE rls_alter (id int4, owner text);
             INSERT INTO rls_alter VALUES (1, 'bob'), (2, 'mallory'), (3, 'bob');
             GRANT SELECT ON rls_alter TO bob;
             ALTER TABLE rls_alter ENABLE ROW LEVEL SECURITY;
             CREATE POLICY p_owner ON rls_alter FOR SELECT TO bob USING (owner = current_user);",
        )
        .expect("setup RLS policy");

    // Initial USING(owner = current_user) → bob sees rows 1 and 3.
    let initial = engine
        .execute_sql(&admin, "SET ROLE bob; SELECT id FROM rls_alter ORDER BY id")
        .expect("initial select");
    let StatementResult::Query { rows, .. } = initial
        .iter()
        .rev()
        .find(|r| matches!(r, StatementResult::Query { .. }))
        .expect("expected Query")
    else {
        unreachable!()
    };
    let initial_ids: Vec<i32> = rows
        .iter()
        .map(|r| match &r.values[0] {
            aiondb_core::Value::Int(n) => *n,
            other => panic!("expected Int, got {other:?}"),
        })
        .collect();
    assert_eq!(initial_ids, vec![1, 3]);

    // Tighten the policy: only id=1 should match.
    engine
        .execute_sql(
            &admin,
            "RESET ROLE; ALTER POLICY p_owner ON rls_alter USING (id = 1);",
        )
        .expect("alter policy USING");

    let tightened = engine
        .execute_sql(&admin, "SET ROLE bob; SELECT id FROM rls_alter ORDER BY id")
        .expect("post-alter select");
    let StatementResult::Query { rows, .. } = tightened
        .iter()
        .rev()
        .find(|r| matches!(r, StatementResult::Query { .. }))
        .expect("expected Query")
    else {
        unreachable!()
    };
    let tightened_ids: Vec<i32> = rows
        .iter()
        .map(|r| match &r.values[0] {
            aiondb_core::Value::Int(n) => *n,
            other => panic!("expected Int, got {other:?}"),
        })
        .collect();
    assert_eq!(
        tightened_ids,
        vec![1],
        "ALTER POLICY USING must replace the visibility predicate"
    );
}

#[test]
fn alter_domain_add_constraint_in_one_session_visible_to_subsequent_session() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session_a, _) = engine.startup(startup_params()).expect("startup A");

    engine
        .execute_sql(
            &session_a,
            "CREATE DOMAIN bounded_age AS int4;
             ALTER DOMAIN bounded_age ADD CONSTRAINT age_range CHECK (VALUE BETWEEN 0 AND 200);",
        )
        .expect("create + alter domain in session A");

    let (session_b, _) = engine.startup(startup_params()).expect("startup B");
    engine
        .execute_sql(
            &session_b,
            "CREATE TABLE people (age bounded_age);
             INSERT INTO people VALUES (42);",
        )
        .expect("session B sees the altered domain");

    let err = engine
        .execute_sql(&session_b, "INSERT INTO people VALUES (-1)")
        .expect_err("ALTER DOMAIN ADD CONSTRAINT enforced cross-session");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);

    let err_high = engine
        .execute_sql(&session_b, "INSERT INTO people VALUES (300)")
        .expect_err("upper bound also enforced cross-session");
    assert_eq!(err_high.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

#[test]
fn create_domain_in_one_session_visible_to_subsequent_session_via_catalog() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session_a, _) = engine.startup(startup_params()).expect("startup A");

    engine
        .execute_sql(
            &session_a,
            "CREATE DOMAIN persistent_age AS int4 CHECK (VALUE >= 0);",
        )
        .expect("create domain in session A");

    // Open a brand-new session: the in-memory session record starts blank
    // and must be hydrated from the catalog for the domain to be visible.
    let (session_b, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(
            &session_b,
            "CREATE TABLE t (n persistent_age); INSERT INTO t VALUES (5);",
        )
        .expect("session B sees the domain via catalog");

    let err = engine
        .execute_sql(&session_b, "INSERT INTO t VALUES (-1)")
        .expect_err("domain CHECK still enforced through the catalog-loaded copy");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

#[test]
fn create_rule_in_one_session_visible_to_subsequent_session_via_catalog() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session_a, _) = engine.startup(startup_params()).expect("startup A");

    engine
        .execute_sql(
            &session_a,
            "CREATE TABLE rule_persist (id int4);
             INSERT INTO rule_persist VALUES (1), (2);
             CREATE RULE block_persist AS ON DELETE TO rule_persist DO INSTEAD NOTHING;",
        )
        .expect("setup rule in session A");

    let (session_b, _) = engine.startup(startup_params()).expect("startup B");
    engine
        .execute_sql(&session_b, "DELETE FROM rule_persist WHERE id = 1")
        .expect("delete in session B");

    let results = engine
        .execute_sql(&session_b, "SELECT id FROM rule_persist ORDER BY id")
        .expect("rule should still suppress the delete after restart");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected Query, got {:?}", results[0]);
    };
    assert_eq!(rows.len(), 2, "INSTEAD NOTHING persists across sessions");
}

#[test]
fn create_policy_in_one_session_visible_to_subsequent_session_via_catalog() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin_a, _) = engine.startup(startup_params()).expect("startup A");

    engine
        .execute_sql(
            &admin_a,
            "CREATE ROLE alice SUPERUSER LOGIN;
             CREATE ROLE bob LOGIN;
             CREATE TABLE rls_persist (id int4, owner text);
             INSERT INTO rls_persist VALUES (1, 'bob'), (2, 'mallory');
             GRANT SELECT ON rls_persist TO bob;
             ALTER TABLE rls_persist ENABLE ROW LEVEL SECURITY;
             CREATE POLICY rls_persist_p ON rls_persist FOR SELECT TO bob USING (owner = current_user);",
        )
        .expect("setup RLS in session A");

    // New session: hydration must rebuild compat_misc_objects /
    // compat_misc_attrs from the catalog so RLS still applies.
    let (admin_b, _) = engine.startup(startup_params()).expect("startup B");
    let results = engine
        .execute_sql(
            &admin_b,
            "SET ROLE bob; SELECT id FROM rls_persist ORDER BY id",
        )
        .expect("session B sees the policy");
    let query_result = results
        .iter()
        .rev()
        .find(|r| matches!(r, StatementResult::Query { .. }))
        .expect("expected Query result");
    let StatementResult::Query { rows, .. } = query_result else {
        unreachable!()
    };
    let visible: Vec<i32> = rows
        .iter()
        .map(|r| match &r.values[0] {
            aiondb_core::Value::Int(n) => *n,
            other => panic!("expected Int, got {other:?}"),
        })
        .collect();
    assert_eq!(
        visible,
        vec![1],
        "policy USING(owner=current_user) survives restart"
    );
}

#[test]
fn create_cast_in_one_session_visible_to_subsequent_session_via_catalog() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session_a, _) = engine.startup(startup_params()).expect("startup A");

    engine
        .execute_sql(
            &session_a,
            "CREATE FUNCTION int4_persist_double(v int4) RETURNS text LANGUAGE SQL AS \
             $$ SELECT ($1 * 2)::text $$;
             CREATE CAST (int4 AS text) WITH FUNCTION int4_persist_double(int4) AS ASSIGNMENT;",
        )
        .expect("register cast in session A");

    // Run the cast in session A first to confirm the in-memory path.
    let results = engine
        .execute_sql(&session_a, "SELECT 21::int4::text")
        .expect("session A casts via registered function");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected Query, got {:?}", results[0]);
    };
    match &rows[0].values[0] {
        aiondb_core::Value::Text(actual) => assert_eq!(actual, "42"),
        other => panic!("expected Text(\"42\"), got {other:?}"),
    }

    // Brand-new session: cast registry must be hydrated from the catalog
    // so `21::int4::text` still routes through the user function.
    let (session_b, _) = engine.startup(startup_params()).expect("startup B");
    let results = engine
        .execute_sql(&session_b, "SELECT 21::int4::text")
        .expect("session B casts via catalog-loaded cast");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected Query, got {:?}", results[0]);
    };
    match &rows[0].values[0] {
        aiondb_core::Value::Text(actual) => {
            assert_eq!(
                actual, "42",
                "cast must persist across sessions via catalog hydration"
            );
        }
        other => panic!("expected Text(\"42\"), got {other:?}"),
    }
}

#[test]
fn create_enum_type_in_one_session_visible_to_subsequent_session_via_catalog() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session_a, _) = engine.startup(startup_params()).expect("startup A");

    engine
        .execute_sql(
            &session_a,
            "CREATE TYPE persistent_mood AS ENUM ('sad', 'ok', 'happy');",
        )
        .expect("create enum in session A");

    // Open a brand-new session: hydration must surface the enum type so
    // the synthetic CHECK still rejects unknown labels.
    let (session_b, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(
            &session_b,
            "CREATE TABLE survey (id int4, m persistent_mood); INSERT INTO survey VALUES (1, 'happy');",
        )
        .expect("session B sees the enum via catalog");

    let err = engine
        .execute_sql(&session_b, "INSERT INTO survey VALUES (2, 'angry')")
        .expect_err("unknown label still rejected after cross-session reload");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidTextRepresentation
    );
}

#[test]
fn drop_type_in_one_session_removes_visibility_in_subsequent_session() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session_a, _) = engine.startup(startup_params()).expect("startup A");

    engine
        .execute_sql(
            &session_a,
            "CREATE TYPE ephemeral_status AS ENUM ('on', 'off');
             DROP TYPE ephemeral_status;",
        )
        .expect("create then drop");

    let (session_b, _) = engine.startup(startup_params()).expect("startup B");
    engine
        .execute_sql(
            &session_b,
            "CREATE TYPE ephemeral_status AS ENUM ('a', 'b');",
        )
        .expect("name reusable after catalog drop persisted");
}

#[test]
fn drop_domain_in_one_session_removes_visibility_in_subsequent_session() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session_a, _) = engine.startup(startup_params()).expect("startup A");

    engine
        .execute_sql(
            &session_a,
            "CREATE DOMAIN ephemeral_score AS int4 CHECK (VALUE >= 0);",
        )
        .expect("create domain");
    engine
        .execute_sql(&session_a, "DROP DOMAIN ephemeral_score")
        .expect("drop domain");

    let (session_b, _) = engine.startup(startup_params()).expect("startup B");

    // Use the bare type name as a regular column type; with no domain
    // registered the binder cannot resolve the identifier as a domain
    // type, but the lookup falls back to text, so INSERT then succeeds for
    // any value because no CHECK survives.
    engine
        .execute_sql(
            &session_b,
            "CREATE TABLE t (label text); INSERT INTO t VALUES ('any');",
        )
        .expect("session B sees no leftover constraint");
}

#[test]
fn drop_domain_removes_check_so_subsequent_table_does_not_enforce() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE DOMAIN positive_int AS int4 CHECK (VALUE > 0);
             DROP DOMAIN positive_int;",
        )
        .expect("create then drop domain");

    // After DROP, the type name is no longer registered as a domain, so a
    // table referencing it falls back to the underlying base type without
    // synthesising a domain CHECK constraint.
    engine
        .execute_sql(&session, "CREATE TABLE t (n int4);")
        .expect("create unrelated table");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (-1)")
        .expect("insert succeeds without domain check");
}

#[test]
fn create_domain_chain_enforces_constraints_from_each_level() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE DOMAIN even_int AS int4 CHECK (VALUE % 2 = 0);
             CREATE DOMAIN small_even AS even_int CHECK (VALUE < 100);
             CREATE TABLE t (n small_even);",
        )
        .expect("setup nested domains");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (4)")
        .expect("4 is even and small");

    let err_odd = engine
        .execute_sql(&session, "INSERT INTO t VALUES (5)")
        .expect_err("odd violates parent domain");
    assert_eq!(err_odd.sqlstate(), aiondb_core::SqlState::CheckViolation);

    let err_large = engine
        .execute_sql(&session, "INSERT INTO t VALUES (200)")
        .expect_err("200 violates child domain");
    assert_eq!(err_large.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

#[test]
fn missing_compat_user_cast_reports_postfix_cast_operator_position_with_trailing_comment() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TYPE casttesttype")
        .expect("create shell type");

    let sql = "SELECT 1234::int4::casttesttype; -- No cast yet, should fail";
    let err = engine
        .execute_sql(&session, sql)
        .expect_err("missing compat cast should fail");
    assert_eq!(
        err.report().position,
        sql.find("::casttesttype").map(|index| index + 1)
    );
}

// ---------------------------------------------------------------------------
// User function with table context
// ---------------------------------------------------------------------------

#[test]
fn call_user_function_in_select_with_table_columns() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION inc(v INT) RETURNS INT AS 'v + 1' LANGUAGE sql",
        )
        .expect("create function");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE nums (n INT); INSERT INTO nums VALUES (10), (20), (30)",
        )
        .expect("create and insert");

    let results = engine
        .execute_sql(&session, "SELECT inc(n) FROM nums ORDER BY n")
        .expect("select with function");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            let values: Vec<i32> = rows
                .iter()
                .map(|r| match &r.values[0] {
                    aiondb_core::Value::Int(v) => *v,
                    other => panic!("expected Int, got {other:?}"),
                })
                .collect();
            assert_eq!(values, vec![11, 21, 31]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// PG-compatible SELECT-body patterns
// ---------------------------------------------------------------------------

#[test]
fn function_with_select_body() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // PG-style function body: SELECT expr
    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION is_positive(x INT) RETURNS BOOL AS 'SELECT $1 > 0' LANGUAGE sql",
        )
        .expect("create function with SELECT body");

    let results = engine
        .execute_sql(&session, "SELECT is_positive(5)")
        .expect("select");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], aiondb_core::Value::Boolean(true));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn function_with_select_false_body() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION always_false() RETURNS BOOL AS 'SELECT false' LANGUAGE sql",
        )
        .expect("create function");

    let results = engine
        .execute_sql(&session, "SELECT always_false()")
        .expect("select");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], aiondb_core::Value::Boolean(false));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn create_function_with_aggregate_body_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION agg_fn() RETURNS INT AS 'SELECT count(*)' LANGUAGE sql",
        )
        .expect("aggregate body should be accepted");
}

#[test]
fn create_function_with_select_from_body_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION complex_fn() RETURNS INT AS 'SELECT count(*) FROM pg_class' LANGUAGE sql",
        )
        .expect("SELECT ... FROM body should be accepted");
}

#[test]
fn create_function_with_insert_body_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE fn_target (v INT)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION inserter(x INT) RETURNS VOID AS 'INSERT INTO fn_target VALUES(x)' LANGUAGE sql",
        )
        .expect("INSERT body should be accepted");
}

#[test]
fn sql_function_with_insert_body_executes_dml() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE fn_target (v INT)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION inserter(x INT) RETURNS VOID AS 'INSERT INTO fn_target VALUES(x)' LANGUAGE sql",
        )
        .expect("create function");

    let results = engine
        .execute_sql(&session, "SELECT inserter(41)")
        .expect("call function");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], aiondb_core::Value::Null);
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let results = engine
        .execute_sql(&session, "SELECT v FROM fn_target")
        .expect("read inserted row");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(41));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn sql_function_with_insert_returning_body_returns_value() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE fn_target (v INT)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION insert_and_echo(x INT) RETURNS INT AS 'INSERT INTO fn_target VALUES($1) RETURNING v' LANGUAGE sql",
        )
        .expect("create function");

    let results = engine
        .execute_sql(&session, "SELECT insert_and_echo(7)")
        .expect("call function");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(7));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn sql_function_with_multi_statement_body_executes_in_order() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE fn_target (v INT)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION insert_and_count(x INT) RETURNS BIGINT AS $$ INSERT INTO fn_target VALUES($1); SELECT count(*) FROM fn_target $$ LANGUAGE sql",
        )
        .expect("create function");

    let first = engine
        .execute_sql(&session, "SELECT insert_and_count(1)")
        .expect("first call");
    match &first[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], aiondb_core::Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let second = engine
        .execute_sql(&session, "SELECT insert_and_count(2)")
        .expect("second call");
    match &second[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], aiondb_core::Value::BigInt(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn function_with_dollar_quoted_select_body() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION add_nums(a INT, b INT) RETURNS INT AS $$ SELECT $1 + $2 $$ LANGUAGE sql",
        )
        .expect("create function with dollar-quoted SELECT body");

    let results = engine
        .execute_sql(&session, "SELECT add_nums(3, 4)")
        .expect("select");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(7));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn function_with_null_return_body() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION null_fn() RETURNS INT AS 'SELECT NULL::BIGINT' LANGUAGE sql",
        )
        .expect("create function with NULL body");

    // Should not panic or error
    let _results = engine
        .execute_sql(&session, "SELECT null_fn()")
        .expect("select");
}

#[test]
fn lo_lseek_seek_end_returns_object_length() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let create = engine
        .execute_sql(&session, "SELECT lo_create(0)")
        .expect("lo_create");
    let oid = match &create[0] {
        StatementResult::Query { rows, .. } => match rows[0].values[0] {
            aiondb_core::Value::Int(v) => v,
            ref other => panic!("expected Int oid, got {other:?}"),
        },
        other => panic!("expected Query, got {other:?}"),
    };

    // Write 5 bytes ("hello") at offset 0, then seek to end. Should report 5.
    engine
        .execute_sql(
            &session,
            &format!("SELECT lo_put({oid}, 0, '\\x68656c6c6f'::bytea)"),
        )
        .expect("lo_put");
    let fd_open = engine
        .execute_sql(&session, &format!("SELECT lo_open({oid}, 393216)"))
        .expect("lo_open");
    let fd = match &fd_open[0] {
        StatementResult::Query { rows, .. } => match rows[0].values[0] {
            aiondb_core::Value::Int(v) => v,
            ref other => panic!("expected Int fd, got {other:?}"),
        },
        other => panic!("expected Query, got {other:?}"),
    };
    // SEEK_END = 2. Seek with offset 0 from end should return length.
    let seek = engine
        .execute_sql(&session, &format!("SELECT lo_lseek({fd}, 0, 2)"))
        .expect("lo_lseek SEEK_END");
    match &seek[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(5));
        }
        other => panic!("expected Query, got {other:?}"),
    }
    // Negative offset from SEEK_END: -2 should return length-2 = 3.
    let seek_neg = engine
        .execute_sql(&session, &format!("SELECT lo_lseek({fd}, -2, 2)"))
        .expect("lo_lseek SEEK_END negative");
    match &seek_neg[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn lo_get_lo_put_roundtrip() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let create = engine
        .execute_sql(&session, "SELECT lo_create(0)")
        .expect("lo_create");
    let oid = match &create[0] {
        StatementResult::Query { rows, .. } => match rows[0].values[0] {
            aiondb_core::Value::Int(v) => v,
            ref other => panic!("expected Int oid, got {other:?}"),
        },
        other => panic!("expected Query, got {other:?}"),
    };

    engine
        .execute_sql(
            &session,
            &format!("SELECT lo_put({oid}, 0, '\\x68656c6c6f'::bytea)"),
        )
        .expect("lo_put");
    let get = engine
        .execute_sql(&session, &format!("SELECT lo_get({oid})"))
        .expect("lo_get");
    match &get[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                aiondb_core::Value::Blob(b"hello".to_vec())
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }

    // lo_open + lo_lseek + lo_tell
    let fd_open = engine
        .execute_sql(&session, &format!("SELECT lo_open({oid}, 393216)"))
        .expect("lo_open");
    let fd = match &fd_open[0] {
        StatementResult::Query { rows, .. } => match rows[0].values[0] {
            aiondb_core::Value::Int(v) => v,
            ref other => panic!("expected Int fd, got {other:?}"),
        },
        other => panic!("expected Query, got {other:?}"),
    };
    let seek = engine
        .execute_sql(&session, &format!("SELECT lo_lseek({fd}, 2, 0)"))
        .expect("lo_lseek");
    match &seek[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
    let tell = engine
        .execute_sql(&session, &format!("SELECT lo_tell({fd})"))
        .expect("lo_tell");
    match &tell[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}
