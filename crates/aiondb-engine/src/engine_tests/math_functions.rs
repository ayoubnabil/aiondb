#![allow(clippy::pedantic)]

use super::*;

fn query_double(engine: &Engine, session: &SessionHandle, sql: &str) -> f64 {
    match query_single_value(engine, session, sql) {
        aiondb_core::Value::Double(v) => v,
        aiondb_core::Value::Int(v) => v as f64,
        aiondb_core::Value::BigInt(v) => v as f64,
        aiondb_core::Value::Numeric(v) => v.to_f64(),
        other => panic!("expected numeric value, got {other:?}"),
    }
}

// ---- abs ----

#[test]
fn abs_positive_int() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT abs(42)");
    assert!((val - 42.0).abs() < 1e-10);
}

#[test]
fn abs_negative_int() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT abs(-7)");
    assert!((val - 7.0).abs() < 1e-10);
}

#[test]
fn abs_negative_double() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT abs(-3.14)");
    assert!((val - 3.14).abs() < 1e-10);
}

#[test]
fn abs_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT abs(NULL)");
    assert_eq!(val, aiondb_core::Value::Null);
}

// ---- ceil / ceiling ----

#[test]
fn ceil_positive() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT ceil(2.3)");
    assert!((val - 3.0).abs() < 1e-10);
}

#[test]
fn ceil_negative() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT ceil(-2.3)");
    assert!((val - (-2.0)).abs() < 1e-10);
}

#[test]
fn ceiling_alias() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT ceiling(1.1)");
    assert!((val - 2.0).abs() < 1e-10);
}

// ---- floor ----

#[test]
fn floor_positive() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT floor(2.7)");
    assert!((val - 2.0).abs() < 1e-10);
}

#[test]
fn floor_negative() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT floor(-2.3)");
    assert!((val - (-3.0)).abs() < 1e-10);
}

// ---- round ----

#[test]
fn round_no_places() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT round(2.5)");
    assert!((val - 3.0).abs() < 1e-10);
}

#[test]
fn round_with_places() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT round(2.3456, 2)");
    assert!((val - 2.35).abs() < 1e-10);
}

// ---- trunc ----

#[test]
fn trunc_positive() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT trunc(2.9)");
    assert!((val - 2.0).abs() < 1e-10);
}

#[test]
fn trunc_with_places() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT trunc(2.3456, 2)");
    assert!((val - 2.34).abs() < 1e-10);
}

// ---- power ----

#[test]
fn power_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT power(2, 3)");
    assert!((val - 8.0).abs() < 1e-10);
}

#[test]
fn power_zero_exponent() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT power(5, 0)");
    assert!((val - 1.0).abs() < 1e-10);
}

#[test]
fn exponent_operator_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT 2 ^ 3");
    assert!((val - 8.0).abs() < 1e-10);
}

#[test]
fn pow_alias() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT pow(3, 2)");
    assert!((val - 9.0).abs() < 1e-10);
}

// ---- sqrt ----

#[test]
fn sqrt_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT sqrt(9)");
    assert!((val - 3.0).abs() < 1e-10);
}

#[test]
fn sqrt_negative_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let result = engine.execute_sql(&session, "SELECT sqrt(-1)");
    assert!(result.is_err());
}

// ---- log ----

#[test]
fn log_one_arg() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT log(100)");
    assert!((val - 2.0).abs() < 1e-10);
}

#[test]
fn log_two_args() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT log(2, 8)");
    assert!((val - 3.0).abs() < 1e-10);
}

#[test]
fn log_zero_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let result = engine.execute_sql(&session, "SELECT log(0)");
    assert!(result.is_err());
}

// ---- ln ----

#[test]
fn ln_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT ln(1)");
    assert!((val - 0.0).abs() < 1e-10);
}

#[test]
fn ln_zero_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let result = engine.execute_sql(&session, "SELECT ln(0)");
    assert!(result.is_err());
}

// ---- exp ----

#[test]
fn exp_zero() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT exp(0)");
    assert!((val - 1.0).abs() < 1e-10);
}

// ---- mod ----

#[test]
fn mod_int_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT mod(10, 3)");
    assert!((val - 1.0).abs() < 1e-10);
}

#[test]
fn mod_zero_divisor_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let result = engine.execute_sql(&session, "SELECT mod(10, 0)");
    assert!(result.is_err());
}

#[test]
fn mod_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT mod(NULL, 3)");
    assert_eq!(val, aiondb_core::Value::Null);
}

#[test]
fn num_nonnulls_variadic_array_matches_postgres() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &session,
        "SELECT num_nonnulls(VARIADIC '{1,2,NULL,3}'::int[])",
    );
    assert_eq!(val, aiondb_core::Value::Int(3));
}

#[test]
fn num_nulls_variadic_array_matches_postgres() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &session,
        "SELECT num_nulls(VARIADIC '{1,NULL,2,NULL}'::int[])",
    );
    assert_eq!(val, aiondb_core::Value::Int(2));
}

#[test]
fn num_nonnulls_variadic_null_array_returns_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &session,
        "SELECT num_nonnulls(VARIADIC NULL::text[])",
    );
    assert_eq!(val, aiondb_core::Value::Null);
}

#[test]
fn num_nulls_variadic_null_array_returns_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT num_nulls(VARIADIC NULL::text[])");
    assert_eq!(val, aiondb_core::Value::Null);
}

#[test]
fn num_nonnulls_without_arguments_is_rejected_like_pg() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let error = engine
        .execute_sql(&session, "SELECT num_nonnulls()")
        .expect_err("zero-arg num_nonnulls should fail");
    let report = error.report();
    assert!(
        report
            .message
            .contains("function num_nonnulls() does not exist"),
        "unexpected error: {}",
        report.message
    );
    assert_eq!(
        report.client_hint.as_deref(),
        Some(
            "No function matches the given name and argument types. You might need to add explicit type casts."
        )
    );
}

#[test]
fn num_nulls_without_arguments_is_rejected_like_pg() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let error = engine
        .execute_sql(&session, "SELECT num_nulls()")
        .expect_err("zero-arg num_nulls should fail");
    let report = error.report();
    assert!(
        report
            .message
            .contains("function num_nulls() does not exist"),
        "unexpected error: {}",
        report.message
    );
    assert_eq!(
        report.client_hint.as_deref(),
        Some(
            "No function matches the given name and argument types. You might need to add explicit type casts."
        )
    );
}

#[test]
fn bitwise_and_shift_operators_work() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT 5 & 3, 5 | 2, 5 # 1, 1 << 3, 8 >> 2")
        .expect("bitwise operators");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values,
                vec![
                    aiondb_core::Value::Int(1),
                    aiondb_core::Value::Int(7),
                    aiondb_core::Value::Int(4),
                    aiondb_core::Value::Int(8),
                    aiondb_core::Value::Int(2),
                ]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn unary_bitwise_not_and_abs_operator_work() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT ~1, @-7")
        .expect("unary operators");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(-2));
            match &rows[0].values[1] {
                aiondb_core::Value::Int(v) => assert_eq!(*v, 7),
                aiondb_core::Value::Double(v) => assert!((*v - 7.0).abs() < 1e-10),
                other => panic!("expected double, got {other:?}"),
            }
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

// ---- sign ----

#[test]
fn sign_positive() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT sign(42)");
    assert!((val - 1.0).abs() < 1e-10);
}

#[test]
fn sign_negative() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT sign(-7)");
    assert!((val - (-1.0)).abs() < 1e-10);
}

#[test]
fn sign_zero() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT sign(0)");
    assert!((val - 0.0).abs() < 1e-10);
}

// ---- pi ----

#[test]
fn pi_value() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT pi()");
    assert!((val - std::f64::consts::PI).abs() < 1e-10);
}

// ---- random ----

#[test]
fn random_in_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT random()");
    assert!((0.0..1.0).contains(&val), "random() = {val}, not in [0,1)");
}

// ---- greatest ----

#[test]
fn greatest_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT greatest(1, 5, 3)");
    assert!((val - 5.0).abs() < 1e-10);
}

#[test]
fn greatest_with_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT greatest(NULL, 2, NULL, 7)");
    assert!((val - 7.0).abs() < 1e-10);
}

#[test]
fn greatest_all_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT greatest(NULL, NULL)");
    assert_eq!(val, aiondb_core::Value::Null);
}

// ---- least ----

#[test]
fn least_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT least(4, 1, 9)");
    assert!((val - 1.0).abs() < 1e-10);
}

#[test]
fn least_with_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_double(&engine, &session, "SELECT least(NULL, 8, 2, NULL)");
    assert!((val - 2.0).abs() < 1e-10);
}

#[test]
fn least_all_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT least(NULL, NULL)");
    assert_eq!(val, aiondb_core::Value::Null);
}

// ---- math functions in expressions ----

#[test]
fn math_functions_in_select_with_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE nums (val INT); \
             INSERT INTO nums VALUES (-7), (3), (0)",
        )
        .expect("setup");

    // abs on table column
    let results = engine
        .execute_sql(&session, "SELECT abs(val) FROM nums ORDER BY abs(val)")
        .expect("abs query");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected query");
    };
    assert_eq!(rows.len(), 3);
    let vals: Vec<f64> = rows
        .iter()
        .map(|r| match &r.values[0] {
            aiondb_core::Value::Double(v) => *v,
            aiondb_core::Value::Int(v) => *v as f64,
            aiondb_core::Value::BigInt(v) => *v as f64,
            other => panic!("expected numeric, got {other:?}"),
        })
        .collect();
    assert!((vals[0] - 0.0).abs() < 1e-10);
    assert!((vals[1] - 3.0).abs() < 1e-10);
    assert!((vals[2] - 7.0).abs() < 1e-10);
}

#[test]
fn combined_math_expression() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    // round(sqrt(power(3,2) + power(4,2)), 2) should be 5.0
    let val = query_double(
        &engine,
        &session,
        "SELECT round(sqrt(power(3, 2) + power(4, 2)), 2)",
    );
    assert!((val - 5.0).abs() < 1e-10);
}
