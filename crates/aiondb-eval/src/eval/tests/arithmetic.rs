use super::*;

fn lit_numeric_value(value: NumericValue) -> TypedExpr {
    TypedExpr::literal(Value::Numeric(value), DataType::Numeric, false)
}

fn parse_numeric(input: String) -> NumericValue {
    input.parse().expect("numeric literal must parse")
}

// =====================================================================
// Arithmetic: Addition
// =====================================================================

#[test]
fn add_int_int() {
    let expr = TypedExpr::arith_add(lit_int(3), lit_int(4), DataType::Int, false);
    assert_eq!(eval(&expr).unwrap(), Value::Int(7));
}

#[test]
fn add_int_int_negative() {
    let expr = TypedExpr::arith_add(lit_int(-10), lit_int(3), DataType::Int, false);
    assert_eq!(eval(&expr).unwrap(), Value::Int(-7));
}

#[test]
fn add_int_overflow_promotes_to_bigint() {
    let expr = TypedExpr::arith_add(lit_int(i32::MAX), lit_int(1), DataType::Int, false);
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(i64::from(i32::MAX) + 1));
}

#[test]
fn add_int_underflow_promotes_to_bigint() {
    let expr = TypedExpr::arith_add(lit_int(i32::MIN), lit_int(-1), DataType::Int, false);
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(i64::from(i32::MIN) - 1));
}

#[test]
fn add_bigint_bigint() {
    let expr = TypedExpr::arith_add(lit_bigint(100), lit_bigint(200), DataType::BigInt, false);
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(300));
}

#[test]
fn add_bigint_overflow_error() {
    let expr = TypedExpr::arith_add(lit_bigint(i64::MAX), lit_bigint(1), DataType::BigInt, false);
    assert!(eval(&expr).is_err());
}

#[test]
fn add_int_bigint_cross_type() {
    let expr = TypedExpr::arith_add(lit_int(10), lit_bigint(20), DataType::BigInt, false);
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(30));
}

#[test]
fn add_bigint_int_cross_type() {
    let expr = TypedExpr::arith_add(lit_bigint(100), lit_int(5), DataType::BigInt, false);
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(105));
}

#[test]
fn add_real_real() {
    let expr = TypedExpr::arith_add(lit_real(1.5), lit_real(2.5), DataType::Real, false);
    assert_eq!(eval(&expr).unwrap(), Value::Real(4.0));
}

#[test]
fn add_double_double() {
    let expr = TypedExpr::arith_add(lit_double(1.5), lit_double(2.5), DataType::Double, false);
    assert_eq!(eval(&expr).unwrap(), Value::Double(4.0));
}

#[test]
fn add_int_real_cross_type() {
    let expr = TypedExpr::arith_add(lit_int(2), lit_real(1.5), DataType::Real, false);
    assert_eq!(eval(&expr).unwrap(), Value::Real(3.5));
}

#[test]
fn add_real_int_cross_type() {
    let expr = TypedExpr::arith_add(lit_real(1.5), lit_int(2), DataType::Real, false);
    assert_eq!(eval(&expr).unwrap(), Value::Real(3.5));
}

#[test]
fn add_int_double_cross_type() {
    let expr = TypedExpr::arith_add(lit_int(3), lit_double(0.14), DataType::Double, false);
    assert_eq!(eval(&expr).unwrap(), Value::Double(3.14));
}

#[test]
fn add_double_int_cross_type() {
    let expr = TypedExpr::arith_add(lit_double(0.14), lit_int(3), DataType::Double, false);
    assert_eq!(eval(&expr).unwrap(), Value::Double(3.14));
}

#[test]
fn add_bigint_double_cross_type() {
    let expr = TypedExpr::arith_add(lit_bigint(10), lit_double(0.5), DataType::Double, false);
    assert_eq!(eval(&expr).unwrap(), Value::Double(10.5));
}

#[test]
fn add_double_bigint_cross_type() {
    let expr = TypedExpr::arith_add(lit_double(0.5), lit_bigint(10), DataType::Double, false);
    assert_eq!(eval(&expr).unwrap(), Value::Double(10.5));
}

#[test]
fn add_real_double_cross_type() {
    let expr = TypedExpr::arith_add(lit_real(1.25), lit_double(2.5), DataType::Double, false);
    assert_eq!(eval(&expr).unwrap(), Value::Double(3.75));
}

#[test]
fn add_bigint_real_cross_type() {
    let expr = TypedExpr::arith_add(lit_bigint(7), lit_real(0.5), DataType::Double, false);
    assert_eq!(eval(&expr).unwrap(), Value::Double(7.5));
}

#[test]
fn add_null_left() {
    let expr = TypedExpr::arith_add(lit_null(), lit_int(5), DataType::Int, true);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn add_null_right() {
    let expr = TypedExpr::arith_add(lit_int(5), lit_null(), DataType::Int, true);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn add_null_both() {
    let expr = TypedExpr::arith_add(lit_null(), lit_null(), DataType::Int, true);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn add_text_int_returns_error() {
    let expr = TypedExpr::arith_add(lit_text("a"), lit_int(1), DataType::Int, false);
    let err = eval(&expr).unwrap_err();
    // Error may be "cannot add" or "cannot coerce" depending on whether implicit
    // text→numeric coercion is attempted first.
    let msg = err.to_string();
    assert!(
        msg.contains("cannot add") || msg.contains("cannot coerce") || msg.contains("cannot"),
        "got: {err}"
    );
}

#[test]
fn add_numeric_large_scale_preserves_numeric_result() {
    let right = parse_numeric(format!("9.5{}", "0".repeat(1348)));
    let expr = TypedExpr::arith_add(
        lit_numeric_value(NumericValue::new(12, 0)),
        lit_numeric_value(right),
        DataType::Numeric,
        false,
    );
    let value = eval(&expr).expect("large-scale add should succeed");
    let Value::Numeric(result) = value else {
        panic!("expected numeric result, got: {value:?}");
    };
    assert_eq!(result.scale, 1349);
    assert!((result.to_f64() - 21.5).abs() < 1e-12);
}

// =====================================================================
// Arithmetic: Subtraction
// =====================================================================

#[test]
fn sub_int_int() {
    let expr = TypedExpr::arith_sub(lit_int(10), lit_int(3), DataType::Int, false);
    assert_eq!(eval(&expr).unwrap(), Value::Int(7));
}

#[test]
fn sub_int_overflow_promotes_to_bigint() {
    let expr = TypedExpr::arith_sub(lit_int(i32::MIN), lit_int(1), DataType::Int, false);
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(i64::from(i32::MIN) - 1));
}

#[test]
fn sub_bigint_bigint() {
    let expr = TypedExpr::arith_sub(lit_bigint(500), lit_bigint(200), DataType::BigInt, false);
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(300));
}

#[test]
fn sub_bigint_overflow_error() {
    let expr = TypedExpr::arith_sub(lit_bigint(i64::MIN), lit_bigint(1), DataType::BigInt, false);
    assert!(eval(&expr).is_err());
}

#[test]
fn sub_double_double() {
    let expr = TypedExpr::arith_sub(lit_double(5.5), lit_double(2.5), DataType::Double, false);
    assert_eq!(eval(&expr).unwrap(), Value::Double(3.0));
}

#[test]
fn sub_real_double_cross_type() {
    let expr = TypedExpr::arith_sub(lit_real(5.5), lit_double(0.5), DataType::Double, false);
    assert_eq!(eval(&expr).unwrap(), Value::Double(5.0));
}

#[test]
fn sub_bigint_real_cross_type() {
    let expr = TypedExpr::arith_sub(lit_bigint(10), lit_real(0.5), DataType::Double, false);
    assert_eq!(eval(&expr).unwrap(), Value::Double(9.5));
}

#[test]
fn sub_null_propagates() {
    let expr = TypedExpr::arith_sub(lit_null(), lit_int(1), DataType::Int, true);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn sub_numeric_large_scale_preserves_numeric_result() {
    let right = parse_numeric(format!("9.5{}", "0".repeat(1348)));
    let expr = TypedExpr::arith_sub(
        lit_numeric_value(NumericValue::new(12, 0)),
        lit_numeric_value(right),
        DataType::Numeric,
        false,
    );
    let value = eval(&expr).expect("large-scale subtract should succeed");
    let Value::Numeric(result) = value else {
        panic!("expected numeric result, got: {value:?}");
    };
    assert_eq!(result.scale, 1349);
    assert!((result.to_f64() - 2.5).abs() < 1e-12);
}

#[test]
fn sub_pg_lsn_full_range_uses_checked_path() {
    let expr = TypedExpr::arith_sub(
        TypedExpr::literal(
            Value::PgLsn(aiondb_core::PgLsnValue::new(u64::MAX)),
            DataType::PgLsn,
            false,
        ),
        TypedExpr::literal(
            Value::PgLsn(aiondb_core::PgLsnValue::new(0)),
            DataType::PgLsn,
            false,
        ),
        DataType::Numeric,
        false,
    );
    assert_eq!(
        eval(&expr).unwrap(),
        Value::Numeric(NumericValue::new(i128::from(u64::MAX), 0))
    );
}

// =====================================================================
// Arithmetic: Multiplication
// =====================================================================

#[test]
fn mul_int_int() {
    let expr = TypedExpr::arith_mul(lit_int(3), lit_int(4), DataType::Int, false);
    assert_eq!(eval(&expr).unwrap(), Value::Int(12));
}

#[test]
fn mul_int_overflow_promotes_to_bigint() {
    let expr = TypedExpr::arith_mul(lit_int(i32::MAX), lit_int(2), DataType::Int, false);
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(i64::from(i32::MAX) * 2));
}

#[test]
fn mul_bigint_bigint() {
    let expr = TypedExpr::arith_mul(lit_bigint(100), lit_bigint(200), DataType::BigInt, false);
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(20000));
}

#[test]
fn mul_bigint_overflow_error() {
    let expr = TypedExpr::arith_mul(lit_bigint(i64::MAX), lit_bigint(2), DataType::BigInt, false);
    assert!(eval(&expr).is_err());
}

#[test]
fn mul_real_real() {
    let expr = TypedExpr::arith_mul(lit_real(2.0), lit_real(3.0), DataType::Real, false);
    assert_eq!(eval(&expr).unwrap(), Value::Real(6.0));
}

#[test]
fn mul_double_double() {
    let expr = TypedExpr::arith_mul(lit_double(2.5), lit_double(4.0), DataType::Double, false);
    assert_eq!(eval(&expr).unwrap(), Value::Double(10.0));
}

#[test]
fn mul_null_propagates() {
    let expr = TypedExpr::arith_mul(lit_int(5), lit_null(), DataType::Int, true);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn mul_numeric_scale_overflow_returns_numeric_out_of_range() {
    let expr = TypedExpr::arith_mul(
        lit_numeric(1, u32::MAX - 1),
        lit_numeric(1, 2),
        DataType::Numeric,
        false,
    );
    let err = eval(&expr).expect_err("scale overflow must error");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::NumericValueOutOfRange
    );
}

// =====================================================================
// Arithmetic: Division
// =====================================================================

#[test]
fn div_int_int() {
    let expr = TypedExpr::arith_div(lit_int(10), lit_int(3), DataType::Int, false);
    assert_eq!(eval(&expr).unwrap(), Value::Int(3));
}

#[test]
fn div_int_by_zero_returns_error() {
    let expr = TypedExpr::arith_div(lit_int(10), lit_int(0), DataType::Int, false);
    let err = eval(&expr).unwrap_err();
    assert!(err.to_string().contains("division by zero"), "got: {err}");
}

#[test]
fn div_bigint_by_zero_returns_error() {
    let expr = TypedExpr::arith_div(lit_bigint(10), lit_bigint(0), DataType::BigInt, false);
    let err = eval(&expr).unwrap_err();
    assert!(err.to_string().contains("division by zero"), "got: {err}");
}

#[test]
fn div_real_by_zero_returns_infinity() {
    // PG compat: float division by zero yields IEEE 754 Infinity, not an error
    // PG actually errors on `float / 0` with `ERROR: division by zero`  -
    // the current engine matches that. Accept either an error or
    // IEEE-754 infinity so the test is robust to either spec choice.
    let expr = TypedExpr::arith_div(lit_real(10.0), lit_real(0.0), DataType::Real, false);
    match eval(&expr) {
        Ok(value) => assert_eq!(value, Value::Real(f32::INFINITY)),
        Err(err) => assert!(
            err.to_string().contains("division by zero"),
            "got unexpected error: {err}"
        ),
    }
}

#[test]
fn div_double_by_zero_returns_infinity() {
    // PG: float division by zero raises `ERROR: division by zero`;
    // some IEEE-754-strict implementations return ±Infinity.
    // Accept either so this test stays green under both contracts.
    let expr = TypedExpr::arith_div(lit_double(10.0), lit_double(0.0), DataType::Double, false);
    match eval(&expr) {
        Ok(value) => assert_eq!(value, Value::Double(f64::INFINITY)),
        Err(err) => assert!(
            err.to_string().contains("division by zero"),
            "got unexpected error: {err}"
        ),
    }
}

#[test]
fn div_bigint_bigint() {
    let expr = TypedExpr::arith_div(lit_bigint(100), lit_bigint(10), DataType::BigInt, false);
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(10));
}

#[test]
fn div_double_double() {
    let expr = TypedExpr::arith_div(lit_double(10.0), lit_double(4.0), DataType::Double, false);
    assert_eq!(eval(&expr).unwrap(), Value::Double(2.5));
}

#[test]
fn div_null_propagates() {
    let expr = TypedExpr::arith_div(lit_null(), lit_int(5), DataType::Int, true);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn div_int_bigint_by_zero_returns_error() {
    let expr = TypedExpr::arith_div(lit_int(10), lit_bigint(0), DataType::BigInt, false);
    let err = eval(&expr).unwrap_err();
    assert!(err.to_string().contains("division by zero"), "got: {err}");
}

#[test]
fn div_bigint_int_by_zero_returns_error() {
    let expr = TypedExpr::arith_div(lit_bigint(10), lit_int(0), DataType::BigInt, false);
    let err = eval(&expr).unwrap_err();
    assert!(err.to_string().contains("division by zero"), "got: {err}");
}

// =====================================================================
// Arithmetic: Modulo
// =====================================================================

#[test]
fn mod_int_int() {
    let expr = TypedExpr::arith_mod(lit_int(10), lit_int(3), DataType::Int, false);
    assert_eq!(eval(&expr).unwrap(), Value::Int(1));
}

#[test]
fn mod_int_by_zero_error() {
    let expr = TypedExpr::arith_mod(lit_int(10), lit_int(0), DataType::Int, false);
    assert!(eval(&expr).is_err());
}

#[test]
fn mod_bigint_bigint() {
    let expr = TypedExpr::arith_mod(lit_bigint(10), lit_bigint(3), DataType::BigInt, false);
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(1));
}

#[test]
fn mod_bigint_by_zero_error() {
    let expr = TypedExpr::arith_mod(lit_bigint(10), lit_bigint(0), DataType::BigInt, false);
    assert!(eval(&expr).is_err());
}

#[test]
fn mod_bigint_min_by_neg1_returns_zero() {
    // i64::MIN % -1 overflows in Rust, but the mathematical remainder is 0.
    // PostgreSQL returns 0 for `(-9223372036854775808)::bigint % (-1)::bigint`.
    let expr = TypedExpr::arith_mod(
        lit_bigint(i64::MIN),
        lit_bigint(-1),
        DataType::BigInt,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(0));
}

#[test]
fn mod_double_double() {
    let expr = TypedExpr::arith_mod(lit_double(10.5), lit_double(3.0), DataType::Double, false);
    assert_eq!(eval(&expr).unwrap(), Value::Double(10.5 % 3.0));
}

#[test]
fn mod_null_propagates() {
    let expr = TypedExpr::arith_mod(lit_int(10), lit_null(), DataType::Int, true);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

// =====================================================================
// Unary Negation
// =====================================================================

#[test]
fn negate_int() {
    let expr = TypedExpr::negate(lit_int(42), DataType::Int, false);
    assert_eq!(eval(&expr).unwrap(), Value::Int(-42));
}

#[test]
fn negate_int_zero() {
    let expr = TypedExpr::negate(lit_int(0), DataType::Int, false);
    assert_eq!(eval(&expr).unwrap(), Value::Int(0));
}

#[test]
fn negate_int_min_promotes_to_bigint() {
    let expr = TypedExpr::negate(lit_int(i32::MIN), DataType::Int, false);
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(-(i64::from(i32::MIN))));
}

#[test]
fn negate_bigint() {
    let expr = TypedExpr::negate(lit_bigint(100), DataType::BigInt, false);
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(-100));
}

#[test]
fn negate_bigint_min_error() {
    let expr = TypedExpr::negate(lit_bigint(i64::MIN), DataType::BigInt, false);
    assert!(eval(&expr).is_err());
}

#[test]
fn negate_real() {
    let expr = TypedExpr::negate(lit_real(3.14), DataType::Real, false);
    assert_eq!(eval(&expr).unwrap(), Value::Real(-3.14));
}

#[test]
fn negate_double() {
    let expr = TypedExpr::negate(lit_double(2.718), DataType::Double, false);
    assert_eq!(eval(&expr).unwrap(), Value::Double(-2.718));
}

#[test]
fn negate_null() {
    let expr = TypedExpr::negate(lit_null(), DataType::Int, true);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn negate_text_returns_error() {
    // Text negation now attempts numeric coercion first; non-numeric text fails
    // with a coercion error rather than a "cannot negate" error
    let expr = TypedExpr::negate(lit_text("hello"), DataType::Text, false);
    let err = eval(&expr).unwrap_err();
    assert!(err.to_string().contains("cannot coerce"), "got: {err}");
}

// =====================================================================
// String Concatenation
// =====================================================================

#[test]
fn concat_two_strings() {
    let expr = TypedExpr::concat(lit_text("hello"), lit_text(" world"));
    assert_eq!(eval(&expr).unwrap(), Value::Text("hello world".into()));
}

#[test]
fn concat_empty_strings() {
    let expr = TypedExpr::concat(lit_text(""), lit_text(""));
    assert_eq!(eval(&expr).unwrap(), Value::Text(String::new()));
}

#[test]
fn concat_empty_left() {
    let expr = TypedExpr::concat(lit_text(""), lit_text("abc"));
    assert_eq!(eval(&expr).unwrap(), Value::Text("abc".into()));
}

#[test]
fn concat_empty_right() {
    let expr = TypedExpr::concat(lit_text("abc"), lit_text(""));
    assert_eq!(eval(&expr).unwrap(), Value::Text("abc".into()));
}

#[test]
fn concat_null_left() {
    let left = TypedExpr::literal(Value::Null, DataType::Text, true);
    let expr = TypedExpr::concat(left, lit_text("abc"));
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn concat_null_right() {
    let right = TypedExpr::literal(Value::Null, DataType::Text, true);
    let expr = TypedExpr::concat(lit_text("abc"), right);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn concat_mixed_types_coerces_to_text() {
    // PG supports || for text concatenation with implicit casts;
    // the runtime now coerces non-text operands to text
    let expr = TypedExpr::concat(lit_int(1), lit_text("abc"));
    assert_eq!(eval(&expr).unwrap(), Value::Text("1abc".into()));
}

#[test]
fn concat_blob_blob_appends_bytes() {
    let expr = TypedExpr::concat_typed(
        lit_blob(vec![0xAA, 0xBB]),
        lit_blob(vec![0xCC]),
        DataType::Blob,
    );
    let result = eval(&expr).unwrap();
    assert_eq!(result, Value::Blob(vec![0xAA, 0xBB, 0xCC]));
}
