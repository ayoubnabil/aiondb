use super::value_convert::{
    value_to_i32_coercing as value_to_i32, value_to_i64_coercing as value_to_i64,
};
use super::*;

// PostgreSQL regression suites (notably join/parallel/hash-path tests) rely on
// larger synthetic ranges than 10k. Keep a hard cap to prevent runaway memory
// growth, but raise it to a value aligned with pg-regress guardrails.
const MAX_GENERATE_SERIES_ROWS: usize = 150_000;

fn zero_step_error() -> DbError {
    DbError::internal("step size cannot equal zero")
}

/// Predict how many rows a `generate_series(start, stop, step)` over
/// `i32` will produce. Used only as a pre-size hint for the output
/// vector; over-sized hints are clamped to `MAX_GENERATE_SERIES_ROWS`
/// at the call site and the actual per-iteration limit check still
/// fires.
fn predicted_int_series_rows(start: i32, stop: i32, step: i32) -> usize {
    if step == 0 {
        return 0;
    }
    let span = i64::from(stop) - i64::from(start);
    let step_i64 = i64::from(step);
    if (step_i64 > 0 && span < 0) || (step_i64 < 0 && span > 0) {
        return 0;
    }
    let count = (span / step_i64).unsigned_abs().saturating_add(1);
    usize::try_from(count).unwrap_or(usize::MAX)
}

/// `i64` analog of `predicted_int_series_rows`. Uses `i128` for the
/// span so neighbour-of-`i64::MAX` ranges don't overflow.
fn predicted_bigint_series_rows(start: i64, stop: i64, step: i64) -> usize {
    if step == 0 {
        return 0;
    }
    let span = i128::from(stop) - i128::from(start);
    let step_i128 = i128::from(step);
    if (step_i128 > 0 && span < 0) || (step_i128 < 0 && span > 0) {
        return 0;
    }
    let count = (span / step_i128).unsigned_abs().saturating_add(1);
    usize::try_from(count).unwrap_or(usize::MAX)
}

pub(super) fn eval_generate_series(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 2, 4, "generate_series() requires 2..=4 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Array(Vec::new()));
    }

    let has_numeric = args.iter().any(|v| matches!(v, Value::Numeric(_)));
    let has_timestamp = args.iter().any(|v| matches!(v, Value::Timestamp(_)));
    let has_timestamp_tz = args.iter().any(|v| matches!(v, Value::TimestampTz(_)));
    let has_bigint = args.iter().any(|v| matches!(v, Value::BigInt(_)));
    let has_interval = args.iter().any(|v| matches!(v, Value::Interval(_)));

    if has_timestamp || has_timestamp_tz || has_interval {
        return eval_generate_series_timestamp(args);
    }
    if has_numeric {
        return eval_generate_series_numeric(args);
    }
    if has_bigint {
        return eval_generate_series_bigint(args);
    }

    eval_generate_series_int(args)
}

fn eval_generate_series_int(args: &[Value]) -> DbResult<Value> {
    let start = value_to_i32(&args[0])?;
    let stop = value_to_i32(&args[1])?;
    let step = if args.len() == 3 {
        value_to_i32(&args[2])?
    } else if start <= stop {
        1
    } else {
        -1
    };

    if step == 0 {
        return Err(zero_step_error());
    }

    // Predict the row count so we pre-size the output vector.
    // Saturate at `MAX_GENERATE_SERIES_ROWS` to keep memory bounded
    // for huge ranges; the per-iteration limit check still fires.
    let predicted_rows = predicted_int_series_rows(start, stop, step);
    let mut result = Vec::with_capacity(predicted_rows.min(MAX_GENERATE_SERIES_ROWS));
    let mut current = start;
    let max_rows = MAX_GENERATE_SERIES_ROWS;

    if step > 0 {
        while current <= stop {
            if result.len() >= max_rows {
                return Err(DbError::program_limit(format!(
                    "generate_series produces too many rows (limit: {MAX_GENERATE_SERIES_ROWS})"
                )));
            }
            result.push(Value::Int(current));
            current = match current.checked_add(step) {
                Some(v) => v,
                None => break,
            };
        }
    } else {
        while current >= stop {
            if result.len() >= max_rows {
                return Err(DbError::program_limit(format!(
                    "generate_series produces too many rows (limit: {MAX_GENERATE_SERIES_ROWS})"
                )));
            }
            result.push(Value::Int(current));
            current = match current.checked_add(step) {
                Some(v) => v,
                None => break,
            };
        }
    }

    Ok(Value::Array(result))
}

fn eval_generate_series_bigint(args: &[Value]) -> DbResult<Value> {
    let start = value_to_i64(&args[0])?;
    let stop = value_to_i64(&args[1])?;
    let step = if args.len() == 3 {
        value_to_i64(&args[2])?
    } else if start <= stop {
        1
    } else {
        -1
    };

    if step == 0 {
        return Err(zero_step_error());
    }

    // Predict the row count (saturating) so we pre-size the output.
    let predicted_rows = predicted_bigint_series_rows(start, stop, step);
    let mut result = Vec::with_capacity(predicted_rows.min(MAX_GENERATE_SERIES_ROWS));
    let mut current = start;
    let max_rows = MAX_GENERATE_SERIES_ROWS;

    if step > 0 {
        while current <= stop {
            if result.len() >= max_rows {
                return Err(DbError::program_limit(format!(
                    "generate_series produces too many rows (limit: {MAX_GENERATE_SERIES_ROWS})"
                )));
            }
            result.push(Value::BigInt(current));
            current = match current.checked_add(step) {
                Some(v) => v,
                None => break,
            };
        }
    } else {
        while current >= stop {
            if result.len() >= max_rows {
                return Err(DbError::program_limit(format!(
                    "generate_series produces too many rows (limit: {MAX_GENERATE_SERIES_ROWS})"
                )));
            }
            result.push(Value::BigInt(current));
            current = match current.checked_add(step) {
                Some(v) => v,
                None => break,
            };
        }
    }

    Ok(Value::Array(result))
}

fn eval_generate_series_numeric(args: &[Value]) -> DbResult<Value> {
    use aiondb_core::NumericValue;

    let start = value_to_numeric(&args[0])?;
    let stop = value_to_numeric(&args[1])?;
    let step = if args.len() == 3 {
        value_to_numeric(&args[2])?
    } else if start <= stop {
        NumericValue::new(1, 0)
    } else {
        NumericValue::new(-1, 0)
    };

    // PostgreSQL validates NaN and infinity for numeric generate_series
    if start.is_nan() {
        return Err(DbError::internal("start value cannot be NaN"));
    }
    if stop.is_nan() {
        return Err(DbError::internal("stop value cannot be NaN"));
    }
    if step.is_nan() {
        return Err(DbError::internal("step size cannot be NaN"));
    }
    if start.is_infinite() {
        return Err(DbError::internal("start value cannot be infinity"));
    }
    if stop.is_infinite() {
        return Err(DbError::internal("stop value cannot be infinity"));
    }
    if step.is_infinite() {
        return Err(DbError::internal("step size cannot be infinity"));
    }

    if step.is_zero() {
        return Err(zero_step_error());
    }

    let mut result = Vec::new();
    let mut current = start;
    let zero = NumericValue::new(0, 0);
    let max_rows = MAX_GENERATE_SERIES_ROWS;

    if step > zero {
        while current <= stop {
            if result.len() >= max_rows {
                return Err(DbError::program_limit(format!(
                    "generate_series produces too many rows (limit: {MAX_GENERATE_SERIES_ROWS})"
                )));
            }
            result.push(Value::Numeric(current.clone()));
            current = current.add(&step);
        }
    } else {
        while current >= stop {
            if result.len() >= max_rows {
                return Err(DbError::program_limit(format!(
                    "generate_series produces too many rows (limit: {MAX_GENERATE_SERIES_ROWS})"
                )));
            }
            result.push(Value::Numeric(current.clone()));
            current = current.add(&step);
        }
    }

    Ok(Value::Array(result))
}

fn eval_generate_series_timestamp(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(
        args,
        3,
        4,
        "generate_series with timestamp requires 3 or 4 arguments",
    )?;

    let step = match &args[2] {
        Value::Interval(iv) => iv.clone(),
        _ => {
            return Err(DbError::internal(
                "third argument of timestamp generate_series must be an interval",
            ));
        }
    };

    if step.months == 0 && step.days == 0 && step.micros == 0 {
        return Err(zero_step_error());
    }

    let max_rows = MAX_GENERATE_SERIES_ROWS;

    match (&args[0], &args[1]) {
        (Value::Timestamp(start), Value::Timestamp(stop)) => {
            Ok(eval_generate_series_ts(*start, *stop, &step, max_rows))
        }
        (Value::TimestampTz(start), Value::TimestampTz(stop)) => {
            Ok(eval_generate_series_tstz(*start, *stop, &step, max_rows))
        }
        (Value::Timestamp(start), Value::TimestampTz(stop)) => {
            let stop_ts = PrimitiveDateTime::new(stop.date(), stop.time());
            Ok(eval_generate_series_ts(*start, stop_ts, &step, max_rows))
        }
        (Value::TimestampTz(start), Value::Timestamp(stop)) => {
            let start_ts = PrimitiveDateTime::new(start.date(), start.time());
            Ok(eval_generate_series_ts(start_ts, *stop, &step, max_rows))
        }
        // PG also exposes generate_series(date, date [, interval]) — same
        // shape, but rendered as date rows. Most ORMs that emit calendar
        // ranges hand us dates rather than timestamps.
        (Value::Date(start), Value::Date(stop)) => {
            let start_ts = PrimitiveDateTime::new(*start, time::Time::MIDNIGHT);
            let stop_ts = PrimitiveDateTime::new(*stop, time::Time::MIDNIGHT);
            let Value::Array(rows) = eval_generate_series_ts(start_ts, stop_ts, &step, max_rows)
            else {
                return Err(DbError::internal(
                    "generate_series produced unexpected non-array result",
                ));
            };
            let date_rows: Vec<Value> = rows
                .into_iter()
                .map(|row| match row {
                    Value::Timestamp(dt) => Value::Date(dt.date()),
                    other => other,
                })
                .collect();
            Ok(Value::Array(date_rows))
        }
        (Value::Date(start), Value::Timestamp(stop)) => {
            let start_ts = PrimitiveDateTime::new(*start, time::Time::MIDNIGHT);
            Ok(eval_generate_series_ts(start_ts, *stop, &step, max_rows))
        }
        (Value::Timestamp(start), Value::Date(stop)) => {
            let stop_ts = PrimitiveDateTime::new(*stop, time::Time::MIDNIGHT);
            Ok(eval_generate_series_ts(*start, stop_ts, &step, max_rows))
        }
        _ => Err(DbError::internal(
            "generate_series requires timestamp arguments with interval step",
        )),
    }
}

fn eval_generate_series_ts(
    start: PrimitiveDateTime,
    stop: PrimitiveDateTime,
    step: &IntervalValue,
    max_rows: usize,
) -> Value {
    let mut result = Vec::new();
    let mut current = start;
    let forward = is_positive_interval(step);

    if forward {
        while current <= stop {
            if result.len() >= max_rows {
                break;
            }
            result.push(Value::Timestamp(current));
            let Some(next) = add_interval_to_ts(current, step) else {
                break;
            };
            if next <= current {
                break; // step did not advance; avoid infinite loop
            }
            current = next;
        }
    } else {
        while current >= stop {
            if result.len() >= max_rows {
                break;
            }
            result.push(Value::Timestamp(current));
            let Some(next) = add_interval_to_ts(current, step) else {
                break;
            };
            if next >= current {
                break; // step did not advance; avoid infinite loop
            }
            current = next;
        }
    }

    Value::Array(result)
}

fn eval_generate_series_tstz(
    start: OffsetDateTime,
    stop: OffsetDateTime,
    step: &IntervalValue,
    max_rows: usize,
) -> Value {
    let mut result = Vec::new();
    let mut current = start;
    let forward = is_positive_interval(step);

    if forward {
        while current <= stop {
            if result.len() >= max_rows {
                break;
            }
            result.push(Value::TimestampTz(current));
            let Some(next) = add_interval_to_tstz(current, step) else {
                break;
            };
            if next <= current {
                break; // step did not advance; avoid infinite loop
            }
            current = next;
        }
    } else {
        while current >= stop {
            if result.len() >= max_rows {
                break;
            }
            result.push(Value::TimestampTz(current));
            let Some(next) = add_interval_to_tstz(current, step) else {
                break;
            };
            if next >= current {
                break; // step did not advance; avoid infinite loop
            }
            current = next;
        }
    }

    Value::Array(result)
}

fn is_positive_interval(iv: &IntervalValue) -> bool {
    // Approximate: use 30 days/month for direction detection.  This matches
    // PostgreSQL's approach in `generate_series_timestamptz_internal` which
    // also uses a simple sign check on the interval components.
    let total_micros = i128::from(iv.months) * 30 * 24 * 3_600_000_000i128
        + i128::from(iv.days) * 24 * 3_600_000_000i128
        + i128::from(iv.micros);
    total_micros > 0
}

fn add_interval_to_ts(ts: PrimitiveDateTime, iv: &IntervalValue) -> Option<PrimitiveDateTime> {
    let mut date = ts.date();
    let time_val = ts.time();

    if iv.months != 0 {
        let total_months = date
            .year()
            .saturating_mul(12)
            .saturating_add(i32::from(u8::from(date.month())) - 1)
            .saturating_add(iv.months);
        let new_year = total_months.div_euclid(12);
        let new_month = u8::try_from(total_months.rem_euclid(12) + 1).ok()?;
        let month = time::Month::try_from(new_month).ok()?;
        let max_day = days_in_month(new_year, new_month);
        let day = date.day().min(max_day);
        date = Date::from_calendar_date(new_year, month, day).ok()?;
    }

    if iv.days != 0 {
        date = date.checked_add(time::Duration::days(i64::from(iv.days)))?;
    }

    if iv.micros != 0 {
        let nanos = iv.micros.checked_mul(1000)?;
        let duration = time::Duration::nanoseconds(nanos);
        let dt = PrimitiveDateTime::new(date, time_val).checked_add(duration)?;
        return Some(dt);
    }

    Some(PrimitiveDateTime::new(date, time_val))
}

fn add_interval_to_tstz(ts: OffsetDateTime, iv: &IntervalValue) -> Option<OffsetDateTime> {
    let offset = ts.offset();
    let pdt = PrimitiveDateTime::new(ts.date(), ts.time());
    let result = add_interval_to_ts(pdt, iv)?;
    Some(result.assume_offset(offset))
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

fn value_to_numeric(v: &Value) -> DbResult<aiondb_core::NumericValue> {
    use aiondb_core::NumericValue;

    match v {
        Value::Numeric(n) => Ok(n.clone()),
        Value::Int(n) => Ok(NumericValue::from_i32(*n)),
        Value::BigInt(n) => Ok(NumericValue::from_i64(*n)),
        Value::Real(n) => {
            let s = format!("{n}");
            s.parse::<NumericValue>()
                .map_err(|e| DbError::internal(format!("cannot convert real to numeric: {e}")))
        }
        Value::Double(n) => {
            let s = format!("{n}");
            s.parse::<NumericValue>()
                .map_err(|e| DbError::internal(format!("cannot convert double to numeric: {e}")))
        }
        Value::Text(s) => s
            .trim()
            .parse::<NumericValue>()
            .map_err(|e| DbError::internal(format!("cannot convert '{s}' to numeric: {e}"))),
        _ => Err(DbError::internal(
            "generate_series: unsupported argument type",
        )),
    }
}
