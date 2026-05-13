use super::*;

// ===================================================================
// cardinality function
// ===================================================================

#[test]
fn cardinality_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT cardinality(ARRAY[10, 20, 30])")
        .expect("cardinality");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(3));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn cardinality_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, NULL)")
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT cardinality(vals) FROM t")
        .expect("cardinality null");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Null);
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// || array concatenation operator
// ===================================================================

#[test]
fn array_concat_operator_two_arrays() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT ARRAY[1, 2] || ARRAY[3, 4]")
        .expect("array || array");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![
                    Value::Int(1),
                    Value::Int(2),
                    Value::Int(3),
                    Value::Int(4)
                ])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_concat_operator_with_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, ARRAY[1, 2])")
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT vals || ARRAY[3, 4] FROM t")
        .expect("column || array");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![
                    Value::Int(1),
                    Value::Int(2),
                    Value::Int(3),
                    Value::Int(4)
                ])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// @> array contains operator
// ===================================================================

#[test]
fn array_contains_operator_true() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT ARRAY[1, 2, 3] @> ARRAY[2, 3]")
        .expect("array @> true");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Boolean(true));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_contains_operator_false() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT ARRAY[1, 2, 3] @> ARRAY[4]")
        .expect("array @> false");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Boolean(false));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_contains_operator_subset() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // ARRAY[1,2,3] contains the subset ARRAY[1]
    let results = engine
        .execute_sql(&session, "SELECT ARRAY[1, 2, 3] @> ARRAY[1]")
        .expect("array @> subset");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Boolean(true));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// <@ array contained-by operator
// ===================================================================

#[test]
fn array_contained_by_operator_true() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT ARRAY[2, 3] <@ ARRAY[1, 2, 3]")
        .expect("array <@ true");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Boolean(true));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_contained_by_operator_false() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT ARRAY[2, 4] <@ ARRAY[1, 2, 3]")
        .expect("array <@ false");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Boolean(false));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// && array overlap operator
// ===================================================================

#[test]
fn array_overlap_operator_true() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT ARRAY[1, 2] && ARRAY[2, 3]")
        .expect("array && true");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Boolean(true));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_overlap_operator_false() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT ARRAY[1, 2] && ARRAY[3, 4]")
        .expect("array && false");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Boolean(false));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_overlap_operator_with_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, tags INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, ARRAY[10, 20, 30])")
        .expect("insert 1");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (2, ARRAY[40, 50])")
        .expect("insert 2");

    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM t WHERE tags && ARRAY[20, 40] ORDER BY id",
        )
        .expect("overlap filter");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[1].values[0], Value::Int(2));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// Combined: functions with table data
// ===================================================================

#[test]
fn array_functions_with_stored_data() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, ARRAY[10, 20, 30])")
        .expect("insert");

    // array_position on stored column
    let results = engine
        .execute_sql(&session, "SELECT array_position(vals, 20) FROM t")
        .expect("array_position from table");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(2));
        }
        other => panic!("expected query, got {other:?}"),
    }

    // cardinality on stored column
    let results = engine
        .execute_sql(&session, "SELECT cardinality(vals) FROM t")
        .expect("cardinality from table");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(3));
        }
        other => panic!("expected query, got {other:?}"),
    }

    // array_remove on stored column
    let results = engine
        .execute_sql(&session, "SELECT array_remove(vals, 20) FROM t")
        .expect("array_remove from table");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![Value::Int(10), Value::Int(30)])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// WHERE clause filtering with array operators
// ===================================================================

#[test]
fn array_contains_in_where_clause() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, tags INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, ARRAY[1, 2, 3])")
        .expect("insert 1");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (2, ARRAY[4, 5])")
        .expect("insert 2");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (3, ARRAY[1, 5, 6])")
        .expect("insert 3");

    // Only row 1 contains both 1 and 2
    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM t WHERE tags @> ARRAY[1, 2] ORDER BY id",
        )
        .expect("contains filter");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn text_concat_still_works() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Ensure || still works for text concatenation
    let results = engine
        .execute_sql(&session, "SELECT 'hello' || ' ' || 'world'")
        .expect("text concat");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("hello world".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}
