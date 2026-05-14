use aiondb_core::{DbError, DbResult, ErrorReport, NumericValue, SqlState, Value};
use std::cmp::Ordering;
use std::str::FromStr;

use super::ext::coerce_to_array_with_bounds;
use super::math::*;
use super::math_generic::coerce_to_numeric;
use super::value_convert::{f64_to_i64, i64_to_f64, u64_to_f64};
use super::{expect_arg_range, expect_args, to_f64};
use crate::eval::operators::compare_runtime_values;

/// Extract (is_numeric, input_scale) from a value - shared by sqrt/ln/log/exp.
fn numeric_info(val: &Value) -> (bool, u32) {
    match val {
        Value::Numeric(n) => (true, n.scale),
        _ => (false, 0),
    }
}

fn numeric_mantissa_and_exp10(n: &NumericValue) -> DbResult<(f64, i64)> {
    let digits = n.coefficient_abs_string();
    if digits.is_empty() || digits == "0" {
        return Err(DbError::internal("cannot take logarithm of zero"));
    }
    let total_digits = i64::try_from(digits.len()).unwrap_or(i64::MAX);
    let exp10 = total_digits.saturating_sub(1) - i64::from(n.scale);

    // Parse enough leading digits so f64 parser rounds to nearest representable value.
    let sig = digits.len().min(64);
    let mut mantissa_text = String::with_capacity(sig + 1);
    mantissa_text.push_str(&digits[..1]);
    if sig > 1 {
        mantissa_text.push('.');
        mantissa_text.push_str(&digits[1..sig]);
    }
    let mantissa = mantissa_text
        .parse::<f64>()
        .map_err(|_| DbError::internal("invalid numeric mantissa"))?;
    if !mantissa.is_finite() || mantissa <= 0.0 {
        return Err(DbError::internal("invalid numeric mantissa"));
    }
    Ok((mantissa, exp10))
}

fn numeric_ln_approx(n: &NumericValue) -> DbResult<f64> {
    if n.is_zero() {
        return Err(DbError::internal("cannot take logarithm of zero"));
    }
    if n.is_negative() {
        return Err(DbError::internal(
            "cannot take logarithm of a negative number",
        ));
    }
    if n.is_nan() {
        return Ok(f64::NAN);
    }
    if n.is_pos_infinity() {
        return Ok(f64::INFINITY);
    }
    let (mantissa, exp10) = numeric_mantissa_and_exp10(n)?;
    if exp10 == 0 {
        let d = mantissa - 1.0;
        if d.abs() <= 0.25 {
            return Ok(d.ln_1p());
        }
    }
    Ok(mantissa.ln() + i64_to_f64(exp10) * std::f64::consts::LN_10)
}

fn numeric_log10_approx(n: &NumericValue) -> DbResult<f64> {
    if n.is_zero() {
        return Err(DbError::internal("cannot take logarithm of zero"));
    }
    if n.is_negative() {
        return Err(DbError::internal(
            "cannot take logarithm of a negative number",
        ));
    }
    if n.is_nan() {
        return Ok(f64::NAN);
    }
    if n.is_pos_infinity() {
        return Ok(f64::INFINITY);
    }
    if let Some(exp10) = numeric_exact_power_of_ten_exponent(n) {
        return Ok(i64_to_f64(exp10));
    }
    let (mantissa, exp10) = numeric_mantissa_and_exp10(n)?;
    if exp10 == 0 {
        let d = mantissa - 1.0;
        if d.abs() <= 0.25 {
            return Ok(d.ln_1p() / std::f64::consts::LN_10);
        }
    }
    Ok(mantissa.log10() + i64_to_f64(exp10))
}

fn numeric_exact_power_of_ten_exponent(n: &NumericValue) -> Option<i64> {
    if n.is_special() || n.is_zero() || n.is_negative() {
        return None;
    }
    let digits = n.coefficient_abs_string();
    let mut chars = digits.chars();
    if chars.next()? != '1' {
        return None;
    }
    if !chars.all(|c| c == '0') {
        return None;
    }
    let len = i64::try_from(digits.len()).ok()?;
    Some(len.saturating_sub(1) - i64::from(n.scale))
}

fn numeric_ln10_constant() -> DbResult<NumericValue> {
    // Enough precision for pg_regress numeric_big ln/log checks.
    NumericValue::from_str(
        "2.302585092994045684017991454684364207601101488628772976033327900967572609677352",
    )
    .map_err(|e| DbError::internal(format!("invalid ln(10) constant: {e}")))
}

fn numeric_mul_i64(value: &NumericValue, n: i64) -> DbResult<NumericValue> {
    let rhs = NumericValue::new(i128::from(n), 0);
    value
        .mul(&rhs)
        .ok_or_else(|| DbError::internal("value overflows numeric format"))
}

fn numeric_ln_power_of_ten(exp10: i64, scale: u32) -> DbResult<NumericValue> {
    let ln10 = numeric_ln10_constant()?;
    let scaled = numeric_mul_i64(&ln10, exp10)?;
    Ok(scaled.round(scale))
}

fn numeric_overflow_error() -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::NumericValueOutOfRange,
        "value overflows numeric format",
    ))
}

fn numeric_parse_const(value: &str) -> DbResult<NumericValue> {
    NumericValue::from_str(value)
        .map_err(|e| DbError::internal(format!("invalid numeric constant '{value}': {e}")))
}

fn numeric_work_scale(target_scale: u32) -> u32 {
    target_scale.saturating_add(12).max(48).min(140)
}

pub(crate) fn numeric_can_use_precise(n: &NumericValue) -> bool {
    !n.is_special() && n.scale <= 120 && n.coefficient_abs_string().len() <= 256
}

fn numeric_mul_round(a: &NumericValue, b: &NumericValue, scale: u32) -> DbResult<NumericValue> {
    a.mul(b)
        .map(|value| value.round(scale))
        .ok_or_else(numeric_overflow_error)
}

fn numeric_div_round(a: &NumericValue, b: &NumericValue, scale: u32) -> DbResult<NumericValue> {
    if b.is_zero() {
        return Err(DbError::internal("division by zero"));
    }
    a.div_with_scale(b, scale.saturating_add(6))
        .map(|value| value.round(scale))
        .ok_or_else(numeric_overflow_error)
}

fn numeric_div_i64_round(value: &NumericValue, n: i64, scale: u32) -> DbResult<NumericValue> {
    let divisor = NumericValue::new(i128::from(n), 0);
    numeric_div_round(value, &divisor, scale)
}

fn numeric_abs_leq(value: &NumericValue, epsilon: &NumericValue) -> bool {
    value.abs().cmp(epsilon) != Ordering::Greater
}

fn numeric_mantissa_decimal_and_exp10(
    n: &NumericValue,
    sig_digits_limit: usize,
) -> DbResult<(NumericValue, i64)> {
    let digits = n.coefficient_abs_string();
    if digits.is_empty() || digits == "0" {
        return Err(DbError::internal("cannot take logarithm of zero"));
    }
    let total_digits = i64::try_from(digits.len()).unwrap_or(i64::MAX);
    let exp10 = total_digits.saturating_sub(1) - i64::from(n.scale);
    let sig = digits.len().min(sig_digits_limit.max(1));
    let mantissa_digits = &digits[..sig];
    let mantissa_scale = u32::try_from(sig.saturating_sub(1)).unwrap_or(u32::MAX);
    let coefficient = if n.is_negative() {
        format!("-{mantissa_digits}")
    } else {
        mantissa_digits.to_owned()
    };
    let mantissa = NumericValue::from_coefficient_string(&coefficient, mantissa_scale)
        .map_err(|e| DbError::internal(format!("invalid numeric mantissa: {e}")))?;
    Ok((mantissa, exp10))
}

fn numeric_ln_mantissa_series(mantissa: &NumericValue, work_scale: u32) -> DbResult<NumericValue> {
    let one = NumericValue::new(1, 0);
    let numerator = mantissa.sub(&one);
    let denominator = mantissa.add(&one);
    let y = numeric_div_round(&numerator, &denominator, work_scale.saturating_add(10))
        .map_err(|e| DbError::internal(format!("ln series initial divide failed: {e}")))?;
    let y2 = numeric_mul_round(&y, &y, work_scale.saturating_add(12))
        .map_err(|e| DbError::internal(format!("ln series initial square failed: {e}")))?;
    let epsilon = NumericValue::new(1, work_scale.saturating_add(4));

    let mut sum = y.clone();
    let mut term = y;
    let mut converged = false;
    for odd in (3_i64..=20_001_i64).step_by(2) {
        term = numeric_mul_round(&term, &y2, work_scale.saturating_add(14)).map_err(|e| {
            DbError::internal(format!("ln series term multiply failed at odd={odd}: {e}"))
        })?;
        let add =
            numeric_div_i64_round(&term, odd, work_scale.saturating_add(10)).map_err(|e| {
                DbError::internal(format!("ln series term divide failed at odd={odd}: {e}"))
            })?;
        if numeric_abs_leq(&add, &epsilon) {
            converged = true;
            break;
        }
        sum = sum.add(&add).round(work_scale.saturating_add(10));
    }
    if !converged {
        return Err(DbError::internal("ln() series failed to converge"));
    }
    numeric_mul_i64(&sum, 2).map(|value| value.round(work_scale.saturating_add(10)))
}

fn numeric_mul_pow10(value: &NumericValue, exp10: i64) -> DbResult<NumericValue> {
    if value.is_special() || value.is_zero() || exp10 == 0 {
        return Ok(value.clone());
    }
    let coefficient = value.coefficient_to_string();
    if exp10 > 0 {
        let shift = u32::try_from(exp10).unwrap_or(u32::MAX);
        if value.scale >= shift {
            return NumericValue::from_coefficient_string(&coefficient, value.scale - shift)
                .map_err(|e| DbError::internal(format!("numeric pow10 scaling failed: {e}")));
        }
        let append = usize::try_from(shift - value.scale).unwrap_or(usize::MAX);
        let mut scaled = coefficient;
        for _ in 0..append {
            scaled.push('0');
        }
        NumericValue::from_coefficient_string(&scaled, 0)
            .map_err(|e| DbError::internal(format!("numeric pow10 scaling failed: {e}")))
    } else {
        let add = u32::try_from(exp10.saturating_neg()).unwrap_or(u32::MAX);
        let new_scale = value
            .scale
            .checked_add(add)
            .ok_or_else(numeric_overflow_error)?;
        NumericValue::from_coefficient_string(&coefficient, new_scale)
            .map_err(|e| DbError::internal(format!("numeric pow10 scaling failed: {e}")))
    }
}

fn numeric_promote_to_big(value: &NumericValue) -> DbResult<NumericValue> {
    if value.is_big() || value.is_special() {
        return Ok(value.clone());
    }
    let mut coefficient = value.coefficient_to_string();
    for _ in 0..40 {
        coefficient.push('0');
    }
    let promoted_scale = value
        .scale
        .checked_add(40)
        .ok_or_else(numeric_overflow_error)?;
    NumericValue::from_coefficient_string(&coefficient, promoted_scale)
        .map_err(|e| DbError::internal(format!("numeric promotion failed: {e}")))
}

fn numeric_ln_precise_at_scale(n: &NumericValue, scale: u32) -> DbResult<NumericValue> {
    if n.is_zero() {
        return Err(DbError::internal("cannot take logarithm of zero"));
    }
    if n.is_negative() {
        return Err(DbError::internal(
            "cannot take logarithm of a negative number",
        ));
    }
    if n.is_nan() {
        return Ok(NumericValue::NAN);
    }
    if n.is_pos_infinity() {
        return Ok(NumericValue::INFINITY);
    }
    if let Some(exp10) = numeric_exact_power_of_ten_exponent(n) {
        return numeric_ln_power_of_ten(exp10, scale);
    }

    let work_scale = numeric_work_scale(scale);
    let sig_limit = usize::try_from(work_scale.saturating_add(16)).unwrap_or(usize::MAX);
    let (mantissa, exp10) = numeric_mantissa_decimal_and_exp10(n, sig_limit)
        .map_err(|e| DbError::internal(format!("ln() mantissa extraction failed: {e}")))?;
    let ln_mantissa = numeric_ln_mantissa_series(&mantissa, work_scale)
        .map_err(|e| DbError::internal(format!("ln() mantissa series failed: {e}")))?;
    let ln10 = numeric_ln10_constant()
        .map_err(|e| DbError::internal(format!("ln() ln10 constant failed: {e}")))?
        .round(work_scale.saturating_add(10));
    let exp_term = numeric_mul_i64(&ln10, exp10)
        .map_err(|e| DbError::internal(format!("ln() exponent scaling failed: {e}")))?;
    Ok(ln_mantissa.add(&exp_term).round(scale))
}

pub(crate) fn numeric_ln_precise_scaled(n: &NumericValue, scale: u32) -> DbResult<NumericValue> {
    numeric_ln_precise_at_scale(n, scale)
}

pub(crate) fn numeric_log10_precise_scaled(n: &NumericValue, scale: u32) -> DbResult<NumericValue> {
    if let Some(exp10) = numeric_exact_power_of_ten_exponent(n) {
        let coefficient = i128::from(exp10);
        return Ok(NumericValue::new(coefficient, 0).round(scale));
    }
    let work_scale = numeric_work_scale(scale);
    let ln_value = numeric_ln_precise_at_scale(n, work_scale.saturating_add(12))?;
    let ln10 = numeric_ln10_constant()?.round(work_scale.saturating_add(12));
    let value = numeric_div_round(&ln_value, &ln10, work_scale.saturating_add(8))?;
    Ok(value.round(scale))
}

fn numeric_exp_series_small(x: &NumericValue, work_scale: u32) -> DbResult<NumericValue> {
    let one = NumericValue::new(1, 0);
    let epsilon = NumericValue::new(1, work_scale.saturating_add(4));
    let mut sum = one.clone();
    let mut term = one;
    let mut converged = false;
    for n in 1_i64..=20_000_i64 {
        term = numeric_mul_round(&term, x, work_scale.saturating_add(14))?;
        term = numeric_div_i64_round(&term, n, work_scale.saturating_add(12))?;
        if numeric_abs_leq(&term, &epsilon) {
            converged = true;
            break;
        }
        sum = sum.add(&term).round(work_scale.saturating_add(12));
    }
    if !converged {
        return Err(DbError::internal("exp() series failed to converge"));
    }
    Ok(sum.round(work_scale.saturating_add(12)))
}

pub(crate) fn numeric_exp_precise_scaled(x: &NumericValue, scale: u32) -> DbResult<NumericValue> {
    if x.is_nan() {
        return Ok(NumericValue::NAN);
    }
    if x.is_pos_infinity() {
        return Ok(NumericValue::INFINITY);
    }
    if x.is_neg_infinity() {
        return Ok(NumericValue::new(0, 0));
    }
    if x.is_zero() {
        return Ok(NumericValue::new(1, 0).round(scale));
    }

    let work_scale = numeric_work_scale(scale);
    if x.is_negative() {
        let positive = numeric_exp_precise_scaled(&x.neg(), work_scale.saturating_add(8))?;
        let one = NumericValue::new(1, 0);
        let reciprocal = numeric_div_round(&one, &positive, work_scale.saturating_add(8))?;
        return Ok(reciprocal.round(scale));
    }

    let ln10 = numeric_ln10_constant()?.round(work_scale.saturating_add(12));
    let estimate = x.to_f64() / std::f64::consts::LN_10;
    if !estimate.is_finite() {
        return Err(numeric_overflow_error());
    }
    let mut k = f64_to_i64(estimate.floor()).map_err(|_| numeric_overflow_error())?;
    let mut residual = x
        .sub(&numeric_mul_i64(&ln10, k)?)
        .round(work_scale.saturating_add(12));
    while residual.cmp(&NumericValue::new(0, 0)) == Ordering::Less {
        k = k.saturating_sub(1);
        residual = residual.add(&ln10).round(work_scale.saturating_add(12));
    }
    while residual.cmp(&ln10) != Ordering::Less {
        k = k.saturating_add(1);
        residual = residual.sub(&ln10).round(work_scale.saturating_add(12));
    }

    let threshold = numeric_parse_const("0.1")?;
    let half = numeric_parse_const("0.5")?;
    let mut reduced = residual;
    let mut square_count = 0u32;
    while reduced.abs().cmp(&threshold) == Ordering::Greater {
        reduced = numeric_mul_round(&reduced, &half, work_scale.saturating_add(12))?;
        square_count = square_count.saturating_add(1);
        if square_count > 64 {
            return Err(DbError::internal("exp() argument reduction exceeded limit"));
        }
    }

    let mut result = numeric_exp_series_small(&reduced, work_scale)?;
    for _ in 0..square_count {
        result = numeric_mul_round(&result, &result, work_scale.saturating_add(12))?;
    }
    let shifted = numeric_mul_pow10(&result, k)?;
    Ok(shifted.round(scale))
}

pub(crate) fn numeric_as_i64_if_integer(n: &NumericValue) -> Option<i64> {
    if n.is_special() {
        return None;
    }
    let int_val = n.trunc(0);
    if int_val.cmp(n) != Ordering::Equal {
        return None;
    }
    int_val
        .try_coefficient_i128()
        .and_then(|value| i64::try_from(value).ok())
}

fn numeric_pow_integer_exact(base: &NumericValue, exponent: u64) -> DbResult<NumericValue> {
    let mut result = NumericValue::new(1, 0);
    // Force big-coefficient arithmetic up front so multiplication does not
    // fail early on i128 overflow for otherwise representable results.
    let mut factor = numeric_promote_to_big(base)?;
    let mut e = exponent;
    while e > 0 {
        if e & 1 == 1 {
            result = result.mul(&factor).ok_or_else(numeric_overflow_error)?;
        }
        e >>= 1;
        if e > 0 {
            factor = factor.mul(&factor).ok_or_else(numeric_overflow_error)?;
        }
    }
    Ok(result)
}

pub(crate) fn numeric_power_result_scale(base_scale: u32, exp_scale: u32, approx: f64) -> u32 {
    let floor_scale = base_scale.max(exp_scale);
    if !approx.is_finite() || approx == 0.0 {
        return floor_scale.max(16).min(1000);
    }
    let log_floor = approx.abs().log10().floor();
    if !log_floor.is_finite() {
        return floor_scale.max(16).min(1000);
    }
    let d = f64_to_i64(log_floor).unwrap_or(i64::MAX);
    let needed = if approx.abs() >= 1.0 {
        // Match PostgreSQL display shape for power(): one more significant digit
        // on values >= 1, but never below the input dscale floor.
        (16_i64 - d).max(0)
    } else {
        // For values < 1, keep 16 significant digits after leading zeros.
        (15_i64 - d).max(0)
    };
    floor_scale
        .max(u32::try_from(needed).unwrap_or(u32::MAX))
        .min(1000)
}

pub(crate) fn numeric_pow_precise_scaled(
    base: &NumericValue,
    exp: &NumericValue,
    out_scale: u32,
) -> DbResult<NumericValue> {
    if let Some(exp_int) = numeric_as_i64_if_integer(exp) {
        if exp_int == 0 {
            return Ok(NumericValue::new(1, 0).round(out_scale));
        }
        if exp_int > 0 {
            let value =
                numeric_pow_integer_exact(base, u64::try_from(exp_int).unwrap_or(u64::MAX))?;
            return Ok(value.round(out_scale));
        }
        let abs_exp = exp_int.unsigned_abs();
        let positive = numeric_pow_integer_exact(base, abs_exp)?;
        let one = NumericValue::new(1, 0);
        let work_scale = numeric_work_scale(out_scale).saturating_add(8);
        let reciprocal = numeric_div_round(&one, &positive, work_scale)?;
        return Ok(reciprocal.round(out_scale));
    }

    let work_scale = numeric_work_scale(out_scale).saturating_add(16);
    let ln_base = numeric_ln_precise_at_scale(base, work_scale)?;
    let exp_times_ln = numeric_mul_round(exp, &ln_base, work_scale)?;
    let value = numeric_exp_precise_scaled(&exp_times_ln, work_scale)?;
    Ok(value.round(out_scale))
}

pub(crate) fn eval_sqrt(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "sqrt")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let (is_numeric, input_scale) = numeric_info(&args[0]);
    // Handle Numeric special values
    if let Value::Numeric(n) = &args[0] {
        if n.is_nan() {
            return Ok(Value::Numeric(aiondb_core::NumericValue::NAN));
        }
        if n.is_pos_infinity() {
            return Ok(Value::Numeric(aiondb_core::NumericValue::INFINITY));
        }
        if n.is_neg_infinity() {
            return Err(DbError::internal(
                "cannot take square root of a negative number",
            ));
        }
    }
    let f = to_f64(&args[0])?;
    if f < 0.0 {
        return Err(DbError::internal(
            "cannot take square root of a negative number",
        ));
    }
    if is_numeric {
        let result = f.sqrt();
        let scale = pg_sqrt_result_scale(input_scale, result);
        return crate::eval::cast::numeric::float_to_numeric_with_scale(result, Some(scale));
    }
    Ok(Value::Double(f.sqrt()))
}

pub(crate) fn eval_cbrt(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "cbrt")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let f = to_f64(&args[0])?;
    Ok(Value::Double(f.cbrt()))
}

pub(crate) fn eval_log(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 1, 2, "log() requires 1 or 2 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let is_numeric = args.iter().any(|a| matches!(a, Value::Numeric(_)));
    if args.len() == 2 {
        // log(base, x) - two-argument form
        // Handle Numeric special values
        for arg in &args[..2] {
            if let Value::Numeric(n) = arg {
                if n.is_nan() {
                    return Ok(Value::Numeric(aiondb_core::NumericValue::NAN));
                }
            }
        }
        // PostgreSQL checks base first, then argument.
        let base = match &args[0] {
            Value::Numeric(base_n) => {
                if base_n.is_zero() {
                    return Err(DbError::internal("cannot take logarithm of zero"));
                }
                if base_n.is_negative() {
                    return Err(DbError::internal(
                        "cannot take logarithm of a negative number",
                    ));
                }
                let one = NumericValue::new(1, 0);
                if base_n.cmp(&one) == std::cmp::Ordering::Equal {
                    return Err(DbError::internal("division by zero"));
                }
                None
            }
            _ => {
                let base = to_f64(&args[0])?;
                if base == 0.0 {
                    return Err(DbError::internal("cannot take logarithm of zero"));
                }
                if base < 0.0 {
                    return Err(DbError::internal(
                        "cannot take logarithm of a negative number",
                    ));
                }
                if base == 1.0 {
                    return Err(DbError::internal("division by zero"));
                }
                Some(base)
            }
        };
        let x = match &args[1] {
            Value::Numeric(x_n) => {
                if x_n.is_zero() {
                    return Err(DbError::internal("cannot take logarithm of zero"));
                }
                if x_n.is_negative() {
                    return Err(DbError::internal(
                        "cannot take logarithm of a negative number",
                    ));
                }
                None
            }
            _ => {
                let x = to_f64(&args[1])?;
                if x == 0.0 {
                    return Err(DbError::internal("cannot take logarithm of zero"));
                }
                if x < 0.0 {
                    return Err(DbError::internal(
                        "cannot take logarithm of a negative number",
                    ));
                }
                Some(x)
            }
        };
        // Handle infinity inputs
        if let Value::Numeric(n) = &args[1] {
            if n.is_pos_infinity() {
                return Ok(Value::Numeric(aiondb_core::NumericValue::INFINITY));
            }
        }
        if is_numeric {
            let base_num = coerce_to_numeric(&args[0])?;
            let x_num = coerce_to_numeric(&args[1])?;
            let rough_base_ln = numeric_ln_approx(&base_num)?;
            let rough_x_ln = numeric_ln_approx(&x_num)?;
            // Reaching here with `rough_base_ln == 0.0` only means the
            // *approximation* underflowed (e.g. base ≈ 1.0 to f64
            // precision). Don't reject yet - fall through to the
            // precise path which checks the real `base_ln.is_zero()`
            // below. Use `1.0` as a sentinel rough divisor so the
            // approximate scale estimate stays usable.
            let rough = if rough_base_ln == 0.0 {
                rough_x_ln
            } else {
                rough_x_ln / rough_base_ln
            };
            let input_scale = base_num.scale.max(x_num.scale);
            let scale = pg_ln_result_scale(input_scale, rough);
            if !numeric_can_use_precise(&base_num) || !numeric_can_use_precise(&x_num) {
                return crate::eval::cast::numeric::float_to_numeric_with_scale(rough, Some(scale));
            }
            let base_ln = numeric_ln_precise_scaled(&base_num, scale.saturating_add(20))?;
            if base_ln.is_zero() {
                return Err(DbError::internal("division by zero"));
            }
            let x_ln = numeric_ln_precise_scaled(&x_num, scale.saturating_add(20))?;
            let numeric_result =
                numeric_div_round(&x_ln, &base_ln, scale.saturating_add(8))?.round(scale);
            return Ok(Value::Numeric(numeric_result));
        }
        Ok(Value::Double(x.unwrap_or(0.0).log(base.unwrap_or(0.0))))
    } else {
        // log(x) = log10(x)
        // Handle Numeric special values
        if let Value::Numeric(n) = &args[0] {
            if n.is_nan() {
                return Ok(Value::Numeric(aiondb_core::NumericValue::NAN));
            }
            if n.is_pos_infinity() {
                return Ok(Value::Numeric(aiondb_core::NumericValue::INFINITY));
            }
        }
        let (_, input_scale) = numeric_info(&args[0]);
        let result = if let Value::Numeric(n) = &args[0] {
            if n.is_zero() {
                return Err(DbError::internal("cannot take logarithm of zero"));
            }
            if n.is_negative() {
                return Err(DbError::internal(
                    "cannot take logarithm of a negative number",
                ));
            }
            let rough = numeric_log10_approx(n)?;
            let scale = pg_ln_result_scale(input_scale, rough);
            if !numeric_can_use_precise(n) {
                return crate::eval::cast::numeric::float_to_numeric_with_scale(rough, Some(scale));
            }
            return Ok(Value::Numeric(numeric_log10_precise_scaled(n, scale)?));
        } else {
            let f = to_f64(&args[0])?;
            if f == 0.0 {
                return Err(DbError::internal("cannot take logarithm of zero"));
            }
            if f < 0.0 {
                return Err(DbError::internal(
                    "cannot take logarithm of a negative number",
                ));
            }
            f.log10()
        };
        if is_numeric {
            let scale = pg_ln_result_scale(input_scale, result);
            return crate::eval::cast::numeric::float_to_numeric_with_scale(result, Some(scale));
        }
        Ok(Value::Double(result))
    }
}

pub(crate) fn eval_mod(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "mod")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    match (&args[0], &args[1]) {
        (Value::Int(a), Value::Int(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            // i32::MIN % -1 panics in Rust; mathematically the result is 0.
            Ok(Value::Int(a.wrapping_rem(*b)))
        }
        (Value::BigInt(a), Value::BigInt(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            // i64::MIN % -1 panics in Rust; mathematically the result is 0.
            Ok(Value::BigInt(a.wrapping_rem(*b)))
        }
        (Value::Numeric(_), _) | (_, Value::Numeric(_)) => {
            let a_num = coerce_to_numeric(&args[0])?;
            let b_num = coerce_to_numeric(&args[1])?;
            if a_num.is_nan() || b_num.is_nan() {
                return Ok(Value::Numeric(NumericValue::NAN));
            }
            if b_num.is_zero() {
                return Err(DbError::internal("division by zero"));
            }
            let truncated = a_num
                .div_trunc_int(&b_num)
                .ok_or_else(|| DbError::internal("division by zero"))?;
            if let Some(product) = truncated.mul(&b_num) {
                Ok(Value::Numeric(a_num.sub(&product)))
            } else {
                Ok(Value::Numeric(NumericValue::NAN))
            }
        }
        _ => {
            let a = to_f64(&args[0])?;
            let b = to_f64(&args[1])?;
            if b == 0.0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Double(a % b))
        }
    }
}

pub(crate) fn eval_ln(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "ln")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let (is_numeric, input_scale) = numeric_info(&args[0]);
    // Handle Numeric special values
    if let Value::Numeric(n) = &args[0] {
        if n.is_nan() {
            return Ok(Value::Numeric(aiondb_core::NumericValue::NAN));
        }
        if n.is_pos_infinity() {
            return Ok(Value::Numeric(aiondb_core::NumericValue::INFINITY));
        }
    }
    let result = if let Value::Numeric(n) = &args[0] {
        if n.is_zero() {
            return Err(DbError::internal("cannot take logarithm of zero"));
        }
        if n.is_negative() {
            return Err(DbError::internal(
                "cannot take logarithm of a negative number",
            ));
        }
        let rough = numeric_ln_approx(n)?;
        let scale = pg_ln_result_scale(input_scale, rough);
        if !numeric_can_use_precise(n) {
            return crate::eval::cast::numeric::float_to_numeric_with_scale(rough, Some(scale));
        }
        return Ok(Value::Numeric(numeric_ln_precise_scaled(n, scale)?));
    } else {
        let f = to_f64(&args[0])?;
        if f == 0.0 {
            return Err(DbError::internal("cannot take logarithm of zero"));
        }
        if f < 0.0 {
            return Err(DbError::internal(
                "cannot take logarithm of a negative number",
            ));
        }
        f.ln()
    };
    if is_numeric {
        let scale = pg_ln_result_scale(input_scale, result);
        return crate::eval::cast::numeric::float_to_numeric_with_scale(result, Some(scale));
    }
    Ok(Value::Double(result))
}

pub(crate) fn eval_exp(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "exp")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let (is_numeric, input_scale) = numeric_info(&args[0]);
    // Handle Numeric special values before converting to f64
    if let Value::Numeric(n) = &args[0] {
        if n.is_nan() {
            return Ok(Value::Numeric(aiondb_core::NumericValue::NAN));
        }
        if n.is_pos_infinity() {
            return Ok(Value::Numeric(aiondb_core::NumericValue::INFINITY));
        }
        if n.is_neg_infinity() {
            return Ok(Value::Numeric(aiondb_core::NumericValue::new(0, 0)));
        }
        let rough = n.to_f64();
        let rough_exp = if rough.is_finite() {
            rough.exp()
        } else if rough.is_sign_positive() {
            f64::INFINITY
        } else {
            0.0
        };
        let scale = if rough_exp.is_finite() {
            pg_exp_result_scale(input_scale, rough_exp)
        } else {
            input_scale.max(16)
        };
        if !numeric_can_use_precise(n) {
            return crate::eval::cast::numeric::float_to_numeric_with_scale(rough_exp, Some(scale));
        }
        return Ok(Value::Numeric(numeric_exp_precise_scaled(n, scale)?));
    }
    let f = to_f64(&args[0])?;
    let result = f.exp();
    if result.is_infinite() && !f.is_infinite() {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "value overflows numeric format",
        )));
    }
    if is_numeric {
        // For Numeric inputs, underflow -> return 0 (no error), return as Numeric
        if result == 0.0 && f != 0.0 {
            return Ok(Value::Numeric(aiondb_core::NumericValue::new(0, 0)));
        }
        let scale = pg_exp_result_scale(input_scale, result);
        return crate::eval::cast::numeric::float_to_numeric_with_scale(result, Some(scale));
    }
    if result == 0.0 && f != 0.0 && !f.is_nan() && !f.is_infinite() {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "value out of range: underflow",
        )));
    }
    Ok(Value::Double(result))
}

pub(crate) fn eval_sign(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "sign")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    // PostgreSQL: sign of Numeric returns Numeric
    if let Value::Numeric(n) = &args[0] {
        if n.is_nan() {
            return Ok(Value::Numeric(aiondb_core::NumericValue::NAN));
        }
        if n.is_special() {
            // sign(Infinity) = 1, sign(-Infinity) = -1
            let s = if n.coefficient > 0 { 1i128 } else { -1i128 };
            return Ok(Value::Numeric(aiondb_core::NumericValue::new(s, 0)));
        }
        let s = match n.coefficient.cmp(&0) {
            std::cmp::Ordering::Greater => 1i128,
            std::cmp::Ordering::Less => -1i128,
            std::cmp::Ordering::Equal => 0i128,
        };
        return Ok(Value::Numeric(aiondb_core::NumericValue::new(s, 0)));
    }
    let f = to_f64(&args[0])?;
    let s = if f > 0.0 {
        1.0
    } else if f < 0.0 {
        -1.0
    } else {
        0.0
    };
    Ok(Value::Double(s))
}

pub(crate) fn eval_pi(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 0, "pi")?;
    Ok(Value::Double(std::f64::consts::PI))
}

pub(crate) fn eval_random(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 0, "random")?;
    // Simple pseudo-random based on current time microseconds.
    // We use the lower bits of the nanosecond timestamp mixed with a
    // simple hash to produce a value in [0, 1).
    let nanos = {
        use std::time::SystemTime;
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    };
    // xorshift-style mixing to avoid sequential correlation
    let mut x = u64::try_from(nanos & u128::from(u64::MAX)).unwrap_or(u64::MAX);
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    x = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
    // Map to [0, 1)
    let val = u64_to_f64(x >> 11) / u64_to_f64(1u64 << 53);
    Ok(Value::Double(val))
}

pub(crate) fn eval_greatest(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        return Err(DbError::internal(
            "greatest() requires at least one argument",
        ));
    }
    // If any arg is text, compare as text (PG polymorphic behavior)
    if args.iter().any(|a| matches!(a, Value::Text(_))) {
        let mut best: Option<String> = None;
        for arg in args {
            if arg.is_null() {
                continue;
            }
            let s = match arg {
                Value::Text(s) => s.clone(),
                other => super::value_to_text(other),
            };
            best = Some(match best {
                Some(current) if current >= s => current,
                _ => s,
            });
        }
        return Ok(best.map_or(Value::Null, Value::Text));
    }
    let mut best: Option<f64> = None;
    for arg in args {
        if arg.is_null() {
            continue;
        }
        let f = to_f64(arg)?;
        best = Some(match best {
            Some(current) if current >= f => current,
            _ => f,
        });
    }
    match best {
        Some(v) => Ok(Value::Double(v)),
        None => Ok(Value::Null),
    }
}

pub(crate) fn eval_least(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        return Err(DbError::internal("least() requires at least one argument"));
    }
    // If any arg is text, compare as text (PG polymorphic behavior)
    if args.iter().any(|a| matches!(a, Value::Text(_))) {
        let mut best: Option<String> = None;
        for arg in args {
            if arg.is_null() {
                continue;
            }
            let s = match arg {
                Value::Text(s) => s.clone(),
                other => super::value_to_text(other),
            };
            best = Some(match best {
                Some(current) if current <= s => current,
                _ => s,
            });
        }
        return Ok(best.map_or(Value::Null, Value::Text));
    }
    let mut best: Option<f64> = None;
    for arg in args {
        if arg.is_null() {
            continue;
        }
        let f = to_f64(arg)?;
        best = Some(match best {
            Some(current) if current <= f => current,
            _ => f,
        });
    }
    match best {
        Some(v) => Ok(Value::Double(v)),
        None => Ok(Value::Null),
    }
}

/// `width_bucket(operand, low, high, count)` -- returns the bucket number
/// (1-based) in an equi-width histogram with `count` buckets spanning
/// `[low, high)`.  Returns 0 when `operand < low` and `count + 1` when
/// `operand >= high`, matching `PostgreSQL` semantics.
///
/// Also supports 2-arg form: `width_bucket(operand, thresholds_array)`.
pub(crate) fn eval_width_bucket(args: &[Value]) -> DbResult<Value> {
    if args.len() == 4 {
        if args.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let operand = to_f64(&args[0])?;
        let low = to_f64(&args[1])?;
        let high = to_f64(&args[2])?;
        let count = match &args[3] {
            Value::Int(n) => i64::from(*n),
            Value::BigInt(n) => *n,
            other => {
                let f = to_f64(other)?;
                // i64::MAX as f64 rounds up to 9223372036854775808.0 (> i64::MAX),
                // so use >= on the upper bound to exclude that representable value.
                if !f.is_finite() || f >= i64_to_f64(i64::MAX) || f < i64_to_f64(i64::MIN) {
                    return Err(DbError::internal("count out of range for bigint"));
                }
                f64_to_i64(f).map_err(|_| DbError::internal("count out of range for bigint"))?
            }
        };
        if count <= 0 {
            return Err(DbError::internal("count must be greater than zero"));
        }
        // PostgreSQL validates NaN and infinity
        if operand.is_nan() || low.is_nan() || high.is_nan() {
            return Err(DbError::internal(
                "operand, lower bound, and upper bound cannot be NaN",
            ));
        }
        if low.is_infinite() || high.is_infinite() {
            return Err(DbError::internal("lower and upper bounds must be finite"));
        }
        if low == high {
            return Err(DbError::internal("lower bound cannot equal upper bound"));
        }

        // PostgreSQL supports reversed bounds (low > high).
        let bucket = if low < high {
            if operand < low {
                0i64
            } else if operand >= high {
                count.saturating_add(1)
            } else {
                let fraction = (operand - low) / (high - low);
                let b = f64_to_i64((fraction * i64_to_f64(count)).floor())
                    .map_err(|_| DbError::internal("count out of range for bigint"))?
                    .saturating_add(1);
                b.clamp(1, count)
            }
        } else {
            // reversed: high < low
            if operand > low {
                0i64
            } else if operand <= high {
                count.saturating_add(1)
            } else {
                let fraction = (low - operand) / (low - high);
                let b = f64_to_i64((fraction * i64_to_f64(count)).floor())
                    .map_err(|_| DbError::internal("count out of range for bigint"))?
                    .saturating_add(1);
                b.clamp(1, count)
            }
        };
        // Check that the result fits in i32 (PostgreSQL returns int4)
        let result = i32::try_from(bucket).map_err(|_| {
            DbError::from_report(ErrorReport::new(
                SqlState::NumericValueOutOfRange,
                "integer out of range",
            ))
        })?;
        Ok(Value::Int(result))
    } else if args.len() == 2 {
        // width_bucket(operand, thresholds_array)
        if args.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let (bounds, threshold_elements) = coerce_to_array_with_bounds(&args[1])
            .ok_or_else(|| DbError::internal("width_bucket() second argument must be an array"))?;
        if bounds.len() > 1
            || threshold_elements
                .iter()
                .any(|value| matches!(value, Value::Array(_)))
        {
            return Err(DbError::internal(
                "thresholds must be one-dimensional array",
            ));
        }
        let mut thresholds = Vec::with_capacity(threshold_elements.len());
        for value in threshold_elements.iter() {
            if value.is_null() {
                return Err(DbError::internal("thresholds array must not contain NULLs"));
            }
            thresholds.push(value.clone());
        }
        let mut bucket = 0i32;
        for threshold in &thresholds {
            match compare_runtime_values(&args[0], threshold)? {
                Some(std::cmp::Ordering::Less) => break,
                Some(_) => bucket += 1,
                None => {
                    return Err(DbError::internal(
                        "width_bucket() requires comparable operand and threshold values",
                    ));
                }
            }
        }
        Ok(Value::Int(bucket))
    } else {
        Err(DbError::internal(
            "width_bucket() requires 2 or 4 arguments",
        ))
    }
}

// =====================================================================
// Trigonometric & hyperbolic functions
// =====================================================================
