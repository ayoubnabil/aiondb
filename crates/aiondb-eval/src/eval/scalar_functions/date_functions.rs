#![allow(clippy::many_single_char_names)]

use aiondb_core::temporal::{neg_infinity_timestamptz, pos_infinity_timestamptz};
use aiondb_core::{DbError, DbResult, ErrorReport, IntervalValue, SqlState, Value};
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

use super::value_convert::{
    f64_to_i128_rounded, f64_to_i64_trunc_saturating, f64_to_u32_rounded_saturating, i64_to_f64,
    timestamp_out_of_range, value_to_i32 as to_i32,
};
use super::{expect_arg_range, expect_args, expect_text_arg, to_f64};
use crate::eval::session::current_time_zone;

fn parse_pg_format_fields(
    input: &str,
    format_text: &str,
) -> DbResult<crate::eval::pg_format::ParsedFields> {
    let format = compile_pg_format(format_text);
    apply_format(input, &format).map_err(|error| error.into_db_error(input))
}

fn expect_text_format_args<'a>(
    args: &'a [Value],
    function_name: &str,
) -> DbResult<(&'a str, &'a str)> {
    Ok((
        expect_text_arg(args, 0, function_name, "first")?,
        expect_text_arg(args, 1, function_name, "second")?,
    ))
}

fn format_fractional_input(value: f64) -> String {
    let mut text = value.to_string();
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    text
}

fn round_f64_to_u32_saturating(value: f64) -> u32 {
    f64_to_u32_rounded_saturating(value)
}

fn trunc_f64_to_i64_saturating(value: f64) -> i64 {
    f64_to_i64_trunc_saturating(value)
}

fn make_date_out_of_range(year: i32, month: i32, day: i32) -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::DatetimeFieldOverflow,
        format!("date field value out of range: {year}-{month:02}-{day:02}"),
    ))
}

fn make_time_out_of_range(hour: i32, minute: i32, second: f64) -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::DatetimeFieldOverflow,
        format!(
            "time field value out of range: {hour:02}:{minute:02}:{}",
            format_fractional_input(second)
        ),
    ))
}

fn pg_input_year_to_astronomical(year: i32) -> Result<i32, DbError> {
    match year.cmp(&0) {
        std::cmp::Ordering::Equal => Err(make_date_out_of_range(year, 1, 1)),
        std::cmp::Ordering::Less => Ok(year + 1),
        std::cmp::Ordering::Greater => Ok(year),
    }
}

fn session_now_local() -> OffsetDateTime {
    let timezone = current_time_zone();
    let now = OffsetDateTime::now_utc();
    let (offset, _) = timezone.parts_for_utc(now);
    now.to_offset(offset)
}

// =====================================================================
// current_time
// =====================================================================

pub(super) fn eval_current_time(args: &[Value]) -> DbResult<Value> {
    // Accept an optional precision argument (0-6). PostgreSQL uses it to
    // truncate fractional seconds; we accept it for compatibility but
    // return full microsecond precision.
    if args.len() > 1 {
        return Err(DbError::bind_error(
            SqlState::SyntaxError,
            "current_time() expects 0..=1 argument(s)",
        ));
    }
    let now = session_now_local();
    let mut micros = now.microsecond();
    if let Some(precision) = args.first() {
        let p = extract_precision_arg(precision)?;
        micros = truncate_micros(micros, p);
    }
    let t = Time::from_hms_micro(now.hour(), now.minute(), now.second(), micros)
        .map_err(|e| DbError::internal(format!("failed to build time: {e}")))?;
    Ok(Value::TimeTz(t, now.offset()))
}

fn extract_precision_arg(value: &Value) -> DbResult<u32> {
    let p = match value {
        Value::Int(v) => *v,
        Value::BigInt(v) => i32::try_from(*v).unwrap_or(-1),
        _ => -1,
    };
    if !(0..=6).contains(&p) {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("precision {p} must be between 0 and 6"),
        ));
    }
    u32::try_from(p).map_err(|_| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("precision {p} must be between 0 and 6"),
        )
    })
}

fn truncate_micros(micros: u32, precision: u32) -> u32 {
    if precision >= 6 {
        return micros;
    }
    let divisor = 10u32.pow(6 - precision);
    (micros / divisor) * divisor
}

// =====================================================================
// localtime  (same as current_time in our UTC-only impl)
// =====================================================================

pub(super) fn eval_localtime(args: &[Value]) -> DbResult<Value> {
    let Value::TimeTz(time, _) = eval_current_time(args)? else {
        return Err(DbError::internal(
            "current_time did not return time with time zone",
        ));
    };
    Ok(Value::Time(time))
}

// =====================================================================
// make_time(hour, min, sec)
// =====================================================================

pub(super) fn eval_make_time(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "make_time")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let hour = to_i32(&args[0])?;
    let min = to_i32(&args[1])?;
    let sec = to_f64(&args[2])?;

    if !(0..=23).contains(&hour)
        || !(0..=59).contains(&min)
        || !(0.0..60.0).contains(&sec)
        || !sec.is_finite()
    {
        return Err(make_time_out_of_range(hour, min, sec));
    }

    let hour_byte = u8::try_from(hour).map_err(|_| make_time_out_of_range(hour, min, sec))?;
    let min_byte = u8::try_from(min).map_err(|_| make_time_out_of_range(hour, min, sec))?;
    let mut whole_sec = u8::try_from(trunc_f64_to_i64_saturating(sec))
        .map_err(|_| make_time_out_of_range(hour, min, sec))?;
    let mut micros = round_f64_to_u32_saturating((sec.fract()) * 1_000_000.0);
    // Carry: rounding sec.fract()*1e6 can produce 1_000_000 (e.g. for
    // sec=59.9999995). Bubble the overflow into whole_sec rather than
    // letting `Time::from_hms_micro` reject the value.
    if micros == 1_000_000 {
        micros = 0;
        whole_sec = whole_sec
            .checked_add(1)
            .ok_or_else(|| make_time_out_of_range(hour, min, sec))?;
        if whole_sec >= 60 {
            return Err(make_time_out_of_range(hour, min, sec));
        }
    }

    let t = Time::from_hms_micro(hour_byte, min_byte, whole_sec, micros)
        .map_err(|_| make_time_out_of_range(hour, min, sec))?;

    Ok(Value::Time(t))
}

// =====================================================================
// make_date(year, month, day)
// =====================================================================

pub(super) fn eval_make_date(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "make_date")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let year = to_i32(&args[0])?;
    let month_num = to_i32(&args[1])?;
    let day = to_i32(&args[2])?;
    let astronomical_year = pg_input_year_to_astronomical(year)
        .map_err(|_| make_date_out_of_range(year, month_num, day))?;

    let month_byte =
        u8::try_from(month_num).map_err(|_| make_date_out_of_range(year, month_num, day))?;
    let month =
        Month::try_from(month_byte).map_err(|_| make_date_out_of_range(year, month_num, day))?;
    let day_byte = u8::try_from(day).map_err(|_| make_date_out_of_range(year, month_num, day))?;
    let date = Date::from_calendar_date(astronomical_year, month, day_byte)
        .map_err(|_| make_date_out_of_range(year, month_num, day))?;

    Ok(Value::Date(date))
}

// =====================================================================
// make_timestamp(year, month, day, hour, min, sec)
// =====================================================================

pub(super) fn eval_make_timestamp(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 6, "make_timestamp")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let year = to_i32(&args[0])?;
    let month_num = to_i32(&args[1])?;
    let day = to_i32(&args[2])?;
    let hour = to_i32(&args[3])?;
    let min = to_i32(&args[4])?;
    let sec = to_f64(&args[5])?;
    let astronomical_year = pg_input_year_to_astronomical(year)
        .map_err(|_| make_date_out_of_range(year, month_num, day))?;

    let month_byte =
        u8::try_from(month_num).map_err(|_| make_date_out_of_range(year, month_num, day))?;
    let month =
        Month::try_from(month_byte).map_err(|_| make_date_out_of_range(year, month_num, day))?;
    let day_byte = u8::try_from(day).map_err(|_| make_date_out_of_range(year, month_num, day))?;
    let date = Date::from_calendar_date(astronomical_year, month, day_byte)
        .map_err(|_| make_date_out_of_range(year, month_num, day))?;

    if !(0..=23).contains(&hour)
        || !(0..=59).contains(&min)
        || !(0.0..60.0).contains(&sec)
        || !sec.is_finite()
    {
        return Err(make_time_out_of_range(hour, min, sec));
    }

    let hour_byte = u8::try_from(hour).map_err(|_| make_time_out_of_range(hour, min, sec))?;
    let min_byte = u8::try_from(min).map_err(|_| make_time_out_of_range(hour, min, sec))?;
    let mut whole_sec = u8::try_from(trunc_f64_to_i64_saturating(sec))
        .map_err(|_| make_time_out_of_range(hour, min, sec))?;
    let mut micros = round_f64_to_u32_saturating((sec.fract()) * 1_000_000.0);
    if micros == 1_000_000 {
        micros = 0;
        whole_sec = whole_sec
            .checked_add(1)
            .ok_or_else(|| make_time_out_of_range(hour, min, sec))?;
        if whole_sec >= 60 {
            return Err(make_time_out_of_range(hour, min, sec));
        }
    }

    let time = Time::from_hms_micro(hour_byte, min_byte, whole_sec, micros)
        .map_err(|_| make_time_out_of_range(hour, min, sec))?;

    Ok(Value::Timestamp(PrimitiveDateTime::new(date, time)))
}

// =====================================================================
// make_interval(years, months, weeks, days, hours, mins, secs)
//   0-7 args, all default to 0
// =====================================================================

pub(super) fn eval_make_interval(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 0, 7, "make_interval requires at most 7 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    let get_int = |idx: usize| -> DbResult<i32> {
        if idx < args.len() {
            to_i32(&args[idx])
        } else {
            Ok(0)
        }
    };

    let years = get_int(0)?;
    let months_arg = get_int(1)?;
    let weeks = get_int(2)?;
    let days_arg = get_int(3)?;
    let hours = get_int(4)?;
    let mins = get_int(5)?;
    let secs: f64 = if args.len() > 6 {
        to_f64(&args[6])?
    } else {
        0.0
    };
    if !secs.is_finite() {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "interval out of range",
        )));
    }

    let interval_overflow = || {
        DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "interval out of range",
        ))
    };
    let total_months = years
        .checked_mul(12)
        .and_then(|v| v.checked_add(months_arg))
        .ok_or_else(interval_overflow)?;
    let total_days = weeks
        .checked_mul(7)
        .and_then(|v| v.checked_add(days_arg))
        .ok_or_else(interval_overflow)?;
    let secs_micros = (secs * 1_000_000.0).round();
    if !secs_micros.is_finite()
        || secs_micros < i64_to_f64(i64::MIN)
        || secs_micros > i64_to_f64(i64::MAX)
    {
        return Err(interval_overflow());
    }
    let secs_micros_i64 = trunc_f64_to_i64_saturating(secs_micros);
    let total_micros = i64::from(hours)
        .checked_mul(3_600_000_000)
        .and_then(|h| h.checked_add(i64::from(mins).checked_mul(60_000_000)?))
        .and_then(|hm| hm.checked_add(secs_micros_i64))
        .ok_or_else(interval_overflow)?;

    Ok(Value::Interval(IntervalValue::new(
        total_months,
        total_days,
        total_micros,
    )))
}

// =====================================================================
// clock_timestamp()  - current wall-clock time
// =====================================================================

pub(super) fn eval_clock_timestamp(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 0, "clock_timestamp")?;
    Ok(Value::TimestampTz(OffsetDateTime::now_utc()))
}

// =====================================================================
// statement_timestamp()  - same as clock_timestamp for our purposes
// =====================================================================

pub(super) fn eval_statement_timestamp(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 0, "statement_timestamp")?;
    Ok(Value::TimestampTz(OffsetDateTime::now_utc()))
}

// =====================================================================
// transaction_timestamp()  - same behaviour
// =====================================================================

pub(super) fn eval_transaction_timestamp(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 0, "transaction_timestamp")?;
    Ok(Value::TimestampTz(OffsetDateTime::now_utc()))
}

use crate::eval::pg_format::{
    apply_format, build_date, build_timestamp_components, compile_pg_format,
};

// =====================================================================
// to_date(text, format)
// =====================================================================

pub(super) fn eval_to_date(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "to_date")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let (s, fmt) = expect_text_format_args(args, "to_date()")?;

    let fields = parse_pg_format_fields(s, fmt)?;
    let date = build_date(&fields, s).map_err(|error| error.into_db_error(s))?;
    Ok(Value::Date(date))
}

// =====================================================================
// to_timestamp(text, format) or to_timestamp(double)
// =====================================================================

pub(super) fn eval_to_timestamp(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 1, 2, "to_timestamp() requires 1 or 2 arguments")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    if args.len() == 1 {
        // to_timestamp(epoch_seconds)
        let epoch = to_f64(&args[0])?;
        if epoch.is_nan() {
            return Err(DbError::internal("timestamp cannot be NaN"));
        }
        if epoch.is_infinite() {
            return Ok(Value::TimestampTz(if epoch.is_sign_positive() {
                pos_infinity_timestamptz()
            } else {
                neg_infinity_timestamptz()
            }));
        }
        let epoch_trunc = epoch.trunc();
        if !epoch_trunc.is_finite()
            || epoch_trunc < i64_to_f64(i64::MIN)
            || epoch_trunc > i64_to_f64(i64::MAX)
        {
            return Err(timestamp_out_of_range());
        }
        let secs = trunc_f64_to_i64_saturating(epoch_trunc);
        let nanos = f64_to_i128_rounded((epoch.fract()) * 1_000_000_000.0, timestamp_out_of_range)?;
        let nanos_i64 = i64::try_from(nanos).map_err(|_| timestamp_out_of_range())?;
        let dt = OffsetDateTime::from_unix_timestamp(secs)
            .map_err(|e| DbError::internal(format!("to_timestamp: {e}")))?;
        let dt = dt
            .checked_add(time::Duration::nanoseconds(nanos_i64))
            .ok_or_else(timestamp_out_of_range)?;
        Ok(Value::TimestampTz(dt))
    } else {
        // to_timestamp(text, format)
        let (s, fmt) = expect_text_format_args(args, "to_timestamp()")?;

        let fields = parse_pg_format_fields(s, fmt)?;
        let (timestamp, explicit_offset): (PrimitiveDateTime, Option<time::UtcOffset>) =
            build_timestamp_components(&fields, s).map_err(|error| error.into_db_error(s))?;
        let session_timezone = current_time_zone();
        let timestamp_tz: OffsetDateTime = match explicit_offset {
            Some(offset) => timestamp.assume_offset(offset),
            None => session_timezone.apply_to_local(timestamp),
        };
        Ok(Value::TimestampTz(timestamp_tz))
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{with_session_context, EvalSessionContext};

    // -----------------------------------------------------------------
    // make_date
    // -----------------------------------------------------------------

    #[test]
    fn test_make_date_basic() {
        let args = vec![Value::Int(2025), Value::Int(6), Value::Int(15)];
        let result = eval_make_date(&args).unwrap();
        let expected = Date::from_calendar_date(2025, Month::June, 15).unwrap();
        assert_eq!(result, Value::Date(expected));
    }

    #[test]
    fn test_make_date_leap_day() {
        let args = vec![Value::Int(2024), Value::Int(2), Value::Int(29)];
        let result = eval_make_date(&args).unwrap();
        let expected = Date::from_calendar_date(2024, Month::February, 29).unwrap();
        assert_eq!(result, Value::Date(expected));
    }

    #[test]
    fn test_make_date_invalid_month() {
        let args = vec![Value::Int(2025), Value::Int(13), Value::Int(1)];
        assert!(eval_make_date(&args).is_err());
    }

    #[test]
    fn test_make_date_null_propagation() {
        let args = vec![Value::Null, Value::Int(1), Value::Int(1)];
        let result = eval_make_date(&args).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn test_make_date_wrong_arg_count() {
        let args = vec![Value::Int(2025), Value::Int(1)];
        assert!(eval_make_date(&args).is_err());
    }

    // -----------------------------------------------------------------
    // make_timestamp
    // -----------------------------------------------------------------

    #[test]
    fn test_make_timestamp_basic() {
        let args = vec![
            Value::Int(2025),
            Value::Int(3),
            Value::Int(7),
            Value::Int(14),
            Value::Int(30),
            Value::Double(45.0),
        ];
        let result = eval_make_timestamp(&args).unwrap();
        let date = Date::from_calendar_date(2025, Month::March, 7).unwrap();
        let time = Time::from_hms_micro(14, 30, 45, 0).unwrap();
        assert_eq!(result, Value::Timestamp(PrimitiveDateTime::new(date, time)));
    }

    #[test]
    fn test_make_timestamp_fractional_seconds() {
        let args = vec![
            Value::Int(2025),
            Value::Int(1),
            Value::Int(1),
            Value::Int(0),
            Value::Int(0),
            Value::Double(12.345678),
        ];
        let result = eval_make_timestamp(&args).unwrap();
        let date = Date::from_calendar_date(2025, Month::January, 1).unwrap();
        let time = Time::from_hms_micro(0, 0, 12, 345678).unwrap();
        assert_eq!(result, Value::Timestamp(PrimitiveDateTime::new(date, time)));
    }

    #[test]
    fn test_make_timestamp_null_propagation() {
        let args = vec![
            Value::Int(2025),
            Value::Null,
            Value::Int(1),
            Value::Int(0),
            Value::Int(0),
            Value::Double(0.0),
        ];
        let result = eval_make_timestamp(&args).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn test_make_timestamp_wrong_arg_count() {
        let args = vec![Value::Int(2025)];
        assert!(eval_make_timestamp(&args).is_err());
    }

    // -----------------------------------------------------------------
    // make_interval
    // -----------------------------------------------------------------

    #[test]
    fn test_make_interval_all_args() {
        // make_interval(1, 2, 3, 4, 5, 6, 7.5)
        // months = 1*12 + 2 = 14
        // days   = 3*7 + 4  = 25
        // micros = 5*3_600_000_000 + 6*60_000_000 + 7_500_000
        let args = vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
            Value::Int(6),
            Value::Double(7.5),
        ];
        let result = eval_make_interval(&args).unwrap();
        let expected = IntervalValue::new(14, 25, 5 * 3_600_000_000 + 6 * 60_000_000 + 7_500_000);
        assert_eq!(result, Value::Interval(expected));
    }

    #[test]
    fn test_make_interval_no_args() {
        let args: Vec<Value> = vec![];
        let result = eval_make_interval(&args).unwrap();
        let expected = IntervalValue::new(0, 0, 0);
        assert_eq!(result, Value::Interval(expected));
    }

    #[test]
    fn test_make_interval_partial_args() {
        // make_interval(2, 6) => months = 2*12 + 6 = 30, days = 0, micros = 0
        let args = vec![Value::Int(2), Value::Int(6)];
        let result = eval_make_interval(&args).unwrap();
        let expected = IntervalValue::new(30, 0, 0);
        assert_eq!(result, Value::Interval(expected));
    }

    #[test]
    fn test_make_interval_null_propagation() {
        let args = vec![Value::Null];
        let result = eval_make_interval(&args).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn test_make_interval_too_many_args() {
        let args = vec![Value::Int(0); 8];
        assert!(eval_make_interval(&args).is_err());
    }

    // -----------------------------------------------------------------
    // clock_timestamp
    // -----------------------------------------------------------------

    #[test]
    fn test_clock_timestamp_returns_timestamptz() {
        let result = eval_clock_timestamp(&[]).unwrap();
        assert!(
            matches!(result, Value::TimestampTz(_)),
            "expected TimestampTz, got {result:?}"
        );
    }

    #[test]
    fn test_clock_timestamp_wrong_arg_count() {
        let args = vec![Value::Int(1)];
        assert!(eval_clock_timestamp(&args).is_err());
    }

    // -----------------------------------------------------------------
    // statement_timestamp
    // -----------------------------------------------------------------

    #[test]
    fn test_statement_timestamp_returns_timestamptz() {
        let result = eval_statement_timestamp(&[]).unwrap();
        assert!(
            matches!(result, Value::TimestampTz(_)),
            "expected TimestampTz, got {result:?}"
        );
    }

    #[test]
    fn test_statement_timestamp_wrong_arg_count() {
        let args = vec![Value::Int(1)];
        assert!(eval_statement_timestamp(&args).is_err());
    }

    // -----------------------------------------------------------------
    // transaction_timestamp
    // -----------------------------------------------------------------

    #[test]
    fn test_transaction_timestamp_returns_timestamptz() {
        let result = eval_transaction_timestamp(&[]).unwrap();
        assert!(
            matches!(result, Value::TimestampTz(_)),
            "expected TimestampTz, got {result:?}"
        );
    }

    #[test]
    fn test_transaction_timestamp_wrong_arg_count() {
        let args = vec![Value::Int(1)];
        assert!(eval_transaction_timestamp(&args).is_err());
    }

    // -----------------------------------------------------------------
    // to_timestamp format parsing - PG regression patterns
    // -----------------------------------------------------------------

    fn ts(s: &str, fmt: &str) -> Value {
        eval_to_timestamp(&[Value::Text(s.into()), Value::Text(fmt.into())]).unwrap()
    }

    fn extract_ts(val: &Value) -> PrimitiveDateTime {
        match val {
            Value::Timestamp(ts) => *ts,
            Value::TimestampTz(ts) => PrimitiveDateTime::new(ts.date(), ts.time()),
            other => panic!("expected timestamp value, got {other:?}"),
        }
    }

    #[test]
    fn test_to_timestamp_yyyy_fmmonth_dd() {
        // Pattern 1: '1985 January 12', 'YYYY FMMonth DD'
        let result = ts("1985 January 12", "YYYY FMMonth DD");
        let dt = extract_ts(&result);
        assert_eq!(dt.year(), 1985);
        assert_eq!(dt.month(), Month::January);
        assert_eq!(dt.day(), 12);
    }

    #[test]
    fn test_to_timestamp_yy_mon_dd_separators() {
        // PostgreSQL rejects this: separators in input, none in format.
        let result = eval_to_timestamp(&[
            Value::Text("97/Feb/16".into()),
            Value::Text("YYMonDD".into()),
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_to_timestamp_yyyymmdd_compact() {
        // Pattern 3: '19971116', 'YYYYMMDD'
        let result = ts("19971116", "YYYYMMDD");
        let dt = extract_ts(&result);
        assert_eq!(dt.year(), 1997);
        assert_eq!(dt.month(), Month::November);
        assert_eq!(dt.day(), 16);
    }

    #[test]
    fn test_to_timestamp_mmddhh24missyyyy() {
        // Pattern 4: '05121445482000', 'MMDDHH24MISSYYYY'
        let result = ts("05121445482000", "MMDDHH24MISSYYYY");
        let dt = extract_ts(&result);
        assert_eq!(dt.year(), 2000);
        assert_eq!(dt.month(), Month::May);
        assert_eq!(dt.day(), 12);
        assert_eq!(dt.hour(), 14);
        assert_eq!(dt.minute(), 45);
        assert_eq!(dt.second(), 48);
    }

    #[test]
    fn test_to_timestamp_yyyy_fmmonth_dd_fmday() {
        // Pattern 5: '2000January09Sunday', 'YYYYFMMonthDDFMDay'
        let result = ts("2000January09Sunday", "YYYYFMMonthDDFMDay");
        let dt = extract_ts(&result);
        assert_eq!(dt.year(), 2000);
        assert_eq!(dt.month(), Month::January);
        assert_eq!(dt.day(), 9);
    }

    #[test]
    fn test_to_timestamp_yy_colon_mon_colon_dd() {
        // '97/Feb/16', 'YY:Mon:DD' - colon in format, slash in input
        let result = ts("97/Feb/16", "YY:Mon:DD");
        let dt = extract_ts(&result);
        assert_eq!(dt.year(), 1997);
        assert_eq!(dt.month(), Month::February);
        assert_eq!(dt.day(), 16);
    }

    #[test]
    fn test_to_timestamp_yyyy_bc_mm_dd() {
        // '1997 AD 11 16', 'YYYY BC MM DD'
        let result = ts("1997 AD 11 16", "YYYY BC MM DD");
        let dt = extract_ts(&result);
        assert_eq!(dt.year(), 1997);
        assert_eq!(dt.month(), Month::November);
        assert_eq!(dt.day(), 16);
    }

    #[test]
    fn test_to_timestamp_yyy_mmdd() {
        // '995-1116', 'YYY-MMDD'
        let result = ts("995-1116", "YYY-MMDD");
        let dt = extract_ts(&result);
        assert_eq!(dt.year(), 995);
        assert_eq!(dt.month(), Month::November);
        assert_eq!(dt.day(), 16);
    }

    #[test]
    fn test_to_timestamp_y_mmdd() {
        // '9-1116', 'Y-MMDD'
        let result = ts("9-1116", "Y-MMDD");
        let dt = extract_ts(&result);
        assert_eq!(dt.year(), 2009);
        assert_eq!(dt.month(), Month::November);
        assert_eq!(dt.day(), 16);
    }

    #[test]
    fn test_to_timestamp_yyyy_slash_mon_dd_time() {
        // '0097/Feb/16 --> 08:14:30', 'YYYY/Mon/DD --> HH:MI:SS'
        let result = ts("0097/Feb/16 --> 08:14:30", "YYYY/Mon/DD --> HH:MI:SS");
        let dt = extract_ts(&result);
        assert_eq!(dt.year(), 97);
        assert_eq!(dt.month(), Month::February);
        assert_eq!(dt.day(), 16);
        assert_eq!(dt.hour(), 8);
        assert_eq!(dt.minute(), 14);
        assert_eq!(dt.second(), 30);
    }

    #[test]
    fn test_to_timestamp_dollar_separators() {
        // '2011$03!18 23_38_15', 'YYYY-MM-DD HH24:MI:SS'
        let result = ts("2011$03!18 23_38_15", "YYYY-MM-DD HH24:MI:SS");
        let dt = extract_ts(&result);
        assert_eq!(dt.year(), 2011);
        assert_eq!(dt.month(), Month::March);
        assert_eq!(dt.day(), 18);
        assert_eq!(dt.hour(), 23);
        assert_eq!(dt.minute(), 38);
        assert_eq!(dt.second(), 15);
    }

    #[test]
    fn test_to_timestamp_20050302() {
        // '20050302', 'YYYYMMDD'
        let result = ts("20050302", "YYYYMMDD");
        let dt = extract_ts(&result);
        assert_eq!(dt.year(), 2005);
        assert_eq!(dt.month(), Month::March);
        assert_eq!(dt.day(), 2);
    }

    #[test]
    fn test_to_timestamp_spaces_yyyymmdd() {
        // '2005 03 02', 'YYYYMMDD' - PG allows spaces in input even if not in format
        let result = ts("2005 03 02", "YYYYMMDD");
        let dt = extract_ts(&result);
        assert_eq!(dt.year(), 2005);
        assert_eq!(dt.month(), Month::March);
        assert_eq!(dt.day(), 2);
    }

    #[test]
    fn test_to_timestamp_yyyy_mon_plus_separator() {
        // '2000+   JUN', 'YYYY/MON' - + and spaces as separators
        let result = ts("2000+   JUN", "YYYY/MON");
        let dt = extract_ts(&result);
        assert_eq!(dt.year(), 2000);
        assert_eq!(dt.month(), Month::June);
    }

    #[test]
    fn test_to_timestamp_tzh_keeps_explicit_offset_for_session_display() {
        let context = EvalSessionContext::from_settings(Some("Postgres, MDY"), Some("PST8PDT"));
        let result = with_session_context(context, || ts("2000 -10", "YYYY TZH"));
        let Value::TimestampTz(ts) = result else {
            panic!("expected timestamptz");
        };
        let utc = ts.to_offset(time::UtcOffset::UTC);
        assert_eq!(
            PrimitiveDateTime::new(utc.date(), utc.time()),
            PrimitiveDateTime::new(
                Date::from_calendar_date(2000, Month::January, 1).unwrap(),
                Time::from_hms(10, 0, 0).unwrap(),
            )
        );
    }

    #[test]
    fn test_to_date_negative_year() {
        // '-44-02-01', 'YYYY-MM-DD'
        let args = vec![
            Value::Text("-44-02-01".into()),
            Value::Text("YYYY-MM-DD".into()),
        ];
        let result = eval_to_date(&args).unwrap();
        match result {
            Value::Date(d) => {
                assert_eq!(d.year(), -44);
                assert_eq!(d.month(), Month::February);
                assert_eq!(d.day(), 1);
            }
            _ => panic!("expected Date"),
        }
    }
}
