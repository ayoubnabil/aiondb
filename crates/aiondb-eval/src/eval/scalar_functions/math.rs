use aiondb_core::{
    convert::u32_to_i32_saturating, DbError, DbResult, ErrorReport, IntervalValue, SqlState, Value,
};

use crate::eval::cast::cast_interval_with_fields;

use super::ext::coerce_to_array_with_bounds;
pub(crate) use super::math_advanced::*;
pub(crate) use super::math_generic::eval_math_generic;
pub(crate) use super::math_trig::eval_trig;
use super::value_convert::{f64_to_i64, i64_to_f64, interval_out_of_range, to_i32_saturating};
use super::{expect_arg_range, expect_args, to_f64};

// =====================================================================
// Pure Rust error functions (erf, erfc)
// =====================================================================

/// Compute the error function using Horner-form rational approximation.
/// Uses the Abramowitz & Stegun method (Handbook of Mathematical Functions,
/// formulas 7.1.25-7.1.28) which provides ~1.5e-7 relative accuracy.
pub(super) fn c_erf(x: f64) -> f64 {
    let a1: f64 = 0.254829592;
    let a2: f64 = -0.284496736;
    let a3: f64 = 1.421413741;
    let a4: f64 = -1.453152027;
    let a5: f64 = 1.061405429;
    let p: f64 = 0.3275911;
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + p * x);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-x * x).exp();
    sign * y
}

/// Compute the complementary error function: erfc(x) = 1 - erf(x).
pub(super) fn c_erfc(x: f64) -> f64 {
    1.0 - c_erf(x)
}

use crate::eval::DAY_MICROS_I128;

#[inline]
fn nonneg_i32_to_u32(value: i32) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

pub(crate) fn interval_from_parts(
    months: i128,
    days: i128,
    micros: i128,
) -> DbResult<IntervalValue> {
    Ok(IntervalValue::new(
        i32::try_from(months).map_err(|_| interval_out_of_range())?,
        i32::try_from(days).map_err(|_| interval_out_of_range())?,
        i64::try_from(micros).map_err(|_| interval_out_of_range())?,
    ))
}

pub(crate) fn justify_hours_parts(days: &mut i128, micros: &mut i128) -> DbResult<()> {
    *days = days
        .checked_add(*micros / DAY_MICROS_I128)
        .ok_or_else(interval_out_of_range)?;
    *micros %= DAY_MICROS_I128;
    Ok(())
}

pub(crate) fn justify_days_parts(months: &mut i128, days: &mut i128) -> DbResult<()> {
    *months = months
        .checked_add(*days / 30)
        .ok_or_else(interval_out_of_range)?;
    *days %= 30;
    Ok(())
}

pub(crate) fn justify_hours_interval(iv: &IntervalValue) -> DbResult<IntervalValue> {
    let mut days = i128::from(iv.days);
    let mut micros = i128::from(iv.micros);
    justify_hours_parts(&mut days, &mut micros)?;
    interval_from_parts(i128::from(iv.months), days, micros)
}

pub(crate) fn justify_days_interval(iv: &IntervalValue) -> DbResult<IntervalValue> {
    let mut months = i128::from(iv.months);
    let mut days = i128::from(iv.days);
    justify_days_parts(&mut months, &mut days)?;
    interval_from_parts(months, days, i128::from(iv.micros))
}

pub(crate) fn justify_interval_value(iv: &IntervalValue) -> DbResult<IntervalValue> {
    let mut months = i128::from(iv.months);
    let mut days = i128::from(iv.days);
    let mut micros = i128::from(iv.micros);

    justify_hours_parts(&mut days, &mut micros)?;
    justify_days_parts(&mut months, &mut days)?;

    loop {
        let before = (months, days, micros);

        if months > 0 && (days < 0 || micros < 0) {
            months -= 1;
            days = days.checked_add(30).ok_or_else(interval_out_of_range)?;
        } else if months < 0 && (days > 0 || micros > 0) {
            months += 1;
            days = days.checked_sub(30).ok_or_else(interval_out_of_range)?;
        }

        if days > 0 && micros < 0 {
            days -= 1;
            micros = micros
                .checked_add(DAY_MICROS_I128)
                .ok_or_else(interval_out_of_range)?;
        } else if days < 0 && micros > 0 {
            days += 1;
            micros = micros
                .checked_sub(DAY_MICROS_I128)
                .ok_or_else(interval_out_of_range)?;
        }

        if before == (months, days, micros) {
            break;
        }
    }

    interval_from_parts(months, days, micros)
}

pub(crate) fn eval_interval_precision(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "__aiondb_interval_precision")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    let Value::Interval(interval) = &args[0] else {
        return Err(DbError::internal(
            "__aiondb_interval_precision() first argument must be interval",
        ));
    };
    let precision = match &args[1] {
        Value::Int(value) => *value,
        Value::BigInt(value) => i32::try_from(*value).map_err(|_| {
            DbError::internal("__aiondb_interval_precision() precision must fit in int4")
        })?,
        _ => {
            return Err(DbError::internal(
                "__aiondb_interval_precision() precision must be integer",
            ));
        }
    };
    if !(0..=6).contains(&precision) {
        return Err(DbError::internal(
            "__aiondb_interval_precision() precision must be between 0 and 6",
        ));
    }

    let rounding_unit = 10_i128.pow(nonneg_i32_to_u32(6 - precision));
    let micros = i128::from(interval.micros);
    let rounded_micros = if micros >= 0 {
        ((micros + rounding_unit / 2) / rounding_unit) * rounding_unit
    } else {
        ((micros - rounding_unit / 2) / rounding_unit) * rounding_unit
    };
    let day_delta = rounded_micros / DAY_MICROS_I128;
    let remaining_micros = rounded_micros % DAY_MICROS_I128;
    let total_days = i128::from(interval.days) + day_delta;

    Ok(Value::Interval(interval_from_parts(
        i128::from(interval.months),
        total_days,
        remaining_micros,
    )?))
}

pub(crate) fn eval_interval_fields(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 4, "__aiondb_interval_fields")?;
    let start_field = match &args[1] {
        Value::Text(value) => value.as_str(),
        Value::Null => {
            return Err(DbError::internal(
                "__aiondb_interval_fields() start field must be text",
            ));
        }
        _ => {
            return Err(DbError::internal(
                "__aiondb_interval_fields() start field must be text",
            ));
        }
    };
    let end_field = match &args[2] {
        Value::Text(value) => Some(value.as_str()),
        Value::Null => None,
        _ => {
            return Err(DbError::internal(
                "__aiondb_interval_fields() end field must be text or null",
            ));
        }
    };
    let second_precision = match &args[3] {
        Value::Int(value) => Some(u32::try_from(*value).map_err(|_| {
            DbError::internal("__aiondb_interval_fields() precision must fit in int4")
        })?),
        Value::BigInt(value) => Some(u32::try_from(*value).map_err(|_| {
            DbError::internal("__aiondb_interval_fields() precision must fit in int4")
        })?),
        Value::Null => None,
        _ => {
            return Err(DbError::internal(
                "__aiondb_interval_fields() precision must be integer or null",
            ));
        }
    };
    cast_interval_with_fields(args[0].clone(), start_field, end_field, second_precision)
}

// =====================================================================
// Math functions
// =====================================================================

pub(crate) fn eval_abs(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "abs")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Int(v) => v
            .checked_abs()
            .map(Value::Int)
            .ok_or_else(|| DbError::internal("integer out of range")),
        Value::BigInt(v) => v
            .checked_abs()
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        Value::Real(v) => Ok(Value::Real(v.abs())),
        Value::Double(v) => Ok(Value::Double(v.abs())),
        Value::Numeric(n) => Ok(Value::Numeric(n.abs())),
        other => {
            let f = to_f64(other)?;
            Ok(Value::Double(f.abs()))
        }
    }
}

pub(crate) fn eval_ceil(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "ceil")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    // PostgreSQL: ceil of Numeric returns Numeric(scale=0)
    if let Value::Numeric(n) = &args[0] {
        if n.is_special() {
            return Ok(Value::Numeric(n.clone()));
        }
        if n.scale == 0 {
            return Ok(Value::Numeric(n.clone()));
        }
        let truncated = n.trunc(0);
        // If the original value is greater than the truncated value, add 1
        // (i.e., there is a positive fractional part)
        let has_frac = n.coefficient
            != truncated.coefficient * aiondb_core::numeric::checked_ten_pow(n.scale).unwrap_or(1);
        if has_frac && n.coefficient > 0 {
            return Ok(Value::Numeric(aiondb_core::NumericValue::new(
                truncated.coefficient + 1,
                0,
            )));
        }
        return Ok(Value::Numeric(truncated));
    }
    let f = to_f64(&args[0])?;
    Ok(Value::Double(f.ceil()))
}

pub(crate) fn eval_floor(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "floor")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    // PostgreSQL: floor of Numeric returns Numeric(scale=0)
    if let Value::Numeric(n) = &args[0] {
        if n.is_special() {
            return Ok(Value::Numeric(n.clone()));
        }
        if n.scale == 0 {
            return Ok(Value::Numeric(n.clone()));
        }
        let truncated = n.trunc(0);
        // If the original value is less than the truncated value, subtract 1
        // (i.e., there is a negative fractional part - value is negative with fraction)
        let has_frac = n.coefficient
            != truncated.coefficient * aiondb_core::numeric::checked_ten_pow(n.scale).unwrap_or(1);
        if has_frac && n.coefficient < 0 {
            return Ok(Value::Numeric(aiondb_core::NumericValue::new(
                truncated.coefficient - 1,
                0,
            )));
        }
        return Ok(Value::Numeric(truncated));
    }
    let f = to_f64(&args[0])?;
    Ok(Value::Double(f.floor()))
}

pub(crate) fn eval_round(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 1, 2, "round() requires 1 or 2 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    // For Numeric inputs, use NumericValue::round to preserve type and scale
    if let Value::Numeric(n) = &args[0] {
        if n.is_special() {
            return Ok(Value::Numeric(n.clone()));
        }
        let places = if args.len() == 2 {
            match &args[1] {
                Value::Int(p) => *p,
                Value::BigInt(p) => super::value_convert::i64_to_i32(*p)?,
                _ => return Err(DbError::internal("round() second arg must be an integer")),
            }
        } else {
            0
        };
        if places < 0 {
            // Negative scale: round to tens, hundreds, etc.
            // Use i64 cast to avoid panic when places == i32::MIN.
            let neg = u32::try_from(i64::from(places).saturating_neg()).unwrap_or(u32::MAX);
            let rounded = n.round_neg(neg);
            return Ok(Value::Numeric(rounded));
        }
        let rounded = n.round(nonneg_i32_to_u32(places));
        // Ensure the scale matches the requested places (for display like "0.0")
        let target_scale = nonneg_i32_to_u32(places);
        if rounded.scale < target_scale {
            // Extend scale by multiplying coefficient
            let diff = target_scale - rounded.scale;
            let factor = 10i128.checked_pow(diff).unwrap_or(1);
            return Ok(Value::Numeric(aiondb_core::NumericValue::new(
                rounded.coefficient * factor,
                target_scale,
            )));
        }
        return Ok(Value::Numeric(rounded));
    }
    let f = to_f64(&args[0])?;
    if args.len() == 2 {
        let places = match &args[1] {
            Value::Int(n) => *n,
            Value::BigInt(n) => super::value_convert::i64_to_i32(*n)?,
            _ => return Err(DbError::internal("round() second arg must be an integer")),
        };
        let factor = 10_f64.powi(places);
        Ok(Value::Double((f * factor).round() / factor))
    } else {
        Ok(Value::Double(f.round()))
    }
}

pub(crate) fn eval_trunc(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 1, 2, "trunc() requires 1 or 2 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    if args.len() == 1 {
        match &args[0] {
            Value::MacAddr(value) => return Ok(Value::MacAddr(value.trunc())),
            Value::MacAddr8(value) => return Ok(Value::MacAddr8(value.trunc())),
            _ => {}
        }
    }
    // For Numeric inputs, use NumericValue::trunc to preserve type and scale
    if let Value::Numeric(n) = &args[0] {
        if n.is_special() {
            return Ok(Value::Numeric(n.clone()));
        }
        let places = if args.len() == 2 {
            match &args[1] {
                Value::Int(p) => *p,
                Value::BigInt(p) => super::value_convert::i64_to_i32(*p)?,
                _ => return Err(DbError::internal("trunc() second arg must be an integer")),
            }
        } else {
            0
        };
        if places < 0 {
            // Negative scale: truncate to tens, hundreds, etc.
            // Use i64 cast to avoid panic when places == i32::MIN.
            let neg_scale = u32::try_from(i64::from(places).saturating_neg()).unwrap_or(u32::MAX);
            // First truncate fractional part, then truncate integer part
            let int_val = n.trunc(0);
            let Some(divisor) = aiondb_core::numeric::checked_ten_pow(neg_scale) else {
                // Divisor overflows → value truncates to 0
                return Ok(Value::Numeric(aiondb_core::NumericValue::new(0, 0)));
            };
            let result_coeff = int_val.coefficient / divisor;
            match result_coeff.checked_mul(divisor) {
                Some(coefficient) => {
                    return Ok(Value::Numeric(aiondb_core::NumericValue::new(
                        coefficient,
                        0,
                    )));
                }
                None => return Ok(Value::Numeric(aiondb_core::NumericValue::NAN)),
            }
        }
        let truncated = n.trunc(nonneg_i32_to_u32(places));
        // Ensure the scale matches the requested places (for display like "0.0")
        let target_scale = nonneg_i32_to_u32(places);
        if truncated.scale < target_scale {
            let diff = target_scale - truncated.scale;
            let factor = 10i128.checked_pow(diff).unwrap_or(1);
            return Ok(Value::Numeric(aiondb_core::NumericValue::new(
                truncated.coefficient * factor,
                target_scale,
            )));
        }
        return Ok(Value::Numeric(truncated));
    }
    let f = to_f64(&args[0])?;
    if args.len() == 2 {
        let places = match &args[1] {
            Value::Int(n) => *n,
            Value::BigInt(n) => super::value_convert::i64_to_i32(*n)?,
            _ => return Err(DbError::internal("trunc() second arg must be an integer")),
        };
        let factor = 10_f64.powi(places);
        Ok(Value::Double((f * factor).trunc() / factor))
    } else {
        Ok(Value::Double(f.trunc()))
    }
}

pub(crate) fn eval_power(args: &[Value]) -> DbResult<Value> {
    use aiondb_core::NumericValue;

    expect_args(args, 2, "power")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    // Check if either argument is Numeric type - if so, result should be Numeric
    let is_numeric = matches!(&args[0], Value::Numeric(_)) || matches!(&args[1], Value::Numeric(_));

    // For Numeric inputs, handle special values first
    if is_numeric {
        let base_num = super::math_generic::coerce_to_numeric(&args[0])?;
        let exp_num = super::math_generic::coerce_to_numeric(&args[1])?;

        // PostgreSQL special cases: NaN^0 = 1, 1^NaN = 1
        // For these special cases (NaN, infinity, or 1 involved), PG returns
        // exactly 1 (scale 0).
        if exp_num.is_zero() {
            // anything^0 = 1, including NaN^0 = 1
            if base_num.is_special() {
                // NaN^0 and inf^0 return plain 1
                return Ok(Value::Numeric(NumericValue::new(1, 0)));
            }
            // For regular numeric base^0, PG returns 1.0000000000000000
            let scale = 16u32;
            let coeff = 10i128.pow(scale);
            return Ok(Value::Numeric(NumericValue::new(coeff, scale)));
        }
        // 1^anything = 1, including 1^NaN = 1
        if !base_num.is_special() {
            let base_is_one = if base_num.scale == 0 {
                base_num.coefficient == 1
            } else {
                let pow10 = 10i128.checked_pow(base_num.scale).unwrap_or(0);
                pow10 != 0 && base_num.coefficient == pow10
            };
            if base_is_one {
                if exp_num.is_special() {
                    // 1^NaN, 1^inf return plain 1
                    return Ok(Value::Numeric(NumericValue::new(1, 0)));
                }
                let scale = 16u32;
                let coeff = 10i128.pow(scale);
                return Ok(Value::Numeric(NumericValue::new(coeff, scale)));
            }
        }
        // NaN propagates (after special cases above)
        if base_num.is_nan() || exp_num.is_nan() {
            return Ok(Value::Numeric(NumericValue::NAN));
        }

        let base_f = to_f64(&args[0])?;
        let exp_f = to_f64(&args[1])?;
        let exp_is_integer = numeric_as_i64_if_integer(&exp_num).is_some();

        // Zero raised to a negative power
        if base_num.is_zero() && exp_num.is_negative() {
            return Err(DbError::internal(
                "zero raised to a negative power is undefined",
            ));
        }

        // Negative base raised to non-integer power (includes -infinity)
        if (base_f < 0.0 || base_num.is_neg_infinity())
            && !exp_is_integer
            && !exp_num.is_infinite()
            && !exp_num.is_zero()
        {
            return Err(DbError::internal(
                "a negative number raised to a non-integer power yields a complex result",
            ));
        }

        // Handle infinity base
        if base_num.is_infinite() {
            if exp_f == 0.0 {
                // inf^0 = 1 (already handled above, but safety)
                return Ok(Value::Numeric(NumericValue::new(1, 0)));
            }
            if exp_f < 0.0 {
                // inf^(-n) = 0
                return Ok(Value::Numeric(NumericValue::new(0, 0)));
            }
            // inf^(positive)
            if base_num.is_neg_infinity() {
                // (-inf)^n: if n is odd integer, result is -Infinity; else Infinity
                let is_odd_int = exp_f.fract() == 0.0
                    && exp_f.is_finite()
                    && exp_f >= i64_to_f64(i64::MIN)
                    && exp_f <= i64_to_f64(i64::MAX)
                    && f64_to_i64(exp_f)
                        .map(|value| value % 2 != 0)
                        .unwrap_or(false);
                if is_odd_int {
                    return Ok(Value::Numeric(NumericValue::NEG_INFINITY));
                }
            }
            return Ok(Value::Numeric(NumericValue::INFINITY));
        }

        // Infinity exponent
        if exp_num.is_infinite() {
            if base_f.abs() == 1.0 {
                return Ok(Value::Numeric(NumericValue::new(1, 0)));
            }
            if exp_num.is_pos_infinity() {
                if base_f.abs() > 1.0 {
                    return Ok(Value::Numeric(NumericValue::INFINITY));
                }
                return Ok(Value::Numeric(NumericValue::new(0, 0)));
            }
            // -infinity exponent
            if base_f.abs() > 1.0 {
                return Ok(Value::Numeric(NumericValue::new(0, 0)));
            }
            return Ok(Value::Numeric(NumericValue::INFINITY));
        }

        // Use rough f64 only to estimate display scale, then compute with
        // high-precision Numeric arithmetic to avoid losing significant digits.
        let approx = base_f.powf(exp_f);
        if !numeric_can_use_precise(&base_num) || !numeric_can_use_precise(&exp_num) {
            if approx.is_infinite() {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    "value overflows numeric format",
                )));
            }
            let base_scale = base_num.scale.max(exp_num.scale);
            let min_scale = base_scale.max(16);
            let scale = pg_ensure_sig_digits(min_scale, approx, 16);
            return crate::eval::cast::numeric::float_to_numeric_with_scale(approx, Some(scale));
        }
        let scale = numeric_power_result_scale(base_num.scale, exp_num.scale, approx);
        let value = numeric_pow_precise_scaled(&base_num, &exp_num, scale)?;
        return Ok(Value::Numeric(value));
    }

    let base = to_f64(&args[0])?;
    let exp = to_f64(&args[1])?;
    // PostgreSQL-compatible error checks
    if base == 0.0 && exp < 0.0 {
        return Err(DbError::internal(
            "zero raised to a negative power is undefined",
        ));
    }
    // NaN exponent: let IEEE handle it ((-1)^NaN = NaN, etc.)
    if base < 0.0 && !exp.is_nan() && exp.fract() != 0.0 && !exp.is_infinite() {
        return Err(DbError::internal(
            "a negative number raised to a non-integer power yields a complex result",
        ));
    }
    let result = base.powf(exp);
    if result.is_infinite() && !base.is_infinite() && !exp.is_infinite() {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "value overflows numeric format",
        )));
    }
    if result == 0.0 && base != 0.0 && !base.is_infinite() && !exp.is_infinite() && exp != 0.0 {
        // Check for underflow: skip when base is infinity
        // (inf^(-2) = 0 is correct IEEE behavior, not underflow)
        if base.abs() > 0.0 && base.abs() != 1.0 {
            return Err(DbError::from_report(ErrorReport::new(
                SqlState::NumericValueOutOfRange,
                "value out of range: underflow",
            )));
        }
    }
    Ok(Value::Double(result))
}

/// `PostgreSQL`'s minimum significant digits for numeric transcendental results.
const NUMERIC_MIN_SIG_DIGITS: u32 = 16;

/// Compute the PostgreSQL-compatible result scale for numeric sqrt.
///
/// `PostgreSQL`'s `sqrt_var()` computes:
///   sweight = (arg.weight + 1) * `DEC_DIGITS` / 2 - 1
///   rscale  = `NUMERIC_MIN_SIG_DIGITS` - sweight
///   rscale  = max(rscale, arg.dscale)
///
/// Since we don't have base-10000 weights, we approximate `sweight` from the
/// result magnitude:  sweight ≈ `int_digits_of_result` - 1.
pub(crate) fn pg_sqrt_result_scale(input_scale: u32, result: f64) -> u32 {
    // Estimate the "result weight" (number of integer digits - 1)
    let sweight = if result == 0.0 || result.abs() < 1.0 {
        // For results < 1, the sweight is effectively 0 or negative.
        // PostgreSQL uses sweight = (arg.weight+1)*2 - 1 from the input.
        // For zero input (weight=0): sweight = (0+1)*4/2 - 1 = 1
        // For small inputs, just use 1 which gives rscale = 15.
        1i32
    } else {
        // result.abs() >= 1.0, so log10() is >= 0.0 and finite (max ~308 for f64).
        // The cast to i32 is safe here.
        i32::try_from(f64_to_i64(result.abs().log10().floor()).unwrap_or(i64::MAX))
            .unwrap_or(i32::MAX)
            .saturating_add(1)
    };
    let rscale_from_sig = u32::try_from((16 - sweight).max(0)).unwrap_or(0);

    rscale_from_sig.max(input_scale)
}

/// Compute the PostgreSQL-compatible result scale for numeric ln / log10.
///
/// `PostgreSQL` uses: `rscale = Max(arg->dscale, NUMERIC_MIN_SIG_DIGITS)` and
/// then adjusts based on the result magnitude to ensure 16 significant digits.
pub(crate) fn pg_ln_result_scale(input_scale: u32, result: f64) -> u32 {
    let base_scale = input_scale.max(NUMERIC_MIN_SIG_DIGITS);
    pg_ensure_sig_digits(base_scale, result, NUMERIC_MIN_SIG_DIGITS)
}

/// Compute the PostgreSQL-compatible result scale for numeric exp.
///
/// For `exp()`, `PostgreSQL` computes with arbitrary precision and adjusts the
/// result scale based on both the input scale and the result magnitude.
/// Since we use f64 internally (≈15-17 significant digits), we compute a
/// scale that shows as many meaningful digits as f64 can provide.
pub(crate) fn pg_exp_result_scale(input_scale: u32, result: f64) -> u32 {
    let base_scale = input_scale.max(NUMERIC_MIN_SIG_DIGITS);
    pg_ensure_sig_digits(base_scale, result, NUMERIC_MIN_SIG_DIGITS)
}

/// Ensure a result has at least `min_sig` significant digits by adjusting
/// the scale upwards when the result magnitude is small.
pub(crate) fn pg_ensure_sig_digits(base_scale: u32, result: f64, min_sig: u32) -> u32 {
    if result == 0.0 {
        return base_scale.max(min_sig);
    }
    let abs_result = result.abs();
    // Number of digits before the decimal point (can be negative for small numbers).
    // log10() can return -inf for subnormal values that are not exactly zero, so
    // we clamp to a finite i32 range before adding 1 to avoid UB.
    let log_floor = abs_result.log10().floor();
    let int_digits = if log_floor.is_finite() {
        // log10(f64::MAX) ≈ 308, log10(f64::MIN_POSITIVE) ≈ -308; both fit in i32.
        i32::try_from(f64_to_i64(log_floor).unwrap_or(i64::MAX))
            .unwrap_or(i32::MAX)
            .saturating_add(1)
    } else {
        // -inf: the value is a denormal so tiny that we need maximum precision.
        i32::MIN / 2
    };
    // We need enough decimal places so that int_digits + scale >= min_sig
    let needed_scale = if int_digits >= u32_to_i32_saturating(min_sig) {
        // Result is large enough that even 0 decimal places gives min_sig
        // significant digits.  But still respect the base_scale minimum.
        0u32
    } else {
        // int_digits < min_sig, so min_sig as i32 - int_digits > 0 and fits in u32.
        u32::try_from(i64::from(u32_to_i32_saturating(min_sig)) - i64::from(int_digits))
            .unwrap_or(u32::MAX)
    };
    base_scale.max(needed_scale)
}

/// `num_nulls(VARIADIC args)` - count the number of NULL arguments.
pub(crate) fn eval_num_nulls(args: &[Value]) -> Value {
    let count = args.iter().filter(|v| v.is_null()).count();
    Value::Int(to_i32_saturating(count))
}

/// `num_nonnulls(VARIADIC args)` - count the number of non-NULL arguments.
pub(crate) fn eval_num_nonnulls(args: &[Value]) -> Value {
    let count = args.iter().filter(|v| !v.is_null()).count();
    Value::Int(to_i32_saturating(count))
}

pub(crate) fn eval_num_nulls_variadic(args: &[Value]) -> DbResult<Value> {
    eval_variadic_array_null_counter(args, true)
}

pub(crate) fn eval_num_nonnulls_variadic(args: &[Value]) -> DbResult<Value> {
    eval_variadic_array_null_counter(args, false)
}

fn eval_variadic_array_null_counter(args: &[Value], count_nulls: bool) -> DbResult<Value> {
    expect_args(args, 1, "num_nulls_variadic")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Array(elements) => {
            let count = elements
                .iter()
                .filter(|value| value.is_null() == count_nulls)
                .count();
            Ok(Value::Int(to_i32_saturating(count)))
        }
        _ => Err(DbError::internal(
            "VARIADIC argument for num_nulls/num_nonnulls must be an array",
        )),
    }
}

/// `generate_subscripts(array, dim [, reverse])` - Generate series of subscripts.
pub(crate) fn eval_generate_subscripts(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(
        args,
        1,
        3,
        "generate_subscripts() requires 2 or 3 arguments",
    )?;
    if args[0].is_null() {
        return Ok(Value::Array(Vec::new()));
    }
    let dimension_bounds = generate_subscript_dimension_bounds(&args[0]).ok_or_else(|| {
        DbError::internal("generate_subscripts() first argument must be an array")
    })?;
    let dim = if args.len() >= 2 {
        match &args[1] {
            Value::Int(d) => i64::from(*d),
            Value::BigInt(d) => *d,
            Value::Null => return Ok(Value::Array(Vec::new())),
            _ => 1,
        }
    } else {
        1
    };
    if dim <= 0 {
        return Ok(Value::Array(Vec::new()));
    }
    let Some(dim_index) = usize::try_from(dim - 1).ok() else {
        return Ok(Value::Array(Vec::new()));
    };
    let Some((lower_bound, upper_bound)) = dimension_bounds.get(dim_index).copied() else {
        return Ok(Value::Array(Vec::new()));
    };
    let reverse = args.len() == 3 && matches!(&args[2], Value::Boolean(true));
    let mut result: Vec<Value> = if upper_bound < lower_bound {
        Vec::new()
    } else {
        (lower_bound..=upper_bound)
            .map(|index| Value::Int(to_i32_saturating(index)))
            .collect()
    };
    if reverse {
        result.reverse();
    }
    Ok(Value::Array(result))
}

fn generate_subscript_dimension_bounds(value: &Value) -> Option<Vec<(i64, i64)>> {
    let mut bounds = Vec::new();
    collect_generate_subscript_dimension_bounds(value, &mut bounds)?;
    Some(bounds)
}

fn collect_generate_subscript_dimension_bounds(
    value: &Value,
    bounds: &mut Vec<(i64, i64)>,
) -> Option<()> {
    let (explicit_bounds, elements) = coerce_to_array_with_bounds(value)?;
    if !explicit_bounds.is_empty() {
        bounds.extend(explicit_bounds);
        return Some(());
    }
    if elements.is_empty() {
        return Some(());
    }

    bounds.push((1, i64::try_from(elements.len()).ok()?));
    let first = elements.first()?;
    if coerce_to_array_with_bounds(first).is_some() {
        collect_generate_subscript_dimension_bounds(first, bounds)?;
    }
    Some(())
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
#[path = "math_tests.rs"]
mod tests;
