use super::*;
use aiondb_core::{IntervalValue, NumericValue};
use std::str::FromStr;

// -- to_f64 helper ------------------------------------------------

#[test]
fn to_f64_converts_int() {
    assert_eq!(to_f64(&Value::Int(42)).unwrap(), 42.0);
}

#[test]
fn to_f64_converts_numeric() {
    let n = NumericValue::new(12345, 2);
    let f = to_f64(&Value::Numeric(n)).unwrap();
    assert!((f - 123.45).abs() < 1e-10);
}

#[test]
fn to_f64_rejects_text() {
    assert!(to_f64(&Value::Text("nope".into())).is_err());
}

// -- abs ----------------------------------------------------------

#[test]
fn abs_positive_int() {
    let r = eval_abs(&[Value::Int(5)]).unwrap();
    assert_eq!(r, Value::Int(5));
}

#[test]
fn abs_negative_int() {
    let r = eval_abs(&[Value::Int(-7)]).unwrap();
    assert_eq!(r, Value::Int(7));
}

#[test]
fn abs_negative_double() {
    let r = eval_abs(&[Value::Double(-3.14)]).unwrap();
    assert_eq!(r, Value::Double(3.14));
}

#[test]
fn abs_null() {
    let r = eval_abs(&[Value::Null]).unwrap();
    assert_eq!(r, Value::Null);
}

#[test]
fn abs_bigint() {
    let r = eval_abs(&[Value::BigInt(-100)]).unwrap();
    assert_eq!(r, Value::BigInt(100));
}

// -- ceil ---------------------------------------------------------

#[test]
fn ceil_positive() {
    let r = eval_ceil(&[Value::Double(2.3)]).unwrap();
    assert_eq!(r, Value::Double(3.0));
}

#[test]
fn ceil_negative() {
    let r = eval_ceil(&[Value::Double(-2.3)]).unwrap();
    assert_eq!(r, Value::Double(-2.0));
}

#[test]
fn ceil_null() {
    assert_eq!(eval_ceil(&[Value::Null]).unwrap(), Value::Null);
}

// -- floor --------------------------------------------------------

#[test]
fn floor_positive() {
    let r = eval_floor(&[Value::Double(2.7)]).unwrap();
    assert_eq!(r, Value::Double(2.0));
}

#[test]
fn floor_negative() {
    let r = eval_floor(&[Value::Double(-2.3)]).unwrap();
    assert_eq!(r, Value::Double(-3.0));
}

#[test]
fn floor_null() {
    assert_eq!(eval_floor(&[Value::Null]).unwrap(), Value::Null);
}

#[test]
fn lcm_rejects_i64_min_without_panicking() {
    let err = eval_math_generic("lcm", &[Value::BigInt(i64::MIN), Value::BigInt(1)]).unwrap_err();
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::NumericValueOutOfRange
    );
}

#[test]
fn justify_hours_rolls_days_out_of_time_component() {
    let result = eval_math_generic(
        "justify_hours",
        &[Value::Interval(IntervalValue::new(
            6,
            3,
            52 * 3_600_000_000 + 3 * 60_000_000 + 2 * 1_000_000,
        ))],
    )
    .unwrap();

    assert_eq!(
        result,
        Value::Interval(IntervalValue::new(
            6,
            5,
            4 * 3_600_000_000 + 3 * 60_000_000 + 2 * 1_000_000,
        ))
    );
}

#[test]
fn justify_interval_aligns_mixed_sign_components() {
    let result = eval_math_generic(
        "justify_interval",
        &[Value::Interval(IntervalValue::new(1, 0, -3_600_000_000))],
    )
    .unwrap();

    assert_eq!(
        result,
        Value::Interval(IntervalValue::new(0, 29, 23 * 3_600_000_000))
    );
}

#[test]
fn justify_days_errors_when_month_field_overflows() {
    let err = eval_math_generic(
        "justify_days",
        &[Value::Interval(IntervalValue::new(i32::MAX, 30, 0))],
    )
    .unwrap_err();

    assert_eq!(err.report().message, "interval out of range");
}

// -- round --------------------------------------------------------

#[test]
fn round_no_places() {
    let r = eval_round(&[Value::Double(2.5)]).unwrap();
    assert_eq!(r, Value::Double(3.0));
}

#[test]
fn round_with_places() {
    let r = eval_round(&[Value::Double(2.3456), Value::Int(2)]).unwrap();
    assert_eq!(r, Value::Double(2.35));
}

#[test]
fn round_negative_value() {
    let r = eval_round(&[Value::Double(-1.5)]).unwrap();
    assert_eq!(r, Value::Double(-2.0));
}

#[test]
fn round_null() {
    assert_eq!(eval_round(&[Value::Null]).unwrap(), Value::Null);
}

// -- trunc --------------------------------------------------------

#[test]
fn trunc_positive() {
    let r = eval_trunc(&[Value::Double(2.9)]).unwrap();
    assert_eq!(r, Value::Double(2.0));
}

#[test]
fn trunc_negative() {
    let r = eval_trunc(&[Value::Double(-2.9)]).unwrap();
    assert_eq!(r, Value::Double(-2.0));
}

#[test]
fn trunc_with_places() {
    let r = eval_trunc(&[Value::Double(2.3456), Value::Int(2)]).unwrap();
    assert_eq!(r, Value::Double(2.34));
}

#[test]
fn trunc_null() {
    assert_eq!(eval_trunc(&[Value::Null]).unwrap(), Value::Null);
}

// -- power --------------------------------------------------------

#[test]
fn power_basic() {
    let r = eval_power(&[Value::Double(2.0), Value::Double(3.0)]).unwrap();
    assert_eq!(r, Value::Double(8.0));
}

#[test]
fn power_zero_exponent() {
    let r = eval_power(&[Value::Double(5.0), Value::Double(0.0)]).unwrap();
    assert_eq!(r, Value::Double(1.0));
}

#[test]
fn power_null() {
    assert_eq!(
        eval_power(&[Value::Null, Value::Double(2.0)]).unwrap(),
        Value::Null
    );
}

#[test]
fn power_numeric_integer_range_no_overflow_regression() {
    let base_a = Value::Numeric("0.084738".parse().unwrap());
    let base_b = Value::Numeric("37.821637".parse().unwrap());
    for p in -20..=20 {
        let exp = Value::Numeric(p.to_string().parse().unwrap());
        let r1 = eval_power(&[base_a.clone(), exp.clone()]);
        assert!(r1.is_ok(), "power(0.084738, {p}) failed: {r1:?}");
        let r2 = eval_power(&[base_b.clone(), exp]);
        assert!(r2.is_ok(), "power(37.821637, {p}) failed: {r2:?}");
    }
}

#[test]
fn power_numeric_integer_helper_detects_negative_integer() {
    let exp: NumericValue = "-20".parse().unwrap();
    assert_eq!(numeric_as_i64_if_integer(&exp), Some(-20));
}

#[test]
fn power_numeric_integer_precise_path_handles_negative_exponent() {
    let base: NumericValue = "0.084738".parse().unwrap();
    let exp: NumericValue = "-20".parse().unwrap();
    let out_scale =
        numeric_power_result_scale(base.scale, exp.scale, base.to_f64().powf(exp.to_f64()));
    let result = numeric_pow_precise_scaled(&base, &exp, out_scale);
    assert!(result.is_ok(), "precise power failed: {result:?}");
}

#[test]
fn power_numeric_integer_precise_path_handles_positive_exponent() {
    let base: NumericValue = "0.084738".parse().unwrap();
    let exp: NumericValue = "20".parse().unwrap();
    let out_scale =
        numeric_power_result_scale(base.scale, exp.scale, base.to_f64().powf(exp.to_f64()));
    let result = numeric_pow_precise_scaled(&base, &exp, out_scale);
    assert!(result.is_ok(), "precise power failed: {result:?}");
}

#[test]
fn ln_one_plus_tiny_numeric_no_overflow_regression() {
    for p in 1..=40 {
        let x: NumericValue = format!("1e-{p}").parse().unwrap();
        let one: NumericValue = "1.0".parse().unwrap();
        let expr = one.add(&x);
        let r = eval_ln(&[Value::Numeric(expr)]);
        assert!(r.is_ok(), "ln(1+1e-{p}) failed: {r:?}");
    }
}

#[test]
fn ln_one_plus_tiny_numeric_precise_helper_debug() {
    let x: NumericValue = "1e-11".parse().unwrap();
    let one: NumericValue = "1.0".parse().unwrap();
    let expr = one.add(&x);
    let rough = expr.to_f64().ln();
    let scale = pg_ln_result_scale(expr.scale, rough);
    let r = numeric_ln_precise_scaled(&expr, scale);
    assert!(r.is_ok(), "precise ln helper failed: {r:?}, scale={scale}");
}

// -- sqrt ---------------------------------------------------------

#[test]
fn sqrt_basic() {
    let r = eval_sqrt(&[Value::Double(9.0)]).unwrap();
    assert_eq!(r, Value::Double(3.0));
}

#[test]
fn sqrt_zero() {
    let r = eval_sqrt(&[Value::Double(0.0)]).unwrap();
    assert_eq!(r, Value::Double(0.0));
}

#[test]
fn sqrt_negative_errors() {
    assert!(eval_sqrt(&[Value::Double(-1.0)]).is_err());
}

#[test]
fn sqrt_null() {
    assert_eq!(eval_sqrt(&[Value::Null]).unwrap(), Value::Null);
}

// -- log (base 10) ------------------------------------------------

#[test]
fn log_basic() {
    let r = eval_log(&[Value::Double(100.0)]).unwrap();
    assert!(
        (r == Value::Double(2.0))
            || if let Value::Double(v) = r {
                (v - 2.0).abs() < 1e-10
            } else {
                false
            }
    );
}

#[test]
fn log_one() {
    let r = eval_log(&[Value::Double(1.0)]).unwrap();
    assert_eq!(r, Value::Double(0.0));
}

#[test]
fn log_zero_errors() {
    assert!(eval_log(&[Value::Double(0.0)]).is_err());
}

#[test]
fn log_negative_errors() {
    assert!(eval_log(&[Value::Double(-5.0)]).is_err());
}

#[test]
fn log_null() {
    assert_eq!(eval_log(&[Value::Null]).unwrap(), Value::Null);
}

#[test]
fn log_numeric_scientific_power_of_ten() {
    let n = NumericValue::from_str("1.0e-1").unwrap();
    let r = eval_log(&[Value::Numeric(n)]).unwrap();
    let Value::Numeric(v) = r else {
        panic!("expected Numeric");
    };
    assert_eq!(v.to_string(), "-1.0000000000000000");
}

// -- ln -----------------------------------------------------------

#[test]
fn ln_e() {
    let r = eval_ln(&[Value::Double(std::f64::consts::E)]).unwrap();
    if let Value::Double(v) = r {
        assert!((v - 1.0).abs() < 1e-10);
    } else {
        panic!("expected Double");
    }
}

#[test]
fn ln_one() {
    let r = eval_ln(&[Value::Double(1.0)]).unwrap();
    assert_eq!(r, Value::Double(0.0));
}

#[test]
fn ln_zero_errors() {
    assert!(eval_ln(&[Value::Double(0.0)]).is_err());
}

#[test]
fn ln_negative_errors() {
    assert!(eval_ln(&[Value::Double(-1.0)]).is_err());
}

#[test]
fn ln_null() {
    assert_eq!(eval_ln(&[Value::Null]).unwrap(), Value::Null);
}

#[test]
fn ln_numeric_scientific_power_of_ten() {
    let n = NumericValue::from_str("1.0e-1").unwrap();
    let r = eval_ln(&[Value::Numeric(n)]).unwrap();
    let Value::Numeric(v) = r else {
        panic!("expected Numeric");
    };
    assert_eq!(v.to_string(), "-2.3025850929940457");
}

// -- exp ----------------------------------------------------------

#[test]
fn exp_zero() {
    let r = eval_exp(&[Value::Double(0.0)]).unwrap();
    assert_eq!(r, Value::Double(1.0));
}

#[test]
fn exp_one() {
    let r = eval_exp(&[Value::Double(1.0)]).unwrap();
    if let Value::Double(v) = r {
        assert!((v - std::f64::consts::E).abs() < 1e-10);
    } else {
        panic!("expected Double");
    }
}

#[test]
fn exp_null() {
    assert_eq!(eval_exp(&[Value::Null]).unwrap(), Value::Null);
}

// -- sign ---------------------------------------------------------

#[test]
fn sign_positive() {
    let r = eval_sign(&[Value::Double(42.0)]).unwrap();
    assert_eq!(r, Value::Double(1.0));
}

#[test]
fn sign_negative() {
    let r = eval_sign(&[Value::Double(-7.0)]).unwrap();
    assert_eq!(r, Value::Double(-1.0));
}

#[test]
fn sign_zero() {
    let r = eval_sign(&[Value::Double(0.0)]).unwrap();
    assert_eq!(r, Value::Double(0.0));
}

#[test]
fn sign_null() {
    assert_eq!(eval_sign(&[Value::Null]).unwrap(), Value::Null);
}

// -- pi -----------------------------------------------------------

#[test]
fn pi_value() {
    let r = eval_pi(&[]).unwrap();
    assert_eq!(r, Value::Double(std::f64::consts::PI));
}

#[test]
fn pi_wrong_args() {
    assert!(eval_pi(&[Value::Int(1)]).is_err());
}

// -- random -------------------------------------------------------

#[test]
fn random_in_range() {
    let r = eval_random(&[]).unwrap();
    if let Value::Double(v) = r {
        assert!((0.0..1.0).contains(&v), "random() = {v}, not in [0,1)");
    } else {
        panic!("expected Double");
    }
}

#[test]
fn random_wrong_args() {
    assert!(eval_random(&[Value::Int(1)]).is_err());
}

// -- greatest -----------------------------------------------------

#[test]
fn greatest_basic() {
    let r = eval_greatest(&[Value::Int(1), Value::Int(5), Value::Int(3)]).unwrap();
    assert_eq!(r, Value::Double(5.0));
}

#[test]
fn greatest_with_null() {
    let r = eval_greatest(&[Value::Null, Value::Int(2), Value::Null, Value::Int(7)]).unwrap();
    assert_eq!(r, Value::Double(7.0));
}

#[test]
fn greatest_all_null() {
    let r = eval_greatest(&[Value::Null, Value::Null]).unwrap();
    assert_eq!(r, Value::Null);
}

#[test]
fn greatest_single() {
    let r = eval_greatest(&[Value::Double(3.14)]).unwrap();
    assert_eq!(r, Value::Double(3.14));
}

// -- least --------------------------------------------------------

#[test]
fn least_basic() {
    let r = eval_least(&[Value::Int(4), Value::Int(1), Value::Int(9)]).unwrap();
    assert_eq!(r, Value::Double(1.0));
}

#[test]
fn least_with_null() {
    let r = eval_least(&[Value::Null, Value::Int(8), Value::Int(2), Value::Null]).unwrap();
    assert_eq!(r, Value::Double(2.0));
}

#[test]
fn least_all_null() {
    let r = eval_least(&[Value::Null, Value::Null]).unwrap();
    assert_eq!(r, Value::Null);
}

#[test]
fn least_single() {
    let r = eval_least(&[Value::Double(3.14)]).unwrap();
    assert_eq!(r, Value::Double(3.14));
}

#[test]
fn least_negative_values() {
    let r = eval_least(&[Value::Double(-1.0), Value::Double(-5.0), Value::Double(0.0)]).unwrap();
    assert_eq!(r, Value::Double(-5.0));
}

// -- log with two args --------------------------------------------

#[test]
fn log_two_args_base2() {
    let r = eval_log(&[Value::Double(2.0), Value::Double(8.0)]).unwrap();
    if let Value::Double(v) = r {
        assert!((v - 3.0).abs() < 1e-10);
    } else {
        panic!("expected Double");
    }
}

#[test]
fn log_two_args_base10() {
    let r = eval_log(&[Value::Double(10.0), Value::Double(1000.0)]).unwrap();
    if let Value::Double(v) = r {
        assert!((v - 3.0).abs() < 1e-10);
    } else {
        panic!("expected Double");
    }
}

#[test]
fn log_two_args_base1_errors() {
    assert!(eval_log(&[Value::Double(1.0), Value::Double(5.0)]).is_err());
}

#[test]
fn log_two_args_negative_base_errors() {
    assert!(eval_log(&[Value::Double(-2.0), Value::Double(8.0)]).is_err());
}

#[test]
fn log_two_args_null() {
    assert_eq!(
        eval_log(&[Value::Null, Value::Double(8.0)]).unwrap(),
        Value::Null
    );
}

// -- mod ----------------------------------------------------------

#[test]
fn mod_int_basic() {
    let r = eval_mod(&[Value::Int(10), Value::Int(3)]).unwrap();
    assert_eq!(r, Value::Int(1));
}

#[test]
fn mod_bigint_basic() {
    let r = eval_mod(&[Value::BigInt(17), Value::BigInt(5)]).unwrap();
    assert_eq!(r, Value::BigInt(2));
}

#[test]
fn mod_double_basic() {
    let r = eval_mod(&[Value::Double(10.5), Value::Double(3.0)]).unwrap();
    if let Value::Double(v) = r {
        assert!((v - 1.5).abs() < 1e-10);
    } else {
        panic!("expected Double");
    }
}

#[test]
fn mod_division_by_zero() {
    assert!(eval_mod(&[Value::Int(10), Value::Int(0)]).is_err());
}

#[test]
fn mod_null() {
    assert_eq!(
        eval_mod(&[Value::Null, Value::Int(3)]).unwrap(),
        Value::Null
    );
}

#[test]
fn num_nulls_variadic_counts_array_elements() {
    let result = eval_num_nulls_variadic(&[Value::Array(vec![
        Value::Int(1),
        Value::Null,
        Value::Int(2),
        Value::Null,
    ])])
    .unwrap();
    assert_eq!(result, Value::Int(2));
}

#[test]
fn num_nonnulls_variadic_counts_array_elements() {
    let result = eval_num_nonnulls_variadic(&[Value::Array(vec![
        Value::Int(1),
        Value::Null,
        Value::Int(2),
        Value::Int(3),
    ])])
    .unwrap();
    assert_eq!(result, Value::Int(3));
}

#[test]
fn num_nulls_variadic_null_array_returns_null() {
    let result = eval_num_nulls_variadic(&[Value::Null]).unwrap();
    assert_eq!(result, Value::Null);
}

#[test]
fn num_nonnulls_variadic_null_array_returns_null() {
    let result = eval_num_nonnulls_variadic(&[Value::Null]).unwrap();
    assert_eq!(result, Value::Null);
}
