use aiondb_core::Value;

use super::*;

// ---------------------------------------------------------------
// 18. Subquery/VALUES column aliasing in FROM clause
// ---------------------------------------------------------------

#[test]
fn subquery_values_column_alias_single() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // FROM (VALUES ...) AS v(x) should rename the column to x
    let rows = query_rows(
        &engine,
        &session,
        "SELECT x FROM (VALUES (1), (2), (3)) AS v(x)",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(3));
}

#[test]
fn subquery_values_column_alias_multiple() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // FROM (VALUES ...) v(a, b) should rename columns to a and b
    let rows = query_rows(
        &engine,
        &session,
        "SELECT a, b FROM (VALUES (10, 20)) v(a, b)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(10));
    assert_eq!(rows[0].values[1], Value::Int(20));
}

#[test]
fn subquery_select_column_alias() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // FROM (SELECT ...) AS t(x, y) should rename output columns
    let rows = query_rows(
        &engine,
        &session,
        "SELECT x, y FROM (SELECT 42, 99) AS t(x, y)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(42));
    assert_eq!(rows[0].values[1], Value::Int(99));
}

#[test]
fn cte_with_column_aliases_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // WITH t(a, b) AS (VALUES (1, 2)) should rename columns
    let rows = query_rows(
        &engine,
        &session,
        "WITH t(a, b) AS (VALUES (1, 2)) SELECT a, b FROM t",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[0].values[1], Value::Int(2));
}

#[test]
fn cte_column_aliases_three_cols() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // WITH t(b, p, bc_result) AS (VALUES (...)) SELECT * FROM t
    let rows = query_rows(
        &engine,
        &session,
        "WITH t(b, p, bc_result) AS (VALUES (1, 2, 3)) SELECT bc_result FROM t",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(3));
}

// ---------------------------------------------------------------
// 20. Column aliases used inside complex expressions (cast, function)
// ---------------------------------------------------------------

#[test]
fn values_column_alias_inside_cast() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Column alias 'x' used inside a cast expression x::int
    let rows = query_rows(&engine, &session, "SELECT x::int FROM (VALUES (42)) v(x)");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(42));
}

#[test]
fn values_column_alias_inside_function_and_cast() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Column alias 'x' used inside a function call with a cast: abs(x::int)
    let rows = query_rows(
        &engine,
        &session,
        "SELECT abs(x::int) FROM (VALUES (-42)) v(x)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(42));
}

#[test]
fn values_column_alias_in_binary_expr() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Column alias used in a binary expression
    let rows = query_rows(&engine, &session, "SELECT x + 10 FROM (VALUES (5)) v(x)");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(15));
}

#[test]
fn aggregate_over_values_set_operation_source() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT sum(x) FROM (VALUES (1), (2), (3)) AS v(x)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(6));
}

#[test]
fn parenthesized_table_ref_column_aliases_survive_binding() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE parenthesized_alias_users (id INT)")
        .expect("create table");
    engine
        .execute_sql(
            &session,
            "INSERT INTO parenthesized_alias_users VALUES (42)",
        )
        .expect("insert row");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT x FROM (parenthesized_alias_users) AS u(x)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(42));
}
