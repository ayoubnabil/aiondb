use aiondb_core::{DbError, DbResult, ErrorReport, NumericValue, SqlState, Value};

use super::math::*;
use super::value_convert::{i64_to_f64, to_i32_saturating};
use super::{expect_at_least_args, to_f64};

// ── Shared helpers (were duplicated across div/gcd/lcm) ─────────────

/// Convert a `Value` to `NumericValue` (lenient: falls back via f64 parse).
pub(super) fn coerce_to_numeric(val: &Value) -> DbResult<NumericValue> {
    match val {
        Value::Numeric(n) => Ok(n.clone()),
        Value::Int(v) => Ok(NumericValue::from_i32(*v)),
        Value::BigInt(v) => Ok(NumericValue::from_i64(*v)),
        _ => {
            let f = to_f64(val)?;
            let s = format!("{f}");
            Ok(s.parse::<NumericValue>().unwrap_or(NumericValue::NAN))
        }
    }
}

/// Convert a `Value` to `NumericValue` for integer-domain functions (gcd/lcm).
/// Rejects values outside the i64 range when falling back through f64.
fn coerce_to_numeric_integer(val: &Value) -> DbResult<NumericValue> {
    match val {
        Value::Numeric(n) => Ok(n.clone()),
        Value::Int(v) => Ok(NumericValue::from_i32(*v)),
        Value::BigInt(v) => Ok(NumericValue::from_i64(*v)),
        _ => {
            let f = to_f64(val)?;
            if !f.is_finite() || f >= i64_to_f64(i64::MAX) || f < i64_to_f64(i64::MIN) {
                return Err(DbError::internal("value out of range for bigint"));
            }
            Ok(NumericValue::from_i64(super::value_convert::f64_to_i64(f)?))
        }
    }
}

/// Convert a `Value` to `i64` (integer-domain).
fn coerce_to_i64(val: &Value) -> DbResult<i64> {
    match val {
        Value::Int(v) => Ok(i64::from(*v)),
        Value::BigInt(v) => Ok(*v),
        other => super::value_convert::f64_to_i64(to_f64(other)?),
    }
}

fn numeric_overflow() -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::NumericValueOutOfRange,
        "numeric value overflows",
    ))
}

/// Scale a `NumericValue` coefficient up to `target_scale`, with overflow checks.
fn scale_coefficient(num: &NumericValue, target_scale: u32) -> DbResult<i128> {
    if num.scale < target_scale {
        let diff = target_scale - num.scale;
        let factor = 10i128.checked_pow(diff).ok_or_else(numeric_overflow)?;
        num.coefficient
            .checked_mul(factor)
            .ok_or_else(numeric_overflow)
    } else {
        Ok(num.coefficient)
    }
}

/// Euclidean GCD on i128 (absolute values).
fn gcd_i128(mut a: i128, mut b: i128) -> i128 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// Check whether any of the first two args is a Numeric special value
/// (NaN or Infinity). Returns `Some(NAN)` if so.
fn check_numeric_special_pair(args: &[Value]) -> Option<Value> {
    for arg in &args[..2] {
        if let Value::Numeric(n) = arg {
            if n.is_nan() || n.is_infinite() {
                return Some(Value::Numeric(NumericValue::NAN));
            }
        }
    }
    None
}

fn any_is_numeric(args: &[Value]) -> bool {
    matches!(&args[0], Value::Numeric(_)) || matches!(&args[1], Value::Numeric(_))
}

pub(crate) fn eval_math_generic(name: &str, args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    match name {
        "justify_hours" => match &args[0] {
            Value::Interval(iv) => Ok(Value::Interval(justify_hours_interval(iv)?)),
            _ => Err(DbError::internal("expected interval argument")),
        },
        "justify_days" => match &args[0] {
            Value::Interval(iv) => Ok(Value::Interval(justify_days_interval(iv)?)),
            _ => Err(DbError::internal("expected interval argument")),
        },
        "justify_interval" => match &args[0] {
            Value::Interval(iv) => Ok(Value::Interval(justify_interval_value(iv)?)),
            _ => Err(DbError::internal("expected interval argument")),
        },
        "scale" => match &args[0] {
            Value::Numeric(n) => {
                if n.is_special() {
                    Ok(Value::Null)
                } else {
                    Ok(Value::Int(to_i32_saturating(n.scale)))
                }
            }
            Value::Int(_) | Value::BigInt(_) => Ok(Value::Int(0)),
            Value::Double(_) | Value::Real(_) => {
                let s = to_f64(&args[0])?.to_string();
                let scale = s.find('.').map_or(0, |dot| s.len() - dot - 1);
                Ok(Value::Int(to_i32_saturating(scale)))
            }
            _ => Ok(Value::Int(0)),
        },
        "div" => {
            expect_at_least_args(args, 2, "div()")?;
            if any_is_numeric(args) {
                let a_num = coerce_to_numeric(&args[0])?;
                let b_num = coerce_to_numeric(&args[1])?;
                if a_num.is_nan() || b_num.is_nan() {
                    return Ok(Value::Numeric(NumericValue::NAN));
                }
                if a_num.is_infinite() {
                    if b_num.is_infinite() || b_num.is_zero() {
                        return Err(DbError::internal("division by zero"));
                    }
                    return Err(DbError::from_report(ErrorReport::new(
                        SqlState::NumericValueOutOfRange,
                        "value overflows numeric format",
                    )));
                }
                if b_num.is_zero() {
                    return Err(DbError::internal("division by zero"));
                }
                if b_num.is_infinite() {
                    return Ok(Value::Numeric(NumericValue::new(0, 0)));
                }
                return match a_num.div_trunc_int(&b_num) {
                    Some(quotient) => Ok(Value::Numeric(quotient)),
                    None => Err(DbError::internal("division by zero")),
                };
            }
            let a = to_f64(&args[0])?;
            let b = to_f64(&args[1])?;
            if b == 0.0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Double((a / b).trunc()))
        }
        "gcd" => {
            expect_at_least_args(args, 2, "gcd()")?;
            if any_is_numeric(args) {
                if let Some(nan) = check_numeric_special_pair(args) {
                    return Ok(nan);
                }
                let a_num = coerce_to_numeric_integer(&args[0])?;
                let b_num = coerce_to_numeric_integer(&args[1])?;
                let max_scale = a_num.scale.max(b_num.scale);
                let a_coeff = scale_coefficient(&a_num, max_scale)?;
                let b_coeff = scale_coefficient(&b_num, max_scale)?;
                let result = gcd_i128(a_coeff, b_coeff);
                return Ok(Value::Numeric(NumericValue::new(result, max_scale)));
            }
            let mut a = coerce_to_i64(&args[0])?;
            let mut b = coerce_to_i64(&args[1])?;
            // Euclidean algorithm with wrapping_rem to handle i64::MIN safely.
            // Matches PostgreSQL's int8gcd behaviour.
            while b != 0 {
                let t = b;
                b = a.wrapping_rem(b);
                a = t;
            }
            let result = a.checked_abs().ok_or_else(|| {
                DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    "bigint out of range",
                ))
            })?;
            Ok(Value::BigInt(result))
        }
        "lcm" => {
            expect_at_least_args(args, 2, "lcm()")?;
            if any_is_numeric(args) {
                if let Some(nan) = check_numeric_special_pair(args) {
                    return Ok(nan);
                }
                let a_num = coerce_to_numeric_integer(&args[0])?;
                let b_num = coerce_to_numeric_integer(&args[1])?;
                if a_num.is_zero() || b_num.is_zero() {
                    return Ok(Value::Numeric(NumericValue::new(0, 0)));
                }
                let max_scale = a_num.scale.max(b_num.scale);
                let a_coeff = scale_coefficient(&a_num, max_scale)?;
                let b_coeff = scale_coefficient(&b_num, max_scale)?;
                let a_abs = a_coeff.abs();
                let b_abs = b_coeff.abs();
                let g = gcd_i128(a_abs, b_abs);
                let lcm_coeff = (a_abs / g)
                    .checked_mul(b_abs)
                    .ok_or_else(numeric_overflow)?;
                return Ok(Value::Numeric(NumericValue::new(lcm_coeff, max_scale)));
            }
            let a_val = coerce_to_i64(&args[0])?;
            let b_val = coerce_to_i64(&args[1])?;
            if a_val == 0 || b_val == 0 {
                return Ok(Value::BigInt(0));
            }
            let bigint_out_of_range = || {
                DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    "bigint out of range",
                ))
            };
            let a_abs = a_val.checked_abs().ok_or_else(bigint_out_of_range)?;
            let b_abs = b_val.checked_abs().ok_or_else(bigint_out_of_range)?;
            let mut ga = a_abs;
            let mut gb = b_abs;
            while gb != 0 {
                let t = gb;
                gb = ga % gb;
                ga = t;
            }
            let lcm = (a_abs / ga)
                .checked_mul(b_abs)
                .ok_or_else(bigint_out_of_range)?;
            Ok(Value::BigInt(lcm))
        }
        "factorial" => {
            let n = coerce_to_i64(&args[0])?;
            if n < 0 {
                return Err(DbError::internal(
                    "factorial of a negative number is undefined",
                ));
            }
            let mut result: i64 = 1;
            for i in 2..=n {
                result = result
                    .checked_mul(i)
                    .ok_or_else(|| DbError::internal("value overflows numeric format"))?;
            }
            Ok(Value::BigInt(result))
        }
        "min_scale" => match &args[0] {
            Value::Numeric(n) => {
                if n.is_special() {
                    // PostgreSQL returns NULL for min_scale(NaN) and min_scale(+/-Infinity)
                    return Ok(Value::Null);
                }
                if n.coefficient == 0 {
                    return Ok(Value::Int(0));
                }
                let mut coeff = n.coefficient.abs();
                let mut trimmed = 0u32;
                while coeff % 10 == 0 && trimmed < n.scale {
                    coeff /= 10;
                    trimmed += 1;
                }
                Ok(Value::Int(to_i32_saturating(n.scale - trimmed)))
            }
            _ => Ok(Value::Int(0)),
        },
        "trim_scale" => match &args[0] {
            Value::Numeric(n) => {
                if n.is_special() {
                    // PostgreSQL returns NaN/Infinity as-is for trim_scale
                    return Ok(Value::Numeric(n.clone()));
                }
                if n.coefficient == 0 {
                    return Ok(Value::Numeric(aiondb_core::NumericValue::new(0, 0)));
                }
                let mut coeff = n.coefficient;
                let mut scale = n.scale;
                while coeff % 10 == 0 && scale > 0 {
                    coeff /= 10;
                    scale -= 1;
                }
                Ok(Value::Numeric(aiondb_core::NumericValue::new(coeff, scale)))
            }
            other => Ok(other.clone()),
        },
        "setseed" => Ok(Value::Text(String::new())),
        "numeric_inc" => {
            // numeric_inc(x) returns x + 1 (PostgreSQL internal function)
            let one = aiondb_core::NumericValue::new(1, 0);
            match &args[0] {
                Value::Numeric(n) => {
                    if n.is_nan() {
                        return Ok(Value::Numeric(aiondb_core::NumericValue::NAN));
                    }
                    if n.is_infinite() {
                        return Ok(Value::Numeric(n.clone()));
                    }
                    // Scale the 1 to match, then add
                    let one_scaled = aiondb_core::NumericValue::new(
                        10i128.checked_pow(n.scale).unwrap_or(1),
                        n.scale,
                    );
                    Ok(Value::Numeric(n.add(&one_scaled)))
                }
                Value::Int(v) => Ok(Value::Numeric(
                    aiondb_core::NumericValue::from_i32(*v).add(&one),
                )),
                Value::BigInt(v) => Ok(Value::Numeric(
                    aiondb_core::NumericValue::from_i64(*v).add(&one),
                )),
                Value::Double(v) => {
                    let nv = format!("{v}")
                        .parse::<aiondb_core::NumericValue>()
                        .unwrap_or(aiondb_core::NumericValue::NAN);
                    let one_scaled = aiondb_core::NumericValue::new(
                        10i128.checked_pow(nv.scale).unwrap_or(1),
                        nv.scale,
                    );
                    Ok(Value::Numeric(nv.add(&one_scaled)))
                }
                _ => {
                    let f = to_f64(&args[0])?;
                    Ok(Value::Numeric(aiondb_core::NumericValue::from_i64(
                        super::value_convert::f64_to_i64(f + 1.0)?,
                    )))
                }
            }
        }
        _ => Err(DbError::internal(format!("unknown math function: {name}"))),
    }
}
