#![allow(clippy::doc_markdown)]

use super::pg_num_format::pg_format_number;
use super::value_convert::{i128_to_f64, i64_to_f64};
use super::*;
use aiondb_core::temporal::{
    is_infinity_date, neg_infinity_timestamptz, pos_infinity_timestamptz, TimeZoneSetting,
};

fn expect_lowercase_text_arg(
    args: &[Value],
    index: usize,
    function_name: &str,
    position: &str,
) -> DbResult<String> {
    Ok(expect_text_arg(args, index, function_name, position)?.to_lowercase())
}

fn timestamp_out_of_range_error() -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::NumericValueOutOfRange,
        "timestamp out of range",
    ))
}

fn checked_to_offset(
    value: OffsetDateTime,
    offset: time::UtcOffset,
    context: &str,
) -> DbResult<OffsetDateTime> {
    value
        .checked_to_offset(offset)
        .ok_or_else(|| timestamp_out_of_range_error().with_client_detail(context))
}

pub(super) fn eval_now(args: &[Value]) -> DbResult<Value> {
    // now() accepts no arguments; current_timestamp accepts an optional
    // precision argument (0-6).  Both share this entry point - just
    // accept 0 or 1 args.
    if args.len() > 1 {
        return Err(DbError::bind_error(
            SqlState::SyntaxError,
            "function expects 0..=1 argument(s)",
        ));
    }
    Ok(Value::TimestampTz(OffsetDateTime::now_utc()))
}

pub(super) fn eval_current_date(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 0, "current_date")?;
    let now = OffsetDateTime::now_utc();
    let date = Date::from_calendar_date(now.year(), now.month(), now.day())
        .map_err(|e| DbError::internal(format!("failed to build date: {e}")))?;
    Ok(Value::Date(date))
}

pub(super) fn eval_date_part(args: &[Value]) -> DbResult<Value> {
    eval_date_part_inner(args, false)
}

pub(super) fn eval_extract(args: &[Value]) -> DbResult<Value> {
    eval_date_part_inner(args, true)
}

fn eval_date_part_inner(args: &[Value], preserve_extract_scale: bool) -> DbResult<Value> {
    expect_args(args, 2, "date_part")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let field = expect_lowercase_text_arg(args, 0, "date_part()", "first")?;
    let source_is_date = matches!(&args[1], Value::Date(_));
    let value = match &args[1] {
        Value::Timestamp(dt) => extract_from_timestamp(&field, dt),
        Value::TimestampTz(odt) => {
            let utc = checked_to_offset(*odt, time::UtcOffset::UTC, "date_part")?;
            let dt = PrimitiveDateTime::new(utc.date(), utc.time());
            if field == "timezone" || field == "timezone_hour" || field == "timezone_minute" {
                let offset_secs = odt.offset().whole_seconds();
                match field.as_str() {
                    "timezone" => Ok(Value::Double(f64::from(offset_secs))),
                    "timezone_hour" => Ok(Value::Double(f64::from(offset_secs / 3600))),
                    "timezone_minute" => Ok(Value::Double(f64::from((offset_secs % 3600) / 60))),
                    _ => Err(DbError::internal(format!(
                        "unknown timezone field: {field}"
                    ))),
                }
            } else {
                extract_from_timestamp(&field, &dt)
            }
        }
        Value::Date(d) => extract_from_date(&field, d),
        Value::Time(t) => extract_from_time(&field, t),
        Value::TimeTz(time, offset) => extract_from_timetz(&field, time, offset),
        Value::Interval(iv) => extract_from_interval(&field, iv),
        Value::Text(s) => {
            // Try to parse the text as a timestamp/date/time/interval
            use super::super::cast::cast_value;
            use aiondb_core::DataType;
            if let Ok(Value::Timestamp(ts)) =
                cast_value(Value::Text(s.clone()), &DataType::Timestamp)
            {
                return extract_from_timestamp(&field, &ts);
            }
            if let Ok(Value::Date(d)) = cast_value(Value::Text(s.clone()), &DataType::Date) {
                return extract_from_date(&field, &d);
            }
            if let Ok(Value::Time(t)) = cast_value(Value::Text(s.clone()), &DataType::Time) {
                return extract_from_time(&field, &t);
            }
            if let Ok(Value::TimeTz(time, offset)) =
                cast_value(Value::Text(s.clone()), &DataType::TimeTz)
            {
                return extract_from_timetz(&field, &time, &offset);
            }
            if let Ok(Value::Interval(iv)) = cast_value(Value::Text(s.clone()), &DataType::Interval)
            {
                return extract_from_interval(&field, &iv);
            }
            Err(DbError::internal(
                "date_part() second arg must be timestamp, date, time, or interval",
            ))
        }
        _ => Err(DbError::internal(
            "date_part() second arg must be timestamp, date, time, or interval",
        )),
    }?;

    if preserve_extract_scale {
        extract_value_to_numeric(&field, value, source_is_date)
    } else {
        Ok(value)
    }
}

fn century_from_year(y: i32) -> f64 {
    let y = pg_display_year(y);
    if y > 0 {
        f64::from((y - 1) / 100 + 1)
    } else {
        -f64::from(((-y - 1) / 100) + 1)
    }
}

fn millennium_from_year(y: i32) -> f64 {
    let y = pg_display_year(y);
    if y > 0 {
        f64::from((y - 1) / 1000 + 1)
    } else {
        -f64::from(((-y - 1) / 1000) + 1)
    }
}

fn decade_from_year(y: i32) -> f64 {
    f64::from(y.div_euclid(10))
}

fn quarter_from_month(m: u8) -> f64 {
    f64::from(((m - 1) / 3) + 1)
}

fn iso_year(d: &Date) -> i32 {
    pg_display_year(d.to_iso_week_date().0)
}

fn pg_display_year(year: i32) -> i32 {
    if year <= 0 {
        year - 1
    } else {
        year
    }
}

fn astronomical_year_from_display(display_year: i32) -> i32 {
    match display_year.cmp(&0) {
        std::cmp::Ordering::Greater => display_year,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Less => display_year + 1,
    }
}

fn trunc_decade_display_year(display_year: i32) -> i32 {
    if display_year >= 0 {
        (display_year / 10) * 10
    } else {
        (display_year + 1).div_euclid(10) * 10 - 1
    }
}

fn trunc_century_display_year(display_year: i32) -> i32 {
    if display_year > 0 {
        ((display_year - 1) / 100) * 100 + 1
    } else {
        display_year.div_euclid(100) * 100
    }
}

fn trunc_millennium_display_year(display_year: i32) -> i32 {
    if display_year > 0 {
        ((display_year - 1) / 1000) * 1000 + 1
    } else {
        display_year.div_euclid(1000) * 1000
    }
}

fn date_part_not_supported(field: &str) -> DbError {
    DbError::internal(format!("unit \"{field}\" not supported for type date"))
}

fn date_part_not_recognized(field: &str) -> DbError {
    DbError::internal(format!("unit \"{field}\" not recognized for type date"))
}

fn interval_part_not_supported(field: &str) -> DbError {
    DbError::internal(format!("unit \"{field}\" not supported for type interval"))
}

fn interval_part_not_recognized(field: &str) -> DbError {
    DbError::internal(format!("unit \"{field}\" not recognized for type interval"))
}

fn extract_value_to_numeric(field: &str, value: Value, source_is_date: bool) -> DbResult<Value> {
    let scale = match field {
        "millisecond" | "milliseconds" | "msec" | "msecs" => 3,
        // DATE has no time component, so EPOCH from DATE is always a whole number.
        "epoch" if source_is_date => 0,
        "second" | "seconds" | "epoch" => 6,
        _ => 0,
    };

    match value {
        Value::Double(v) => {
            if v.is_nan() {
                return Ok(Value::Numeric(NumericValue::NAN));
            }
            if v.is_infinite() {
                return Ok(Value::Numeric(if v.is_sign_positive() {
                    NumericValue::INFINITY
                } else {
                    NumericValue::NEG_INFINITY
                }));
            }
            let text = if scale == 0 {
                format!("{v:.0}")
            } else {
                format!("{v:.scale$}")
            };
            let numeric = text
                .parse::<NumericValue>()
                .map_err(|_| DbError::invalid_input_syntax("numeric", &text))?;
            Ok(Value::Numeric(numeric))
        }
        other => Ok(other),
    }
}

fn zero_year_boundary() -> Option<Date> {
    Date::from_calendar_date(0, time::Month::January, 1).ok()
}

fn is_positive_infinity_marker(date: Date) -> bool {
    if let Some(boundary) = zero_year_boundary() {
        date > boundary
    } else {
        date.year() >= 0
    }
}

fn infinity_value_for_date_part(field: &str, date: &Date) -> Option<Value> {
    let infinity = if is_positive_infinity_marker(*date) {
        f64::INFINITY
    } else {
        f64::NEG_INFINITY
    };

    match field {
        "day" | "month" | "quarter" | "week" | "dow" | "dayofweek" | "isodow" | "doy"
        | "dayofyear" => Some(Value::Null),
        "year" | "decade" | "century" | "millennium" | "julian" | "isoyear" | "epoch" => {
            Some(Value::Double(infinity))
        }
        _ => None,
    }
}

fn extract_from_timestamp(field: &str, dt: &PrimitiveDateTime) -> DbResult<Value> {
    let result = match field {
        "year" => f64::from(pg_display_year(dt.year())),
        "month" => f64::from(u8::from(dt.month())),
        "day" => f64::from(dt.day()),
        "hour" => f64::from(dt.hour()),
        "minute" => f64::from(dt.minute()),
        "second" | "seconds" => f64::from(dt.second()) + f64::from(dt.microsecond()) / 1_000_000.0,
        "dow" | "dayofweek" => f64::from(dt.weekday().number_days_from_sunday()),
        "doy" | "dayofyear" => f64::from(dt.ordinal()),
        "century" => century_from_year(dt.year()),
        "millennium" => millennium_from_year(dt.year()),
        "decade" => decade_from_year(dt.year()),
        "quarter" => quarter_from_month(u8::from(dt.month())),
        "week" => f64::from(dt.date().to_iso_week_date().1),
        "isoyear" => f64::from(pg_display_year(iso_year(&dt.date()))),
        "isodow" => f64::from(dt.weekday().number_days_from_monday() + 1),
        "microsecond" | "microseconds" | "usec" | "usecs" => {
            (f64::from(dt.second()) * 1_000_000.0) + f64::from(dt.microsecond())
        }
        "millisecond" | "milliseconds" | "msec" | "msecs" => {
            (f64::from(dt.second()) * 1_000.0) + f64::from(dt.microsecond()) / 1_000.0
        }
        "timezone" | "timezone_hour" | "timezone_minute" => 0.0,
        "julian" => {
            let d = dt.date();
            let y = d.year();
            // month() → u8 value in [1, 12]; day() → u8 in [1, 31]: both fit i32.
            let m = i32::from(u8::from(d.month()));
            let day = i32::from(d.day());
            let mut jdn = if m > 2 {
                365 * y + y / 4 - y / 100 + y / 400 + (153 * (m - 3) + 2) / 5 + day + 1721119
            } else {
                let y2 = y - 1;
                365 * y2 + y2 / 4 - y2 / 100 + y2 / 400 + (153 * (m + 9) + 2) / 5 + day + 1721119
            };
            if y <= 0 {
                jdn -= 1;
            }
            // Add fractional day from time
            let frac = (f64::from(dt.hour()) * 3600.0
                + f64::from(dt.minute()) * 60.0
                + f64::from(dt.second())
                + f64::from(dt.microsecond()) / 1_000_000.0)
                / 86400.0;
            f64::from(jdn) + frac
        }
        "epoch" => {
            // Seconds since 1970-01-01 00:00:00 UTC
            let unix_date = Date::from_calendar_date(1970, time::Month::January, 1)
                .map_err(|e| DbError::internal(format!("date error: {e}")))?;
            let unix_epoch = PrimitiveDateTime::new(unix_date, Time::MIDNIGHT);
            let diff = *dt - unix_epoch;
            diff.as_seconds_f64()
        }
        _ => {
            return Err(DbError::internal(format!(
                "date_part: unknown field \"{field}\""
            )));
        }
    };
    Ok(Value::Double(result))
}

fn extract_from_date(field: &str, d: &Date) -> DbResult<Value> {
    if is_infinity_date(*d) {
        if let Some(value) = infinity_value_for_date_part(field, d) {
            return Ok(value);
        }
    }

    let result = match field {
        "year" => f64::from(pg_display_year(d.year())),
        "month" => f64::from(u8::from(d.month())),
        "day" => f64::from(d.day()),
        "dow" | "dayofweek" => f64::from(d.weekday().number_days_from_sunday()),
        "doy" | "dayofyear" => f64::from(d.ordinal()),
        "century" => century_from_year(d.year()),
        "millennium" => millennium_from_year(d.year()),
        "decade" => decade_from_year(d.year()),
        "quarter" => quarter_from_month(u8::from(d.month())),
        "week" => f64::from(d.to_iso_week_date().1),
        "isoyear" => f64::from(iso_year(d)),
        "isodow" => f64::from(d.weekday().number_days_from_monday() + 1),
        "hour" | "hours" | "minute" | "minutes" | "second" | "seconds" | "microsecond"
        | "microseconds" | "usec" | "usecs" | "millisecond" | "milliseconds" | "msec" | "msecs"
        | "timezone" | "timezone_h" | "timezone_hour" | "timezone_m" | "timezone_minute" => {
            return Err(date_part_not_supported(field));
        }
        "julian" => {
            // Julian day number: days since 4713 BC January 1 (Julian calendar)
            // PostgreSQL formula: date2j(y,m,d)
            let y = d.year();
            // month() → u8 in [1, 12]; day() → u8 in [1, 31]: both fit i32.
            let m = i32::from(u8::from(d.month()));
            let day = i32::from(d.day());
            let mut jdn = if m > 2 {
                365 * y + y / 4 - y / 100 + y / 400 + (153 * (m - 3) + 2) / 5 + day + 1721119
            } else {
                let y2 = y - 1;
                365 * y2 + y2 / 4 - y2 / 100 + y2 / 400 + (153 * (m + 9) + 2) / 5 + day + 1721119
            };
            if y <= 0 {
                jdn -= 1;
            }
            f64::from(jdn)
        }
        "epoch" => {
            let unix_date = Date::from_calendar_date(1970, time::Month::January, 1)
                .map_err(|e| DbError::internal(format!("date error: {e}")))?;
            let diff = *d - unix_date;
            i64_to_f64(diff.whole_days()) * 86400.0
        }
        "microsec" => return Err(date_part_not_recognized(field)),
        _ => return Err(date_part_not_recognized(field)),
    };
    Ok(Value::Double(result))
}

fn time_part_not_supported(field: &str) -> DbError {
    DbError::internal(format!(
        "unit \"{field}\" not supported for type time without time zone"
    ))
}

fn time_part_not_recognized(field: &str) -> DbError {
    DbError::internal(format!(
        "unit \"{field}\" not recognized for type time without time zone"
    ))
}

fn extract_from_time(field: &str, t: &Time) -> DbResult<Value> {
    let result = match field {
        "hour" => f64::from(t.hour()),
        "minute" => f64::from(t.minute()),
        "second" | "seconds" => f64::from(t.second()) + f64::from(t.microsecond()) / 1_000_000.0,
        "microsecond" | "microseconds" | "usec" | "usecs" => {
            (f64::from(t.second()) * 1_000_000.0) + f64::from(t.microsecond())
        }
        "millisecond" | "milliseconds" | "msec" | "msecs" => {
            (f64::from(t.second()) * 1_000.0) + f64::from(t.microsecond()) / 1_000.0
        }
        "epoch" => {
            f64::from(t.hour()) * 3600.0
                + f64::from(t.minute()) * 60.0
                + f64::from(t.second())
                + f64::from(t.microsecond()) / 1_000_000.0
        }
        "year" | "month" | "day" | "dow" | "dayofweek" | "doy" | "dayofyear" | "century"
        | "millennium" | "decade" | "quarter" | "week" | "isoyear" | "isodow" | "julian"
        | "timezone" | "timezone_hour" | "timezone_minute" => {
            return Err(time_part_not_supported(field));
        }
        _ => return Err(time_part_not_recognized(field)),
    };
    Ok(Value::Double(result))
}

fn extract_from_timetz(field: &str, time: &Time, offset: &time::UtcOffset) -> DbResult<Value> {
    let offset_secs = offset.whole_seconds();
    let utc_epoch = {
        let local = f64::from(time.hour()) * 3600.0
            + f64::from(time.minute()) * 60.0
            + f64::from(time.second())
            + f64::from(time.microsecond()) / 1_000_000.0;
        let utc = local - f64::from(offset_secs);
        utc.rem_euclid(86_400.0)
    };
    let result = match field {
        "timezone" => f64::from(offset_secs),
        "timezone_hour" => f64::from(offset_secs / 3600),
        "timezone_minute" => f64::from((offset_secs % 3600) / 60),
        "hour" => f64::from(time.hour()),
        "minute" => f64::from(time.minute()),
        "second" | "seconds" => {
            f64::from(time.second()) + f64::from(time.microsecond()) / 1_000_000.0
        }
        "microsecond" | "microseconds" | "usec" | "usecs" => {
            (f64::from(time.second()) * 1_000_000.0) + f64::from(time.microsecond())
        }
        "millisecond" | "milliseconds" | "msec" | "msecs" => {
            (f64::from(time.second()) * 1_000.0) + f64::from(time.microsecond()) / 1_000.0
        }
        "epoch" => utc_epoch,
        "year" | "month" | "day" | "dow" | "dayofweek" | "doy" | "dayofyear" | "century"
        | "millennium" | "decade" | "quarter" | "week" | "isoyear" | "isodow" | "julian" => {
            return Err(DbError::internal(format!(
                "unit \"{field}\" not supported for type time with time zone"
            )));
        }
        _ => {
            return Err(DbError::internal(format!(
                "unit \"{field}\" not recognized for type time with time zone"
            )));
        }
    };
    Ok(Value::Double(result))
}

fn extract_from_interval(field: &str, iv: &IntervalValue) -> DbResult<Value> {
    let result = match field {
        "year" => f64::from(iv.months / 12),
        "month" => f64::from(iv.months % 12),
        "day" => f64::from(iv.days),
        // `iv.micros` is `i64`; dividing by 3_600_000_000 can exceed i32::MAX for
        // extreme intervals (> ~2_562_047 hours), so keep as i64 then widen to f64.
        "hour" => i64_to_f64(iv.micros / 3_600_000_000),
        // The minute remainder is always in [0, 59], so the i32 cast would be safe,
        // but staying as i64 → f64 is equally correct and avoids any narrowing.
        "minute" => i64_to_f64((iv.micros % 3_600_000_000) / 60_000_000),
        "second" | "seconds" => {
            let remainder_micros = iv.micros % 60_000_000;
            i64_to_f64(remainder_micros) / 1_000_000.0
        }
        "microsecond" | "microseconds" | "usec" | "usecs" => {
            let remainder_micros = iv.micros % 60_000_000;
            i64_to_f64(remainder_micros)
        }
        "millisecond" | "milliseconds" | "msec" | "msecs" => {
            let remainder_micros = iv.micros % 60_000_000;
            i64_to_f64(remainder_micros) / 1_000.0
        }
        "decade" => f64::from(iv.months / 12 / 10),
        "century" => f64::from(iv.months / 12 / 100),
        "millennium" => f64::from(iv.months / 12 / 1000),
        "quarter" => f64::from((iv.months % 12) / 3 + 1),
        "week" => f64::from(iv.days / 7),
        "epoch" => {
            let years = iv.months / 12;
            let months = iv.months % 12;
            f64::from(years) * 31_557_600.0
                + f64::from(months) * 2_592_000.0
                + f64::from(iv.days) * 86_400.0
                + i64_to_f64(iv.micros) / 1_000_000.0
        }
        "timezone" | "timezone_hour" | "timezone_minute" => {
            return Err(interval_part_not_supported(field));
        }
        _ => return Err(interval_part_not_recognized(field)),
    };
    Ok(Value::Double(result))
}

pub(super) fn eval_date_trunc(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 2, 3, "date_trunc() requires 2 or 3 arguments")?;
    // Third argument (timezone) is accepted but currently ignored.
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let field = expect_lowercase_text_arg(args, 0, "date_trunc()", "first")?;
    match &args[1] {
        Value::Timestamp(dt) => trunc_timestamp(&field, dt),
        Value::TimestampTz(odt) => {
            // Convert to UTC, truncate, return as timestamptz
            let utc = checked_to_offset(*odt, time::UtcOffset::UTC, "date_trunc")?;
            let dt = PrimitiveDateTime::new(utc.date(), utc.time());
            match trunc_timestamp(&field, &dt)? {
                Value::Timestamp(ts) => Ok(Value::TimestampTz(ts.assume_utc())),
                other => Ok(other),
            }
        }
        Value::Date(d) => {
            // PostgreSQL promotes DATE inputs using the session timezone.
            if is_infinity_date(*d) {
                return Ok(Value::TimestampTz(if is_positive_infinity_marker(*d) {
                    pos_infinity_timestamptz()
                } else {
                    neg_infinity_timestamptz()
                }));
            }
            let timezone = crate::eval::session::current_time_zone();
            let dt = PrimitiveDateTime::new(*d, Time::MIDNIGHT);
            match trunc_timestamp(&field, &dt)? {
                Value::Timestamp(ts) => Ok(Value::TimestampTz(timezone.apply_to_local(ts))),
                other => Ok(other),
            }
        }
        Value::Interval(iv) => {
            // date_trunc on an interval: truncate the interval fields
            trunc_interval(&field, iv)
        }
        Value::Text(s) => {
            // Try parsing as timestamp via cast
            use super::super::cast::cast_value;
            use aiondb_core::DataType;
            match cast_value(Value::Text(s.clone()), &DataType::Timestamp) {
                Ok(Value::Timestamp(ts)) => trunc_timestamp(&field, &ts),
                _ => match cast_value(Value::Text(s.clone()), &DataType::TimestampTz) {
                    Ok(Value::TimestampTz(odt)) => {
                        let utc = checked_to_offset(odt, time::UtcOffset::UTC, "date_trunc")?;
                        let dt = PrimitiveDateTime::new(utc.date(), utc.time());
                        match trunc_timestamp(&field, &dt)? {
                            Value::Timestamp(ts) => Ok(Value::TimestampTz(ts.assume_utc())),
                            other => Ok(other),
                        }
                    }
                    _ => match cast_value(Value::Text(s.clone()), &DataType::Date) {
                        Ok(Value::Date(d)) => {
                            if is_infinity_date(d) {
                                return Ok(Value::TimestampTz(if is_positive_infinity_marker(d) {
                                    pos_infinity_timestamptz()
                                } else {
                                    neg_infinity_timestamptz()
                                }));
                            }
                            let timezone = crate::eval::session::current_time_zone();
                            let dt = PrimitiveDateTime::new(d, Time::MIDNIGHT);
                            match trunc_timestamp(&field, &dt)? {
                                Value::Timestamp(ts) => {
                                    Ok(Value::TimestampTz(timezone.apply_to_local(ts)))
                                }
                                other => Ok(other),
                            }
                        }
                        _ => Err(DbError::internal(
                            "date_trunc() second arg must be a timestamp",
                        )),
                    },
                },
            }
        }
        _ => Err(DbError::internal(
            "date_trunc() second arg must be a timestamp",
        )),
    }
}

fn trunc_timestamp(field: &str, dt: &PrimitiveDateTime) -> DbResult<Value> {
    let make = |y, m: time::Month, d, h, min, s| -> DbResult<Value> {
        let date = Date::from_calendar_date(y, m, d)
            .map_err(|e| DbError::internal(format!("date error: {e}")))?;
        let time =
            Time::from_hms(h, min, s).map_err(|e| DbError::internal(format!("time error: {e}")))?;
        Ok(Value::Timestamp(PrimitiveDateTime::new(date, time)))
    };
    match field {
        "microseconds" | "microsecond" | "usec" | "usecs" => Ok(Value::Timestamp(*dt)),
        "milliseconds" | "millisecond" | "msec" | "msecs" => {
            let ms = u32::from(dt.millisecond());
            let micros = ms * 1000;
            let t = Time::from_hms_micro(dt.hour(), dt.minute(), dt.second(), micros)
                .map_err(|e| DbError::internal(format!("time error: {e}")))?;
            let d = Date::from_calendar_date(dt.year(), dt.month(), dt.day())
                .map_err(|e| DbError::internal(format!("date error: {e}")))?;
            Ok(Value::Timestamp(PrimitiveDateTime::new(d, t)))
        }
        "second" | "seconds" => make(
            dt.year(),
            dt.month(),
            dt.day(),
            dt.hour(),
            dt.minute(),
            dt.second(),
        ),
        "minute" => make(dt.year(), dt.month(), dt.day(), dt.hour(), dt.minute(), 0),
        "hour" => make(dt.year(), dt.month(), dt.day(), dt.hour(), 0, 0),
        "day" => make(dt.year(), dt.month(), dt.day(), 0, 0, 0),
        "week" => {
            // Truncate to start of ISO week (Monday)
            let weekday = dt.weekday().number_from_monday(); // 1=Mon, 7=Sun
            let monday = dt.date() - time::Duration::days(i64::from(weekday - 1));
            let d = Date::from_calendar_date(monday.year(), monday.month(), monday.day())
                .map_err(|e| DbError::internal(format!("date error: {e}")))?;
            Ok(Value::Timestamp(PrimitiveDateTime::new(d, Time::MIDNIGHT)))
        }
        "month" => make(dt.year(), dt.month(), 1, 0, 0, 0),
        "quarter" => {
            let m = u8::from(dt.month());
            let qm = ((m - 1) / 3) * 3 + 1;
            let month = time::Month::try_from(qm)
                .map_err(|e| DbError::internal(format!("month error: {e}")))?;
            make(dt.year(), month, 1, 0, 0, 0)
        }
        "year" => make(dt.year(), time::Month::January, 1, 0, 0, 0),
        "decade" => {
            let display_year = pg_display_year(dt.year());
            let year = astronomical_year_from_display(trunc_decade_display_year(display_year));
            make(year, time::Month::January, 1, 0, 0, 0)
        }
        "century" => {
            let display_year = pg_display_year(dt.year());
            let year = astronomical_year_from_display(trunc_century_display_year(display_year));
            make(year, time::Month::January, 1, 0, 0, 0)
        }
        "millennium" => {
            let display_year = pg_display_year(dt.year());
            let year = astronomical_year_from_display(trunc_millennium_display_year(display_year));
            make(year, time::Month::January, 1, 0, 0, 0)
        }
        _ => Err(DbError::internal(format!(
            "date_trunc: unknown field \"{field}\""
        ))),
    }
}

fn trunc_interval(field: &str, iv: &aiondb_core::IntervalValue) -> DbResult<Value> {
    use aiondb_core::IntervalValue;
    match field {
        "microseconds" | "microsecond" | "usec" | "usecs" => Ok(Value::Interval(iv.clone())),
        "milliseconds" | "millisecond" | "msec" | "msecs" => {
            let ms = (iv.micros / 1000) * 1000;
            Ok(Value::Interval(IntervalValue::new(iv.months, iv.days, ms)))
        }
        "second" | "seconds" => {
            let s = (iv.micros / 1_000_000) * 1_000_000;
            Ok(Value::Interval(IntervalValue::new(iv.months, iv.days, s)))
        }
        "minute" => {
            let m = (iv.micros / 60_000_000) * 60_000_000;
            Ok(Value::Interval(IntervalValue::new(iv.months, iv.days, m)))
        }
        "hour" => {
            let h = (iv.micros / 3_600_000_000) * 3_600_000_000;
            Ok(Value::Interval(IntervalValue::new(iv.months, iv.days, h)))
        }
        "day" => Ok(Value::Interval(IntervalValue::new(iv.months, iv.days, 0))),
        "month" => Ok(Value::Interval(IntervalValue::new(iv.months, 0, 0))),
        "quarter" => {
            let m = (iv.months / 3) * 3;
            Ok(Value::Interval(IntervalValue::new(m, 0, 0)))
        }
        "year" => {
            let y = (iv.months / 12) * 12;
            Ok(Value::Interval(IntervalValue::new(y, 0, 0)))
        }
        "decade" => {
            let y = (iv.months / 120) * 120;
            Ok(Value::Interval(IntervalValue::new(y, 0, 0)))
        }
        "century" => {
            let y = (iv.months / 1200) * 1200;
            Ok(Value::Interval(IntervalValue::new(y, 0, 0)))
        }
        "millennium" => {
            let y = (iv.months / 12000) * 12000;
            Ok(Value::Interval(IntervalValue::new(y, 0, 0)))
        }
        _ => Err(DbError::internal(format!(
            "date_trunc: unknown field \"{field}\""
        ))),
    }
}

pub(super) fn eval_age(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "age")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let (Value::Timestamp(dt1), Value::Timestamp(dt2)) = (&args[0], &args[1]) else {
        return Err(DbError::internal("age() requires two timestamp arguments"));
    };
    // Compute interval = dt1 - dt2 following PostgreSQL's age() semantics
    let mut years = dt1.year() - dt2.year();
    let mut months = i32::from(u8::from(dt1.month())) - i32::from(u8::from(dt2.month()));
    let mut days = i32::from(dt1.day()) - i32::from(dt2.day());

    if days < 0 {
        months -= 1;
        // Use the number of days in dt2's month as the correction
        let prev_month_days = days_in_month(dt1.year(), dt1.month());
        days += prev_month_days;
    }
    if months < 0 {
        years -= 1;
        months += 12;
    }
    let total_months = years.saturating_mul(12).saturating_add(months);

    // Compute time difference in microseconds
    let t1_micros = time_to_micros(dt1.hour(), dt1.minute(), dt1.second(), dt1.microsecond());
    let t2_micros = time_to_micros(dt2.hour(), dt2.minute(), dt2.second(), dt2.microsecond());
    let mut micros = t1_micros - t2_micros;
    let mut final_days = days;
    if micros < 0 && final_days > 0 {
        final_days -= 1;
        micros += 86_400_000_000; // one day in micros
    }

    Ok(Value::Interval(IntervalValue::new(
        total_months,
        final_days,
        micros,
    )))
}

fn time_to_micros(h: u8, m: u8, s: u8, us: u32) -> i64 {
    i64::from(h) * 3_600_000_000
        + i64::from(m) * 60_000_000
        + i64::from(s) * 1_000_000
        + i64::from(us)
}

fn days_in_month(year: i32, month: time::Month) -> i32 {
    // Use the time crate to find the last day of the given month
    let next_month = month.next();
    let (y, m) = if next_month == time::Month::January {
        (year + 1, next_month)
    } else {
        (year, next_month)
    };
    if let Ok(first_of_next) = Date::from_calendar_date(y, m, 1) {
        first_of_next
            .previous_day()
            .map_or(30, |d| i32::from(d.day()))
    } else {
        30 // fallback
    }
}

pub(super) fn eval_to_char(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "to_char")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let fmt = expect_text_arg(args, 1, "to_char()", "second")?.to_owned();
    match &args[0] {
        Value::Timestamp(dt) => {
            let result = pg_format_timestamp(dt, &fmt, None);
            Ok(Value::Text(result))
        }
        Value::TimestampTz(dt) => {
            let timezone = crate::eval::session::current_time_zone();
            let (offset, label) = timezone.parts_for_utc(*dt);
            let local = dt
                .checked_to_offset(offset)
                .unwrap_or_else(|| dt.to_offset(time::UtcOffset::UTC));
            let pdt = time::PrimitiveDateTime::new(local.date(), local.time());
            let result = pg_format_timestamp(&pdt, &fmt, Some(label.as_str()));
            Ok(Value::Text(result))
        }
        Value::Int(n) => Ok(Value::Text(pg_format_number(f64::from(*n), &fmt))),
        Value::BigInt(n) => Ok(Value::Text(pg_format_number(i64_to_f64(*n), &fmt))),
        Value::Real(n) => Ok(Value::Text(pg_format_number(f64::from(*n), &fmt))),
        Value::Double(n) => Ok(Value::Text(pg_format_number(*n, &fmt))),
        Value::Numeric(n) => {
            // `n.scale` is u32; clamp to i32::MAX before powi to avoid wrapping.
            // In practice scales are tiny (≤ 38), but defend against arbitrary values.
            let scale_i32 = to_i32_saturating(n.scale);
            let f = i128_to_f64(n.coefficient) / 10f64.powi(scale_i32);
            Ok(Value::Text(pg_format_number(f, &fmt)))
        }
        Value::Interval(iv) => {
            // Simplified: return a basic interval representation
            Ok(Value::Text(format!(
                "{} months {} days {} us",
                iv.months, iv.days, iv.micros
            )))
        }
        Value::Date(d) => {
            let pdt = time::PrimitiveDateTime::new(*d, time::Time::MIDNIGHT);
            let result = pg_format_timestamp(&pdt, &fmt, None);
            Ok(Value::Text(result))
        }
        _ => {
            // Fallback: convert to string
            Ok(Value::Text(args[0].to_string()))
        }
    }
}

/// PostgreSQL-compatible `to_char(numeric, format)` for numbers.
///
/// Supported format patterns:
///   `9`  - digit position (blank if insignificant)
///   `0`  - digit position (zero if insignificant)
///   `.`  - decimal point
///   `D`  - locale decimal point (we use `.`)
///   `,`  - grouping separator at this position
///   `G`  - locale grouping separator (we use `,`)
///   `S`  - sign anchored to number (`+`/`-`)
///   `SG` - sign always shown (`+`/`-`)
///   `PR` - angle brackets for negative: `<value>`
///   `MI` - trailing minus for negative
///   `FM` - fill mode (suppress padding)
///   `TH`/`th` - ordinal suffix (st, nd, rd, th)
///   `"text"` - literal text
///   space - literal space in output
///
/// For simplicity we support common abbreviations and +HH:MM offsets.
pub(super) fn eval_timezone(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "timezone")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let zone: std::borrow::Cow<'_, str> = match &args[0] {
        Value::Text(s) => std::borrow::Cow::Borrowed(s.as_str()),
        Value::Interval(iv) => {
            // Interval used as timezone offset - convert to hours/minutes.
            // `iv.days` is i32; widening to i64 is always safe.
            let total_secs = iv.micros / 1_000_000 + i64::from(iv.days) * 86400;
            let hours = total_secs / 3600;
            let mins = (total_secs % 3600) / 60;
            std::borrow::Cow::Owned(if mins == 0 {
                format!("{hours:+}")
            } else {
                format!("{hours:+}:{mins:02}")
            })
        }
        _ => {
            return Err(DbError::internal(
                "timezone() first arg must be text or interval",
            ));
        }
    };

    let target_zone = parse_timezone_target(zone.as_ref())?;
    let target_offset = target_zone.offset_for_reference_date(session_local_date());

    match &args[1] {
        Value::Timestamp(ts) => Ok(Value::TimestampTz(target_zone.apply_to_local(*ts))),
        Value::TimestampTz(odt) => {
            let in_zone = target_zone.convert_utc(*odt);
            let local_ts = PrimitiveDateTime::new(in_zone.date(), in_zone.time());
            Ok(Value::Timestamp(local_ts))
        }
        Value::Time(t) => Ok(Value::TimeTz(*t, target_offset)),
        Value::TimeTz(time, current_offset) => {
            let utc_micros = {
                let local = i64::from(time.hour()) * 3_600_000_000
                    + i64::from(time.minute()) * 60_000_000
                    + i64::from(time.second()) * 1_000_000
                    + i64::from(time.microsecond());
                let utc = local - i64::from(current_offset.whole_seconds()) * 1_000_000;
                ((utc % 86_400_000_000) + 86_400_000_000) % 86_400_000_000
            };
            let local_micros = ((utc_micros
                + i64::from(target_offset.whole_seconds()) * 1_000_000)
                % 86_400_000_000
                + 86_400_000_000)
                % 86_400_000_000;
            // `local_micros` is in [0, 86_400_000_000) by the double-modulo above.
            // Therefore: hours ∈ [0, 23], minutes ∈ [0, 59], seconds ∈ [0, 59]  -
            // all fit u8 without overflow.
            let hours = u8::try_from(local_micros / 3_600_000_000).unwrap_or(0);
            let minutes = u8::try_from((local_micros % 3_600_000_000) / 60_000_000).unwrap_or(0);
            let seconds = u8::try_from((local_micros % 60_000_000) / 1_000_000).unwrap_or(0);
            // micros ∈ [0, 999_999] - fits u32.
            let micros = u32::try_from(local_micros % 1_000_000).unwrap_or(0);
            let local_time =
                Time::from_hms_micro(hours, minutes, seconds, micros).unwrap_or(Time::MIDNIGHT);
            Ok(Value::TimeTz(local_time, target_offset))
        }
        Value::Date(d) => {
            let ts = PrimitiveDateTime::new(*d, Time::MIDNIGHT);
            Ok(Value::TimestampTz(target_zone.apply_to_local(ts)))
        }
        Value::Text(s) => {
            use super::super::cast::cast_value;
            use aiondb_core::DataType;
            if let Ok(Value::Timestamp(ts)) =
                cast_value(Value::Text(s.clone()), &DataType::Timestamp)
            {
                return Ok(Value::TimestampTz(target_zone.apply_to_local(ts)));
            }
            if let Ok(Value::TimestampTz(odt)) =
                cast_value(Value::Text(s.clone()), &DataType::TimestampTz)
            {
                let in_zone = target_zone.convert_utc(odt);
                let local_ts = PrimitiveDateTime::new(in_zone.date(), in_zone.time());
                return Ok(Value::Timestamp(local_ts));
            }
            Err(DbError::internal(
                "timezone() second arg must be a timestamp or timestamptz",
            ))
        }
        _ => Err(DbError::internal(
            "timezone() second arg must be a timestamp or timestamptz",
        )),
    }
}

#[derive(Clone, Debug)]
enum ParsedTimeZoneTarget {
    Named(TimeZoneSetting),
    Fixed(time::UtcOffset),
}

impl ParsedTimeZoneTarget {
    fn offset_for_reference_date(&self, date: Date) -> time::UtcOffset {
        match self {
            Self::Named(setting) => setting.offset_for_date(date),
            Self::Fixed(offset) => *offset,
        }
    }

    fn apply_to_local(&self, local: PrimitiveDateTime) -> OffsetDateTime {
        match self {
            Self::Named(setting) => setting.apply_to_local(local),
            Self::Fixed(offset) => local.assume_offset(*offset),
        }
    }

    fn convert_utc(&self, utc: OffsetDateTime) -> OffsetDateTime {
        match self {
            Self::Named(setting) => {
                let (offset, _) = setting.parts_for_utc(utc);
                utc.checked_to_offset(offset).unwrap_or(utc)
            }
            Self::Fixed(offset) => utc.checked_to_offset(*offset).unwrap_or(utc),
        }
    }
}

fn parse_timezone_target(zone: &str) -> DbResult<ParsedTimeZoneTarget> {
    if let Some(setting) = TimeZoneSetting::try_parse(zone) {
        return Ok(ParsedTimeZoneTarget::Named(setting));
    }

    let trimmed = zone.trim();
    if trimmed.contains('/') {
        return Err(DbError::internal(format!(
            "time zone \"{trimmed}\" not recognized"
        )));
    }
    let offset = time::UtcOffset::from_whole_seconds(parse_numeric_tz_offset(trimmed))
        .unwrap_or(time::UtcOffset::UTC);
    Ok(ParsedTimeZoneTarget::Fixed(offset))
}

fn session_local_date() -> Date {
    let timezone = crate::eval::session::current_time_zone();
    let now = OffsetDateTime::now_utc();
    let (offset, _) = timezone.parts_for_utc(now);
    now.checked_to_offset(offset).unwrap_or(now).date()
}

/// Parse a numeric timezone offset string like "+05", "-08:30", "+0530".
fn parse_numeric_tz_offset(s: &str) -> i32 {
    let s = s.trim();
    if s.is_empty() {
        return 0;
    }
    let (sign, rest) = if let Some(rest) = s.strip_prefix('+') {
        (1i32, rest)
    } else if let Some(rest) = s.strip_prefix('-') {
        (-1i32, rest)
    } else {
        (1i32, s)
    };
    // Try HH:MM format
    if let Some(colon_pos) = rest.find(':') {
        let hours: i32 = rest[..colon_pos].parse().unwrap_or(0);
        let mins: i32 = rest[colon_pos + 1..].parse().unwrap_or(0);
        return sign * (hours * 3600 + mins * 60);
    }
    // Try HHMM or HH format
    if rest.len() == 4 {
        let hours: i32 = rest[..2].parse().unwrap_or(0);
        let mins: i32 = rest[2..].parse().unwrap_or(0);
        return sign * (hours * 3600 + mins * 60);
    }
    if let Ok(hours) = rest.parse::<i32>() {
        return sign * hours * 3600;
    }
    0 // default UTC
}

fn pg_format_timestamp(dt: &PrimitiveDateTime, fmt: &str, zone_label: Option<&str>) -> String {
    let seconds_of_day =
        u32::from(dt.hour()) * 3600 + u32::from(dt.minute()) * 60 + u32::from(dt.second());
    let mut result = fmt.to_string();
    result = result.replace("YYYY", &format!("{:04}", dt.year()));
    result = result.replace("MM", &format!("{:02}", u8::from(dt.month())));
    result = result.replace("DD", &format!("{:02}", dt.day()));
    result = result.replace("HH24", &format!("{:02}", dt.hour()));
    result = result.replace("HH", &format!("{:02}", dt.hour()));
    result = result.replace("MI", &format!("{:02}", dt.minute()));
    result = result.replace("SSSSS", &seconds_of_day.to_string());
    result = result.replace("SSSS", &seconds_of_day.to_string());
    if let Some(label) = zone_label {
        result = result.replace("TZ", label);
    }
    result = result.replace("SS", &format!("{:02}", dt.second()));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{with_session_context, EvalSessionContext};
    use time::{Month, UtcOffset};

    #[test]
    fn to_char_timestamptz_uses_session_zone_tokens() {
        let context = EvalSessionContext::from_settings(Some("Postgres, MDY"), Some("-1.5"));
        let value = Value::TimestampTz(
            PrimitiveDateTime::new(
                Date::from_calendar_date(2012, Month::December, 12).unwrap(),
                Time::from_hms(12, 0, 0).unwrap(),
            )
            .assume_offset(UtcOffset::from_hms(-1, -30, 0).unwrap()),
        );

        let formatted = with_session_context(context.clone(), || {
            eval_to_char(&[value.clone(), Value::Text("YYYY-MM-DD HH:MI:SS TZ".into())])
                .expect("to_char")
        });
        assert_eq!(formatted, Value::Text("2012-12-12 12:00:00 -01:30".into()));

        let formatted = with_session_context(context.clone(), || {
            eval_to_char(&[value.clone(), Value::Text("YYYY-MM-DD SSSS".into())]).expect("to_char")
        });
        assert_eq!(formatted, Value::Text("2012-12-12 43200".into()));

        let formatted = with_session_context(context, || {
            eval_to_char(&[value, Value::Text("YYYY-MM-DD SSSSS".into())]).expect("to_char")
        });
        assert_eq!(formatted, Value::Text("2012-12-12 43200".into()));
    }
}
