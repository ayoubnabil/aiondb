#![allow(clippy::unreadable_literal)]

use super::*;
use aiondb_core::NumericValue;

// ===================================================================
// CREATE TABLE with NUMERIC column and basic INSERT/SELECT
// ===================================================================

#[test]
fn create_table_with_numeric_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, price NUMERIC)")
        .expect("create table with numeric column");
}

#[test]
fn insert_and_select_numeric_literal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, val NUMERIC)")
        .expect("create table");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 123.45)")
        .expect("insert numeric literal");

    let results = engine
        .execute_sql(&session, "SELECT val FROM t WHERE id = 1")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Numeric(NumericValue::new(12345, 2))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn decimal_alias_for_numeric() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, val DECIMAL)")
        .expect("DECIMAL should be accepted as alias for NUMERIC");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 99.99)")
        .expect("insert into DECIMAL column");

    let results = engine
        .execute_sql(&session, "SELECT val FROM t WHERE id = 1")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Numeric(NumericValue::new(9999, 2))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn numeric_with_precision_and_scale() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // NUMERIC(10,2) should parse without error (precision/scale are accepted
    // but mapped to plain NUMERIC internally).
    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT NOT NULL, price NUMERIC(10, 2))",
        )
        .expect("NUMERIC(p,s) should be accepted");
}

// ===================================================================
// Arithmetic operations on numeric literals
// ===================================================================

#[test]
fn numeric_addition() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT 1.5 + 2.3")
        .expect("numeric addition");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Numeric(NumericValue::new(38, 1)));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn numeric_subtraction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT 10.0 - 3.5")
        .expect("numeric subtraction");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Numeric(NumericValue::new(65, 1)));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn numeric_multiplication() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT 2.5 * 4.0")
        .expect("numeric multiplication");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            // 2.5 (scale=1) * 4.0 (scale=1) = 10.00 (scale=2)
            assert_eq!(
                rows[0].values[0],
                Value::Numeric(NumericValue::new(1000, 2))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn numeric_division() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT 10.0 / 3.0")
        .expect("numeric division");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            // Division produces extra precision. The result should be close
            // to 3.333... We just check it parses and is roughly correct.
            let val = &rows[0].values[0];
            match val {
                Value::Numeric(nv) => {
                    let f = nv.coefficient as f64 / 10f64.powi(nv.scale as i32);
                    assert!((f - 3.333333).abs() < 0.001, "expected ~3.333, got {f}");
                }
                other => panic!("expected Numeric, got {other:?}"),
            }
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// Comparison operators
// ===================================================================

#[test]
fn numeric_comparison_in_where() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE products (id INT NOT NULL, price NUMERIC)",
        )
        .expect("create table");

    engine
        .execute_sql(&session, "INSERT INTO products VALUES (1, 5.99)")
        .expect("insert 1");
    engine
        .execute_sql(&session, "INSERT INTO products VALUES (2, 10.50)")
        .expect("insert 2");
    engine
        .execute_sql(&session, "INSERT INTO products VALUES (3, 25.00)")
        .expect("insert 3");

    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM products WHERE price > 10.50 ORDER BY id",
        )
        .expect("select with numeric comparison");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(3));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn numeric_equality_comparison() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, val NUMERIC)")
        .expect("create table");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 42.00)")
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT id FROM t WHERE val = 42.00")
        .expect("select with equality");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// INT to NUMERIC coercion
// ===================================================================

#[test]
fn int_to_numeric_coercion_on_insert() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, val NUMERIC)")
        .expect("create table");

    // Insert an integer value into a NUMERIC column -- should coerce
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 42)")
        .expect("insert int into numeric column");

    let results = engine
        .execute_sql(&session, "SELECT val FROM t WHERE id = 1")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Numeric(NumericValue::new(42, 0)));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn int_numeric_mixed_arithmetic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // INT + NUMERIC should coerce INT to NUMERIC and produce NUMERIC result
    let results = engine
        .execute_sql(&session, "SELECT 5 + 2.5")
        .expect("int + numeric");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Numeric(NumericValue::new(75, 1)));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// Aggregate functions on NUMERIC columns
// ===================================================================

#[test]
fn numeric_sum_aggregate() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, val NUMERIC)")
        .expect("create table");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 10.50)")
        .expect("insert 1");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (2, 20.25)")
        .expect("insert 2");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (3, 5.75)")
        .expect("insert 3");

    let results = engine
        .execute_sql(&session, "SELECT SUM(val) FROM t")
        .expect("sum");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Numeric(NumericValue::new(3650, 2))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn numeric_avg_aggregate() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, val NUMERIC)")
        .expect("create table");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 10.00)")
        .expect("insert 1");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (2, 20.00)")
        .expect("insert 2");

    let results = engine
        .execute_sql(&session, "SELECT AVG(val) FROM t")
        .expect("avg");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            // AVG of NUMERIC returns NUMERIC (PG compat)
            // 30.00 / 2 with extra_precision=6 → scale=8 → 1500000000
            assert_eq!(
                rows[0].values[0],
                Value::Numeric(NumericValue::new(1500000000, 8))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// CAST operations
// ===================================================================

#[test]
fn cast_int_to_numeric() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT CAST(42 AS NUMERIC)")
        .expect("cast int to numeric");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Numeric(NumericValue::new(42, 0)));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn cast_string_to_numeric() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT CAST('123.45' AS NUMERIC)")
        .expect("cast string to numeric");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Numeric(NumericValue::new(12345, 2))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn cast_numeric_to_text() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT CAST(99.50 AS TEXT)")
        .expect("cast numeric to text");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            // NumericValue::Display for 99.50 (coefficient=9950, scale=2) => "99.50"
            assert_eq!(rows[0].values[0], Value::Text("99.50".to_owned()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// Numeric literal parsing
// ===================================================================

#[test]
fn select_numeric_literal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT 123.45")
        .expect("select numeric literal");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Numeric(NumericValue::new(12345, 2))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn select_numeric_literal_whole_number_with_decimal_point() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT 100.0")
        .expect("select 100.0");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Numeric(NumericValue::new(1000, 1))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// Unary minus on NUMERIC
// ===================================================================

#[test]
fn numeric_unary_minus() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT -5.25")
        .expect("unary minus on numeric");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Numeric(NumericValue::new(-525, 2))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// NULL handling with NUMERIC
// ===================================================================

#[test]
fn numeric_null_value() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, val NUMERIC)")
        .expect("create table");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, NULL)")
        .expect("insert null numeric");

    let results = engine
        .execute_sql(&session, "SELECT val FROM t WHERE id = 1")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Null);
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// ORDER BY on NUMERIC columns
// ===================================================================

#[test]
fn numeric_order_by() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, val NUMERIC)")
        .expect("create table");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 30.5)")
        .expect("insert 1");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (2, 10.2)")
        .expect("insert 2");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (3, 20.8)")
        .expect("insert 3");

    let results = engine
        .execute_sql(&session, "SELECT id FROM t ORDER BY val")
        .expect("order by numeric");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values[0], Value::Int(2)); // 10.2
            assert_eq!(rows[1].values[0], Value::Int(3)); // 20.8
            assert_eq!(rows[2].values[0], Value::Int(1)); // 30.5
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// Multiple rows with numeric arithmetic in SELECT
// ===================================================================

#[test]
fn numeric_arithmetic_on_columns() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT NOT NULL, a NUMERIC, b NUMERIC)",
        )
        .expect("create table");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 10.5, 3.2)")
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT a + b, a - b, a * b FROM t WHERE id = 1")
        .expect("select arithmetic");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            // a + b = 10.5 + 3.2 = 13.7
            assert_eq!(rows[0].values[0], Value::Numeric(NumericValue::new(137, 1)));
            // a - b = 10.5 - 3.2 = 7.3
            assert_eq!(rows[0].values[1], Value::Numeric(NumericValue::new(73, 1)));
            // a * b = 10.5 * 3.2 = 33.60
            assert_eq!(
                rows[0].values[2],
                Value::Numeric(NumericValue::new(3360, 2))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}
