use super::*;
use crate::eval::scalar_functions::value_convert::{i32_to_f32, i64_to_f64};

pub(crate) fn eval_arith_div(left: &Value, right: &Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Int(a), Value::Int(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            a.checked_div(*b).map(Value::Int).ok_or_else(|| {
                DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    "integer out of range",
                ))
            })
        }
        (Value::Int(a), Value::BigInt(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            i64::from(*a)
                .checked_div(*b)
                .map(Value::BigInt)
                .ok_or_else(|| DbError::internal("bigint out of range"))
        }
        (Value::BigInt(a), Value::Int(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            a.checked_div(i64::from(*b))
                .map(Value::BigInt)
                .ok_or_else(|| DbError::internal("bigint out of range"))
        }
        (Value::BigInt(a), Value::BigInt(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            a.checked_div(*b)
                .map(Value::BigInt)
                .ok_or_else(|| DbError::internal("bigint out of range"))
        }
        // IEEE-754: float8/float4 divide by zero yields ±Infinity (or NaN
        // for 0/0). PG matches this for float types — only int and numeric
        // raise. Whenever either operand is Real or Double the result is
        // promoted to float and the division never errors.
        (Value::Real(a), Value::Real(b)) => Ok(Value::Real(a / b)),
        (Value::Double(a), Value::Double(b)) => Ok(Value::Double(a / b)),
        (Value::Int(a), Value::Real(b)) => Ok(Value::Real(i32_to_f32(*a) / b)),
        (Value::Real(a), Value::Int(b)) => Ok(Value::Real(a / i32_to_f32(*b))),
        (Value::Int(a), Value::Double(b)) => Ok(Value::Double(f64::from(*a) / b)),
        (Value::Double(a), Value::Int(b)) => Ok(Value::Double(a / f64::from(*b))),
        (Value::BigInt(a), Value::Double(b)) => Ok(Value::Double(i64_to_f64(*a) / b)),
        (Value::Double(a), Value::BigInt(b)) => Ok(Value::Double(a / i64_to_f64(*b))),
        (Value::Money(a), Value::Money(b)) => {
            let left = money_to_numeric(*a);
            let right = money_to_numeric(*b);
            if right.coefficient == 0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Double(
                numeric_to_f64(&left) / numeric_to_f64(&right),
            ))
        }
        (Value::Money(a), Value::Int(b)) => money_div_i64(*a, i64::from(*b)).map(Value::Money),
        (Value::Money(a), Value::BigInt(b)) => money_div_i64(*a, *b).map(Value::Money),
        (Value::Money(a), Value::Real(b)) => money_div_f64(*a, f64::from(*b)).map(Value::Money),
        (Value::Money(a), Value::Double(b)) => money_div_f64(*a, *b).map(Value::Money),
        (Value::Numeric(a), Value::Numeric(b)) => a
            .div(b)
            .map(Value::Numeric)
            .ok_or_else(|| DbError::internal("division by zero")),
        (Value::Int(a), Value::Numeric(b)) => {
            let a = NumericValue::new(i128::from(*a), 0);
            a.div(b)
                .map(Value::Numeric)
                .ok_or_else(|| DbError::internal("division by zero"))
        }
        (Value::Numeric(a), Value::Int(b)) => {
            let b = NumericValue::new(i128::from(*b), 0);
            a.div(&b)
                .map(Value::Numeric)
                .ok_or_else(|| DbError::internal("division by zero"))
        }
        (Value::BigInt(a), Value::Numeric(b)) => {
            let a = NumericValue::new(i128::from(*a), 0);
            a.div(b)
                .map(Value::Numeric)
                .ok_or_else(|| DbError::internal("division by zero"))
        }
        (Value::Numeric(a), Value::BigInt(b)) => {
            let b = NumericValue::new(i128::from(*b), 0);
            a.div(&b)
                .map(Value::Numeric)
                .ok_or_else(|| DbError::internal("division by zero"))
        }
        (Value::Numeric(a), Value::Double(b)) => {
            if *b == 0.0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Double(numeric_to_f64(a) / b))
        }
        (Value::Double(a), Value::Numeric(b)) => {
            let rhs = numeric_to_f64(b);
            if rhs == 0.0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Double(a / rhs))
        }
        (Value::Numeric(a), Value::Real(b)) => {
            if *b == 0.0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Double(numeric_to_f64(a) / f64::from(*b)))
        }
        (Value::Real(a), Value::Numeric(b)) => {
            let rhs = numeric_to_f64(b);
            if rhs == 0.0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Double(f64::from(*a) / rhs))
        }
        // Interval / number → Interval
        (Value::Interval(iv), num) => {
            let divisor = interval_factor(num, "divide")?;
            if divisor == 0.0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Interval(super::scale_interval(iv, 1.0 / divisor)?))
        }
        // Text / numeric: coerce text to numeric at runtime
        (Value::Money(a), Value::Text(text)) => {
            if let Ok(divisor) = parse_money_text(text) {
                if divisor == 0 {
                    return Err(DbError::internal("division by zero"));
                }
                let left = money_to_numeric(*a);
                let right = money_to_numeric(divisor);
                return Ok(Value::Double(
                    numeric_to_f64(&left) / numeric_to_f64(&right),
                ));
            }
            let coerced = super::coerce_text_to_numeric(text)?;
            eval_arith_div(&Value::Money(*a), &coerced)
        }
        (Value::Text(s), right_val) if right_val.is_numeric_coercible() => {
            let coerced = super::coerce_text_to_numeric(s)?;
            eval_arith_div(&coerced, right_val)
        }
        (left_val, Value::Text(s)) if left_val.is_numeric_coercible() => {
            let coerced = super::coerce_text_to_numeric(s)?;
            eval_arith_div(left_val, &coerced)
        }
        (Value::Text(s1), Value::Text(s2)) => {
            let c1 = super::coerce_text_to_numeric(s1)?;
            let c2 = super::coerce_text_to_numeric(s2)?;
            eval_arith_div(&c1, &c2)
        }
        // Array / scalar → element-wise division
        (Value::Array(arr), scalar) => {
            let results: DbResult<Vec<Value>> = arr
                .iter()
                .map(|elem| eval_arith_div(elem, scalar))
                .collect();
            Ok(Value::Array(results?))
        }
        _ => Err(DbError::internal(format!(
            "cannot divide {:?} and {:?}",
            left.data_type(),
            right.data_type()
        ))),
    }
}

pub(crate) fn eval_arith_mod(left: &Value, right: &Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Int(a), Value::Int(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            // checked_rem handles i32::MIN % -1 which panics with plain `%`
            // The mathematical remainder is 0 (matches PostgreSQL).
            Ok(Value::Int(a.checked_rem(*b).unwrap_or(0)))
        }
        (Value::Int(a), Value::BigInt(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            // checked_rem returns None only for MIN % -1; the
            // mathematical remainder is 0 (matches PostgreSQL).
            Ok(Value::BigInt(i64::from(*a).checked_rem(*b).unwrap_or(0)))
        }
        (Value::BigInt(a), Value::Int(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            // checked_rem returns None only for MIN % -1; the
            // mathematical remainder is 0 (matches PostgreSQL).
            Ok(Value::BigInt(a.checked_rem(i64::from(*b)).unwrap_or(0)))
        }
        (Value::BigInt(a), Value::BigInt(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            // checked_rem returns None only for MIN % -1; the
            // mathematical remainder is 0 (matches PostgreSQL).
            Ok(Value::BigInt(a.checked_rem(*b).unwrap_or(0)))
        }
        (Value::Real(a), Value::Real(b)) => {
            if *b == 0.0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Real(a % b))
        }
        (Value::Double(a), Value::Double(b)) => {
            if *b == 0.0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Double(a % b))
        }
        (Value::Int(a), Value::Real(b)) => {
            if *b == 0.0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Real(i32_to_f32(*a) % b))
        }
        (Value::Real(a), Value::Int(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Real(a % i32_to_f32(*b)))
        }
        (Value::Int(a), Value::Double(b)) => {
            if *b == 0.0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Double(f64::from(*a) % b))
        }
        (Value::Double(a), Value::Int(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Double(a % f64::from(*b)))
        }
        (Value::BigInt(a), Value::Double(b)) => {
            if *b == 0.0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Double(i64_to_f64(*a) % b))
        }
        (Value::Double(a), Value::BigInt(b)) => {
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            Ok(Value::Double(a % i64_to_f64(*b)))
        }
        // Numeric % Int/BigInt → Numeric
        (Value::Numeric(a), Value::Int(b)) => {
            // PostgreSQL: NaN % anything = NaN
            if a.is_nan() {
                return Ok(Value::Numeric(NumericValue::NAN));
            }
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            let b_n = NumericValue::new(i128::from(*b), 0);
            let quotient = a
                .div_trunc_int(&b_n)
                .ok_or_else(|| DbError::internal("division by zero"))?;
            let product = quotient.mul(&b_n).ok_or_else(numeric_out_of_range)?;
            checked_numeric_binary_value(a, &product, a.sub(&product))
        }
        (Value::Numeric(a), Value::BigInt(b)) => {
            // PostgreSQL: NaN % anything = NaN
            if a.is_nan() {
                return Ok(Value::Numeric(NumericValue::NAN));
            }
            if *b == 0 {
                return Err(DbError::internal("division by zero"));
            }
            let b_n = NumericValue::new(i128::from(*b), 0);
            let quotient = a
                .div_trunc_int(&b_n)
                .ok_or_else(|| DbError::internal("division by zero"))?;
            let product = quotient.mul(&b_n).ok_or_else(numeric_out_of_range)?;
            checked_numeric_binary_value(a, &product, a.sub(&product))
        }
        (Value::Numeric(a), Value::Numeric(b)) => {
            // PostgreSQL: NaN % anything = NaN, anything % NaN = NaN
            if a.is_nan() || b.is_nan() {
                return Ok(Value::Numeric(NumericValue::NAN));
            }
            if b.is_zero() {
                return Err(DbError::internal("division by zero"));
            }
            let quotient = a
                .div_trunc_int(b)
                .ok_or_else(|| DbError::internal("division by zero"))?;
            let product = quotient.mul(b).ok_or_else(numeric_out_of_range)?;
            checked_numeric_binary_value(a, &product, a.sub(&product))
        }
        // Text % numeric: coerce text to numeric at runtime
        (Value::Text(s), right_val) if right_val.is_numeric_coercible() => {
            let coerced = super::coerce_text_to_numeric(s)?;
            eval_arith_mod(&coerced, right_val)
        }
        (left_val, Value::Text(s)) if left_val.is_numeric_coercible() => {
            let coerced = super::coerce_text_to_numeric(s)?;
            eval_arith_mod(left_val, &coerced)
        }
        (Value::Text(s1), Value::Text(s2)) => {
            let c1 = super::coerce_text_to_numeric(s1)?;
            let c2 = super::coerce_text_to_numeric(s2)?;
            eval_arith_mod(&c1, &c2)
        }
        // Array % scalar → element-wise modulo
        (Value::Array(arr), scalar) => {
            let results: DbResult<Vec<Value>> = arr
                .iter()
                .map(|elem| eval_arith_mod(elem, scalar))
                .collect();
            Ok(Value::Array(results?))
        }
        _ => Err(DbError::internal(format!(
            "cannot modulo {:?} and {:?}",
            left.data_type(),
            right.data_type()
        ))),
    }
}

pub(crate) fn eval_negate(value: &Value) -> DbResult<Value> {
    match value {
        Value::Null => Ok(Value::Null),
        Value::Int(v) => match v.checked_neg() {
            Some(result) => Ok(Value::Int(result)),
            None => Ok(Value::BigInt(-(i64::from(*v)))),
        },
        Value::BigInt(v) => v
            .checked_neg()
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        Value::Real(v) => Ok(Value::Real(-v)),
        Value::Double(v) => Ok(Value::Double(-v)),
        Value::Numeric(v) => Ok(Value::Numeric(v.neg())),
        Value::Money(v) => v
            .checked_neg()
            .map(Value::Money)
            .ok_or_else(money_out_of_range),
        Value::Interval(iv) => {
            let months = iv.months.checked_neg();
            let days = iv.days.checked_neg();
            let micros = iv.micros.checked_neg();
            match (months, days, micros) {
                (Some(m), Some(d), Some(u)) => Ok(Value::Interval(IntervalValue::new(m, d, u))),
                _ => Err(DbError::internal("interval out of range")),
            }
        }
        // Text → coerce to numeric and negate
        Value::Text(s) => {
            let coerced = super::coerce_text_to_numeric(s)?;
            eval_negate(&coerced)
        }
        _ => Err(DbError::internal(format!(
            "cannot negate {:?}",
            value.data_type()
        ))),
    }
}
pub(crate) fn eval_arith_sub(left: &Value, right: &Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Vector(a), Value::Vector(b)) => eval_vector_pair_op(a, b, "-", |a, b| a - b),
        // PG compatibility: i32 overflow promotes to BigInt rather than error.
        (Value::Int(a), Value::Int(b)) => match a.checked_sub(*b) {
            Some(result) => Ok(Value::Int(result)),
            None => Ok(Value::BigInt(i64::from(*a) - i64::from(*b))),
        },
        (Value::Int(a), Value::BigInt(b)) => i64::from(*a)
            .checked_sub(*b)
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        (Value::BigInt(a), Value::Int(b)) => a
            .checked_sub(i64::from(*b))
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        (Value::BigInt(a), Value::BigInt(b)) => a
            .checked_sub(*b)
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        (Value::Real(a), Value::Real(b)) => {
            let result = a - b;
            if result.is_infinite() && !a.is_infinite() && !b.is_infinite() {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    "value out of range: overflow",
                )));
            }
            Ok(Value::Real(result))
        }
        (Value::Double(a), Value::Double(b)) => {
            let result = a - b;
            if result.is_infinite() && !a.is_infinite() && !b.is_infinite() {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    "value out of range: overflow",
                )));
            }
            Ok(Value::Double(result))
        }
        (Value::Real(a), Value::Double(b)) => Ok(Value::Double(f64::from(*a) - b)),
        (Value::Double(a), Value::Real(b)) => Ok(Value::Double(a - f64::from(*b))),
        (Value::Int(a), Value::Real(b)) => Ok(Value::Real(i32_to_f32(*a) - b)),
        (Value::Real(a), Value::Int(b)) => Ok(Value::Real(a - i32_to_f32(*b))),
        (Value::Int(a), Value::Double(b)) => Ok(Value::Double(f64::from(*a) - b)),
        (Value::Double(a), Value::Int(b)) => Ok(Value::Double(a - f64::from(*b))),
        (Value::BigInt(a), Value::Real(b)) => Ok(Value::Double(i64_to_f64(*a) - f64::from(*b))),
        (Value::Real(a), Value::BigInt(b)) => Ok(Value::Double(f64::from(*a) - i64_to_f64(*b))),
        (Value::BigInt(a), Value::Double(b)) => Ok(Value::Double(i64_to_f64(*a) - b)),
        (Value::Double(a), Value::BigInt(b)) => Ok(Value::Double(a - i64_to_f64(*b))),
        (Value::Money(a), Value::Money(b)) => a
            .checked_sub(*b)
            .map(Value::Money)
            .ok_or_else(money_out_of_range),
        (Value::Money(a), Value::Text(text)) => parse_money_text(text).and_then(|b| {
            a.checked_sub(b)
                .map(Value::Money)
                .ok_or_else(money_out_of_range)
        }),
        (Value::Text(text), Value::Money(b)) => parse_money_text(text).and_then(|a| {
            a.checked_sub(*b)
                .map(Value::Money)
                .ok_or_else(money_out_of_range)
        }),
        (Value::Numeric(a), Value::Numeric(b)) => checked_numeric_binary_value(a, b, a.sub(b)),
        (Value::Int(a), Value::Numeric(b)) => {
            let a = NumericValue::new(i128::from(*a), 0);
            checked_numeric_binary_value(&a, b, a.sub(b))
        }
        (Value::Numeric(a), Value::Int(b)) => {
            let b = NumericValue::new(i128::from(*b), 0);
            checked_numeric_binary_value(a, &b, a.sub(&b))
        }
        (Value::BigInt(a), Value::Numeric(b)) => {
            let a = NumericValue::new(i128::from(*a), 0);
            checked_numeric_binary_value(&a, b, a.sub(b))
        }
        (Value::Numeric(a), Value::BigInt(b)) => {
            let b = NumericValue::new(i128::from(*b), 0);
            checked_numeric_binary_value(a, &b, a.sub(&b))
        }
        (Value::PgLsn(left), Value::PgLsn(right)) => i128::from(left.raw())
            .checked_sub(i128::from(right.raw()))
            .map(|diff| Value::Numeric(NumericValue::new(diff, 0)))
            .ok_or_else(pg_lsn_out_of_range),
        (Value::PgLsn(value), offset)
            if matches!(offset, Value::Int(_) | Value::BigInt(_) | Value::Numeric(_)) =>
        {
            let delta = pg_lsn_offset_from_value(offset, "subtract")?;
            let negated = delta.checked_neg().ok_or_else(pg_lsn_out_of_range)?;
            value
                .checked_add_signed(negated)
                .map(Value::PgLsn)
                .ok_or_else(pg_lsn_out_of_range)
        }
        // Date - Date → integer (number of days)
        (Value::Date(a), Value::Date(b)) => {
            let days = (*a - *b).whole_days();
            match i32::try_from(days) {
                Ok(d) => Ok(Value::Int(d)),
                Err(_) => Err(DbError::internal("integer out of range")),
            }
        }
        // Date - Int → Date (subtract days)
        (Value::Date(d), Value::Int(n)) => {
            let result = d
                .checked_sub(time::Duration::days(i64::from(*n)))
                .ok_or_else(date_out_of_range)?;
            Ok(Value::Date(result))
        }
        // Date - Time → Timestamp
        (Value::Date(d), Value::Time(t)) => {
            let timestamp = time::PrimitiveDateTime::new(*d, time::Time::MIDNIGHT);
            let duration = time::Duration::hours(i64::from(t.hour()))
                + time::Duration::minutes(i64::from(t.minute()))
                + time::Duration::seconds(i64::from(t.second()))
                + time::Duration::microseconds(i64::from(t.microsecond()));
            Ok(Value::Timestamp(
                timestamp
                    .checked_sub(duration)
                    .ok_or_else(timestamp_out_of_range)?,
            ))
        }
        // Timestamp - Timestamp → Interval
        (Value::Timestamp(a), Value::Timestamp(b)) => {
            let diff = *a - *b;
            let micros = diff.whole_microseconds();
            if micros < i128::from(i64::MIN) || micros > i128::from(i64::MAX) {
                return Err(DbError::internal("interval out of range"));
            }
            Ok(Value::Interval(interval_from_total_micros(micros)?))
        }
        // Timestamp - Interval → Timestamp
        (Value::Timestamp(ts), Value::Interval(iv)) => {
            Ok(Value::Timestamp(sub_interval_from_timestamp(*ts, iv)?))
        }
        // Date - BigInt → Date (subtract days)
        (Value::Date(d), Value::BigInt(n)) => {
            let result = d
                .checked_sub(time::Duration::days(*n))
                .ok_or_else(date_out_of_range)?;
            Ok(Value::Date(result))
        }
        // Date - Interval → Date (Cypher semantics; only months/days).
        (Value::Date(d), Value::Interval(iv)) => Ok(Value::Date(
            super::temporal::apply_interval_calendar_to_date(*d, iv, true)?,
        )),
        // TimestampTz - TimestampTz → Interval
        (Value::TimestampTz(a), Value::TimestampTz(b)) => {
            let diff = *a - *b;
            let micros = diff.whole_microseconds();
            if micros < i128::from(i64::MIN) || micros > i128::from(i64::MAX) {
                return Err(DbError::internal("interval out of range"));
            }
            Ok(Value::Interval(interval_from_total_micros(micros)?))
        }
        // TimestampTz - Interval → TimestampTz
        (Value::TimestampTz(odt), Value::Interval(iv)) => {
            Ok(Value::TimestampTz(sub_interval_from_timestamptz(*odt, iv)?))
        }
        // TimestampTz - Timestamp → Interval (promote timestamp to timestamptz)
        (Value::TimestampTz(a), Value::Timestamp(b)) => {
            let b_tz = promote_timestamp_to_timestamptz(*b);
            let diff = *a - b_tz;
            let micros = diff.whole_microseconds();
            if micros < i128::from(i64::MIN) || micros > i128::from(i64::MAX) {
                return Err(DbError::internal("interval out of range"));
            }
            Ok(Value::Interval(interval_from_total_micros(micros)?))
        }
        // Timestamp - TimestampTz → Interval
        (Value::Timestamp(a), Value::TimestampTz(b)) => {
            let a_tz = promote_timestamp_to_timestamptz(*a);
            let diff = a_tz - *b;
            let micros = diff.whole_microseconds();
            if micros < i128::from(i64::MIN) || micros > i128::from(i64::MAX) {
                return Err(DbError::internal("interval out of range"));
            }
            Ok(Value::Interval(interval_from_total_micros(micros)?))
        }
        // Time - Time → Interval
        (Value::Time(a), Value::Time(b)) => {
            let a_micros = i64::from(a.hour()) * 3_600_000_000
                + i64::from(a.minute()) * 60_000_000
                + i64::from(a.second()) * 1_000_000
                + i64::from(a.microsecond());
            let b_micros = i64::from(b.hour()) * 3_600_000_000
                + i64::from(b.minute()) * 60_000_000
                + i64::from(b.second()) * 1_000_000
                + i64::from(b.microsecond());
            Ok(Value::Interval(IntervalValue::new(
                0,
                0,
                a_micros - b_micros,
            )))
        }
        // TimeTz - TimeTz → Interval (compare instants-of-day in UTC)
        (Value::TimeTz(a_time, a_offset), Value::TimeTz(b_time, b_offset)) => {
            Ok(Value::Interval(IntervalValue::new(
                0,
                0,
                timetz_utc_micros(a_time, a_offset) - timetz_utc_micros(b_time, b_offset),
            )))
        }
        // TimeTz - Interval → TimeTz
        (Value::TimeTz(time, offset), Value::Interval(iv)) => {
            let day_micros = i64::from(iv.days)
                .checked_mul(DAY_MICROS_I64)
                .ok_or_else(interval_out_of_range)?;
            let iv_micros = iv
                .micros
                .checked_add(day_micros)
                .ok_or_else(interval_out_of_range)?;
            Ok(Value::TimeTz(
                micros_to_time_wrapped(time_to_micros(time) - iv_micros)?,
                *offset,
            ))
        }
        // Time - Interval → Time
        (Value::Time(t), Value::Interval(iv)) => {
            let base_micros = i64::from(t.hour()) * 3_600_000_000
                + i64::from(t.minute()) * 60_000_000
                + i64::from(t.second()) * 1_000_000
                + i64::from(t.microsecond());
            let day_micros = i64::from(iv.days)
                .checked_mul(86_400_000_000)
                .ok_or_else(|| {
                    DbError::out_of_range("interval", "interval field value out of range")
                })?;
            let iv_micros = iv.micros.checked_add(day_micros).ok_or_else(|| {
                DbError::out_of_range("interval", "interval field value out of range")
            })?;
            let total =
                ((base_micros - iv_micros) % 86_400_000_000 + 86_400_000_000) % 86_400_000_000;
            Ok(Value::Time(micros_to_time_wrapped(total)?))
        }
        // Interval - Interval → Interval
        (Value::Interval(a), Value::Interval(b)) => {
            let months = a.months.checked_sub(b.months).ok_or_else(|| {
                DbError::out_of_range("interval", "interval field value out of range")
            })?;
            let days = a.days.checked_sub(b.days).ok_or_else(|| {
                DbError::out_of_range("interval", "interval field value out of range")
            })?;
            let micros = a.micros.checked_sub(b.micros).ok_or_else(|| {
                DbError::out_of_range("interval", "interval field value out of range")
            })?;
            Ok(Value::Interval(IntervalValue::new(months, days, micros)))
        }
        // Numeric - float → Double
        (Value::Numeric(a), Value::Double(b)) => Ok(Value::Double(numeric_to_f64(a) - b)),
        (Value::Double(a), Value::Numeric(b)) => Ok(Value::Double(a - numeric_to_f64(b))),
        (Value::Numeric(a), Value::Real(b)) => Ok(Value::Double(numeric_to_f64(a) - f64::from(*b))),
        (Value::Real(a), Value::Numeric(b)) => Ok(Value::Double(f64::from(*a) - numeric_to_f64(b))),
        // Boolean - numeric (PG: TRUE=1, FALSE=0)
        (Value::Boolean(a), Value::Int(b)) => i32::from(*a)
            .checked_sub(*b)
            .map(Value::Int)
            .ok_or_else(|| DbError::internal("integer out of range")),
        (Value::Int(a), Value::Boolean(b)) => a
            .checked_sub(i32::from(*b))
            .map(Value::Int)
            .ok_or_else(|| DbError::internal("integer out of range")),
        // Text - numeric: try inet-integer first (PG semantics), then fall
        // back to numeric coercion. inet text representations like
        // "127.0.0.1" never coerce as numeric, so we have to detect the
        // network shape explicitly before the numeric path.
        (Value::Text(s), right_val) if right_val.is_numeric_coercible() => {
            if let Some(result) = super::eval_network_minus_numeric(s, right_val) {
                return result;
            }
            let coerced = coerce_text_to_numeric(s)?;
            eval_arith_sub(&coerced, right_val)
        }
        (left_val, Value::Text(s)) if left_val.is_numeric_coercible() => {
            let coerced = coerce_text_to_numeric(s)?;
            eval_arith_sub(left_val, &coerced)
        }
        (Value::Text(s1), Value::Text(s2)) => {
            use crate::eval::scalar_functions::range as rng;
            // Try inet - inet first (returns BigInt diff). When it doesn't
            // apply (mixed family or unparseable), fall through to range or
            // numeric coercion.
            if let Some(result) = super::eval_network_minus_network(s1, s2) {
                return result;
            }
            if rng::looks_like_multirange(s1) || rng::looks_like_multirange(s2) {
                return rng::eval_multirange_minus(left, right);
            }
            if looks_like_range(s1) && looks_like_range(s2) {
                return rng::eval_range_difference(left, right);
            }
            let c1 = coerce_text_to_numeric(s1)?;
            let c2 = coerce_text_to_numeric(s2)?;
            eval_arith_sub(&c1, &c2)
        }
        // JSONB - text → delete key from object
        (Value::Jsonb(serde_json::Value::Object(map)), Value::Text(key)) => {
            // Only clone if the key actually exists
            if map.contains_key(key.as_str()) {
                let mut new_map = map.clone();
                new_map.remove(key);
                Ok(Value::Jsonb(serde_json::Value::Object(new_map)))
            } else {
                Ok(left.clone())
            }
        }
        // JSONB - int → delete element by index from array
        (Value::Jsonb(serde_json::Value::Array(arr)), Value::Int(idx)) => {
            let len_i64 = i64::try_from(arr.len()).unwrap_or(i64::MAX);
            let i = if *idx < 0 {
                let adjusted = len_i64 + i64::from(*idx);
                if adjusted < 0 {
                    return Ok(left.clone());
                }
                usize::try_from(adjusted).unwrap_or(usize::MAX)
            } else {
                usize::try_from(*idx).unwrap_or(usize::MAX)
            };
            if i < arr.len() {
                let mut new_arr = Vec::with_capacity(arr.len() - 1);
                new_arr.extend(arr[..i].iter().cloned());
                new_arr.extend(arr[i + 1..].iter().cloned());
                Ok(Value::Jsonb(serde_json::Value::Array(new_arr)))
            } else {
                Ok(left.clone())
            }
        }
        // JSONB - text[] → delete multiple keys
        (Value::Jsonb(serde_json::Value::Object(map)), Value::Array(keys)) => {
            // Collect keys to remove, only clone map if at least one exists
            let keys_to_remove: Vec<&str> = keys
                .iter()
                .filter_map(|k| {
                    if let Value::Text(key) = k {
                        if map.contains_key(key.as_str()) {
                            return Some(key.as_str());
                        }
                    }
                    None
                })
                .collect();
            if keys_to_remove.is_empty() {
                Ok(left.clone())
            } else {
                let mut new_map = map.clone();
                for key in keys_to_remove {
                    new_map.remove(key);
                }
                Ok(Value::Jsonb(serde_json::Value::Object(new_map)))
            }
        }
        // Array - scalar → element-wise subtraction
        (Value::Array(arr), scalar) => {
            let results: DbResult<Vec<Value>> = arr
                .iter()
                .map(|elem| eval_arith_sub(elem, scalar))
                .collect();
            Ok(Value::Array(results?))
        }
        // PG: temporal - text coerces text to interval. Mirrors the
        // matching arms in eval_arith_add. Without these, expressions like
        // `now() - '1 day'` raise "cannot subtract".
        (Value::Interval(_), Value::Text(s)) => {
            let coerced = crate::eval::cast::cast_value(
                Value::Text(s.clone()),
                &aiondb_core::DataType::Interval,
            )?;
            eval_arith_sub(left, &coerced)
        }
        (Value::Timestamp(_), Value::Text(s)) => {
            let coerced = crate::eval::cast::cast_value(
                Value::Text(s.clone()),
                &aiondb_core::DataType::Interval,
            )?;
            eval_arith_sub(left, &coerced)
        }
        (Value::TimestampTz(_), Value::Text(s)) => {
            let coerced = crate::eval::cast::cast_value(
                Value::Text(s.clone()),
                &aiondb_core::DataType::Interval,
            )?;
            eval_arith_sub(left, &coerced)
        }
        (Value::Date(_), Value::Text(s)) => {
            let coerced = crate::eval::cast::cast_value(
                Value::Text(s.clone()),
                &aiondb_core::DataType::Interval,
            )?;
            eval_arith_sub(left, &coerced)
        }
        _ => Err(DbError::internal(format!(
            "cannot subtract {:?} and {:?}",
            left.data_type(),
            right.data_type()
        ))),
    }
}

pub(crate) fn eval_arith_mul(left: &Value, right: &Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Vector(a), Value::Vector(b)) => eval_vector_pair_op(a, b, "*", |a, b| a * b),
        // PG compatibility: i32 overflow promotes to BigInt rather than error.
        (Value::Int(a), Value::Int(b)) => match a.checked_mul(*b) {
            Some(result) => Ok(Value::Int(result)),
            None => Ok(Value::BigInt(i64::from(*a) * i64::from(*b))),
        },
        (Value::Int(a), Value::BigInt(b)) => i64::from(*a)
            .checked_mul(*b)
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        (Value::BigInt(a), Value::Int(b)) => a
            .checked_mul(i64::from(*b))
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        (Value::BigInt(a), Value::BigInt(b)) => a
            .checked_mul(*b)
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        (Value::Real(a), Value::Real(b)) => {
            let result = a * b;
            if result.is_infinite() && !a.is_infinite() && !b.is_infinite() {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    "value out of range: overflow",
                )));
            }
            Ok(Value::Real(result))
        }
        (Value::Double(a), Value::Double(b)) => {
            let result = a * b;
            if result.is_infinite() && !a.is_infinite() && !b.is_infinite() {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    "value out of range: overflow",
                )));
            }
            Ok(Value::Double(result))
        }
        (Value::Int(a), Value::Real(b)) => Ok(Value::Real(i32_to_f32(*a) * b)),
        (Value::Real(a), Value::Int(b)) => Ok(Value::Real(a * i32_to_f32(*b))),
        (Value::Int(a), Value::Double(b)) => Ok(Value::Double(f64::from(*a) * b)),
        (Value::Double(a), Value::Int(b)) => Ok(Value::Double(a * f64::from(*b))),
        (Value::BigInt(a), Value::Double(b)) => Ok(Value::Double(i64_to_f64(*a) * b)),
        (Value::Double(a), Value::BigInt(b)) => Ok(Value::Double(a * i64_to_f64(*b))),
        (Value::Money(a), Value::Int(b)) => money_mul_i64(*a, i64::from(*b)).map(Value::Money),
        (Value::Int(a), Value::Money(b)) => money_mul_i64(*b, i64::from(*a)).map(Value::Money),
        (Value::Money(a), Value::BigInt(b)) => money_mul_i64(*a, *b).map(Value::Money),
        (Value::BigInt(a), Value::Money(b)) => money_mul_i64(*b, *a).map(Value::Money),
        (Value::Money(a), Value::Real(b)) => money_mul_f64(*a, f64::from(*b)).map(Value::Money),
        (Value::Real(a), Value::Money(b)) => money_mul_f64(*b, f64::from(*a)).map(Value::Money),
        (Value::Money(a), Value::Double(b)) => money_mul_f64(*a, *b).map(Value::Money),
        (Value::Double(a), Value::Money(b)) => money_mul_f64(*b, *a).map(Value::Money),
        (Value::Numeric(a), Value::Numeric(b)) => a
            .mul(b)
            .map(Value::Numeric)
            .ok_or_else(numeric_out_of_range),
        (Value::Int(a), Value::Numeric(b)) => {
            let a = NumericValue::new(i128::from(*a), 0);
            a.mul(b)
                .map(Value::Numeric)
                .ok_or_else(numeric_out_of_range)
        }
        (Value::Numeric(a), Value::Int(b)) => {
            let b = NumericValue::new(i128::from(*b), 0);
            a.mul(&b)
                .map(Value::Numeric)
                .ok_or_else(numeric_out_of_range)
        }
        (Value::BigInt(a), Value::Numeric(b)) => {
            let a = NumericValue::new(i128::from(*a), 0);
            a.mul(b)
                .map(Value::Numeric)
                .ok_or_else(numeric_out_of_range)
        }
        (Value::Numeric(a), Value::BigInt(b)) => {
            let b = NumericValue::new(i128::from(*b), 0);
            a.mul(&b)
                .map(Value::Numeric)
                .ok_or_else(numeric_out_of_range)
        }
        // Numeric * float → Double (PG coerces numeric to float for mixed ops)
        (Value::Numeric(a), Value::Double(b)) => Ok(Value::Double(numeric_to_f64(a) * b)),
        (Value::Double(a), Value::Numeric(b)) => Ok(Value::Double(a * numeric_to_f64(b))),
        (Value::Numeric(a), Value::Real(b)) => Ok(Value::Double(numeric_to_f64(a) * f64::from(*b))),
        (Value::Real(a), Value::Numeric(b)) => Ok(Value::Double(f64::from(*a) * numeric_to_f64(b))),
        // Real * BigInt → Double
        (Value::Real(a), Value::BigInt(b)) => Ok(Value::Double(f64::from(*a) * i64_to_f64(*b))),
        (Value::BigInt(a), Value::Real(b)) => Ok(Value::Double(i64_to_f64(*a) * f64::from(*b))),
        // Interval * number → Interval
        (Value::Interval(iv), num) | (num, Value::Interval(iv)) => {
            let factor = interval_factor(num, "multiply")?;
            Ok(Value::Interval(scale_interval(iv, factor)?))
        }
        // Text * numeric: coerce text to numeric at runtime
        (Value::Text(s), right_val) if right_val.is_numeric_coercible() => {
            let coerced = coerce_text_to_numeric(s)?;
            eval_arith_mul(&coerced, right_val)
        }
        (left_val, Value::Text(s)) if left_val.is_numeric_coercible() => {
            let coerced = coerce_text_to_numeric(s)?;
            eval_arith_mul(left_val, &coerced)
        }
        (Value::Text(s1), Value::Text(s2)) => {
            use crate::eval::scalar_functions::range as rng;
            if rng::looks_like_multirange(s1) || rng::looks_like_multirange(s2) {
                return rng::eval_multirange_intersect(left, right);
            }
            if rng::looks_like_range(s1) && rng::looks_like_range(s2) {
                return rng::eval_range_intersect_op(left, right);
            }
            let c1 = coerce_text_to_numeric(s1)?;
            let c2 = coerce_text_to_numeric(s2)?;
            eval_arith_mul(&c1, &c2)
        }
        // Array * scalar → element-wise multiplication
        (Value::Array(arr), scalar) => {
            let results: DbResult<Vec<Value>> = arr
                .iter()
                .map(|elem| eval_arith_mul(elem, scalar))
                .collect();
            Ok(Value::Array(results?))
        }
        (scalar, Value::Array(arr)) => {
            let results: DbResult<Vec<Value>> = arr
                .iter()
                .map(|elem| eval_arith_mul(scalar, elem))
                .collect();
            Ok(Value::Array(results?))
        }
        _ => Err(DbError::internal(format!(
            "cannot multiply {:?} and {:?}",
            left.data_type(),
            right.data_type()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modulo_subtraction_with_large_scale_preserves_numeric_result() {
        let left = NumericValue::new(12, 0);
        let product: NumericValue = format!("9.5{}", "0".repeat(1348))
            .parse()
            .expect("numeric literal must parse");

        let value = checked_numeric_binary_value(&left, &product, left.sub(&product))
            .expect("large-scale subtraction should succeed");
        let Value::Numeric(result) = value else {
            panic!("expected numeric result, got: {value:?}");
        };
        assert_eq!(result.scale, 1349);
        assert!((result.to_f64() - 2.5).abs() < 1e-12);
    }
}
