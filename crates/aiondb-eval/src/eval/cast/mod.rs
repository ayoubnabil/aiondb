#![allow(clippy::ignored_unit_patterns, clippy::format_collect)]

mod array_literal;
mod date_time;
mod date_time_helpers;
mod interval;
mod interval_iso;
pub(crate) mod numeric;
mod time_parse;

use aiondb_core::temporal::{
    is_infinity_date, is_infinity_timestamp, is_infinity_timestamptz, neg_infinity_date,
    neg_infinity_timestamptz, pg_timestamp_max, pg_timestamp_min, pg_timestamptz_max,
    pg_timestamptz_min, pos_infinity_date, pos_infinity_timestamptz,
};
use aiondb_core::{
    DataType, DateOrder, DbError, DbResult, ErrorReport, IntervalValue, MacAddr, MacAddr8,
    NumericValue, PgLsnValue, SqlState, TidValue, TimeZoneSetting, Value, VectorValue,
};

use self::array_literal::{has_explicit_array_bounds, parse_pg_array_text};
use super::money::{float_to_money, money_to_numeric, numeric_to_money, parse_money_text};
use super::operators::{float_to_i32, float_to_i64};
use super::scalar_functions::value_convert::{
    f64_to_i64, i32_to_f32, i64_to_f32, i64_to_f64, pg_lsn_out_of_range, timestamp_out_of_range,
};
use date_time::*;
pub(crate) use interval::cast_interval_with_fields;
use interval::parse_interval;
use numeric::*;
use time_parse::{parse_pg_time_components, TimeParseError};

fn fixed_date(year: i32, month: time::Month, day: u8) -> DbResult<time::Date> {
    time::Date::from_calendar_date(year, month, day)
        .map_err(|error| DbError::internal(format!("invalid built-in date constant: {error}")))
}

fn min_sentinel_date() -> time::Date {
    neg_infinity_date()
}

fn max_sentinel_date() -> time::Date {
    pos_infinity_date()
}

fn epoch_date() -> DbResult<time::Date> {
    fixed_date(1970, time::Month::January, 1)
}

fn date_out_of_range() -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::DatetimeFieldOverflow,
        "date out of range",
    ))
}

fn date_out_of_range_value(value: &str) -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::DatetimeFieldOverflow,
        format!("date out of range: \"{value}\""),
    ))
}

fn date_out_of_range_for_timestamp() -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::DatetimeFieldOverflow,
        "date out of range for timestamp",
    ))
}

fn ensure_timestamp_in_range_with(
    value: time::PrimitiveDateTime,
    err: fn() -> DbError,
) -> DbResult<time::PrimitiveDateTime> {
    if is_infinity_timestamp(value) || (value >= pg_timestamp_min() && value <= pg_timestamp_max())
    {
        Ok(value)
    } else {
        Err(err())
    }
}

fn ensure_timestamptz_in_range_with(
    value: time::OffsetDateTime,
    err: fn() -> DbError,
) -> DbResult<time::OffsetDateTime> {
    if is_infinity_timestamptz(value)
        || (value >= pg_timestamptz_min() && value <= pg_timestamptz_max())
    {
        Ok(value)
    } else {
        Err(err())
    }
}

fn ensure_timestamp_in_range(value: time::PrimitiveDateTime) -> DbResult<time::PrimitiveDateTime> {
    ensure_timestamp_in_range_with(value, timestamp_out_of_range)
}

fn ensure_timestamptz_in_range(value: time::OffsetDateTime) -> DbResult<time::OffsetDateTime> {
    ensure_timestamptz_in_range_with(value, timestamp_out_of_range)
}

fn ensure_timestamp_from_date_in_range(
    value: time::PrimitiveDateTime,
) -> DbResult<time::PrimitiveDateTime> {
    ensure_timestamp_in_range_with(value, date_out_of_range_for_timestamp)
}

fn ensure_timestamptz_from_date_in_range(
    value: time::OffsetDateTime,
) -> DbResult<time::OffsetDateTime> {
    ensure_timestamptz_in_range_with(value, date_out_of_range_for_timestamp)
}

fn cannot_cast_type(source: &str, target: &str) -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::DatatypeMismatch,
        format!("cannot cast type {source} to {target}"),
    ))
}

fn date_time_field_out_of_range(value: &str) -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::DatetimeFieldOverflow,
        format!("date/time field value out of range: \"{value}\""),
    ))
}

fn date_time_field_out_of_range_with_datestyle_hint(value: &str) -> DbError {
    DbError::from_report(
        ErrorReport::new(
            SqlState::DatetimeFieldOverflow,
            format!("date/time field value out of range: \"{value}\""),
        )
        .with_client_hint("Perhaps you need a different \"datestyle\" setting."),
    )
}

fn trailing_timezone_token(input: &str) -> Option<&str> {
    let (_, token) = input.trim().rsplit_once(char::is_whitespace)?;
    let token = token.trim();
    (!token.is_empty()).then_some(token)
}

fn timezone_not_recognized(input: &str) -> Option<DbError> {
    let token = trailing_timezone_token(input)?;
    if token.contains('/') && TimeZoneSetting::try_parse(token).is_none() {
        Some(DbError::internal(format!(
            "time zone \"{}\" not recognized",
            token.to_ascii_lowercase()
        )))
    } else {
        None
    }
}

fn timezone_displacement_out_of_range(input: &str) -> Option<DbError> {
    let token = trailing_timezone_token(input)?;
    let rest = token
        .strip_prefix('+')
        .or_else(|| token.strip_prefix('-'))?;
    if rest.is_empty() || !rest.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    if date_time_helpers::parse_utc_offset(token).is_err() {
        Some(DbError::internal(format!(
            "time zone displacement out of range: \"{input}\""
        )))
    } else {
        None
    }
}

fn timestamp_out_of_range_value(value: &str) -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::NumericValueOutOfRange,
        format!("timestamp out of range: \"{value}\""),
    ))
}

fn likely_datetime_field_overflow(value: &str) -> bool {
    value.to_ascii_lowercase().contains("feb 29")
}

fn maybe_datestyle_field_out_of_range(value: &str) -> Option<DbError> {
    let trimmed = value.trim();
    let date_prefix = trimmed
        .split_once(' ')
        .map(|(date, _)| date)
        .or_else(|| trimmed.split_once('T').map(|(date, _)| date))
        .unwrap_or(trimmed);
    let parts: Vec<&str> = date_prefix.split('/').collect();
    if parts.len() != 3 || !parts.iter().all(|part| !part.is_empty()) {
        return None;
    }

    let [first, second, year] = parts.as_slice() else {
        return None;
    };
    if year.len() < 4 || !year.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    if !first.chars().all(|ch| ch.is_ascii_digit()) || !second.chars().all(|ch| ch.is_ascii_digit())
    {
        return None;
    }

    let Ok(first) = first.parse::<u8>() else {
        return None;
    };
    let Ok(second) = second.parse::<u8>() else {
        return None;
    };

    let order = crate::eval::session::current_date_order();
    match order {
        DateOrder::Mdy if first > 12 && second <= 12 => {
            Some(date_time_field_out_of_range_with_datestyle_hint(trimmed))
        }
        DateOrder::Dmy if second > 12 && first <= 12 => {
            Some(date_time_field_out_of_range_with_datestyle_hint(trimmed))
        }
        _ => None,
    }
}

/// Check if a float input string represents a nonzero value.
/// Used to detect underflow when parsing returns 0.0.
fn float_input_is_nonzero(s: &str) -> bool {
    let s = s.trim().trim_start_matches(['+', '-']);
    // Strip leading zeros and find significant digits
    let s = s.trim_start_matches('0');
    // If what remains starts with a digit 1-9, or a '.' followed by nonzero digits, it's nonzero
    if let Some(ch) = s.chars().next() {
        if ch.is_ascii_digit() && ch != '0' {
            return true;
        }
        if ch == '.' {
            // Check digits after decimal point
            return s[1..].chars().any(|c| c.is_ascii_digit() && c != '0');
        }
    }
    false
}

pub(crate) fn cast_value(value: Value, target: &DataType) -> DbResult<Value> {
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }

    match (&value, target) {
        // Identity casts
        (Value::Int(_), DataType::Int)
        | (Value::BigInt(_), DataType::BigInt)
        | (Value::Real(_), DataType::Real)
        | (Value::Double(_), DataType::Double)
        | (Value::Numeric(_), DataType::Numeric)
        | (Value::Money(_), DataType::Money)
        | (Value::Text(_), DataType::Text)
        | (Value::Boolean(_), DataType::Boolean)
        | (Value::Uuid(_), DataType::Uuid)
        | (Value::Timestamp(_), DataType::Timestamp)
        | (Value::Date(_) | Value::LargeDate(_), DataType::Date)
        | (Value::Time(_), DataType::Time)
        | (Value::TimeTz(_, _), DataType::TimeTz)
        | (Value::Interval(_), DataType::Interval)
        | (Value::MacAddr(_), DataType::MacAddr)
        | (Value::MacAddr8(_), DataType::MacAddr8)
        | (Value::TimestampTz(_), DataType::TimestampTz)
        | (Value::Tid(_), DataType::Tid)
        | (Value::PgLsn(_), DataType::PgLsn)
        | (Value::Blob(_), DataType::Blob)
        | (Value::Jsonb(_), DataType::Jsonb) => Ok(value),

        // Int -> other numeric types
        (Value::Int(v), DataType::Numeric) => Ok(Value::Numeric(NumericValue::from_i32(*v))),
        (Value::Int(v), DataType::Money) => Ok(Value::Money(i64::from(*v) * 100)),
        (Value::Int(v), DataType::BigInt) => Ok(Value::BigInt(i64::from(*v))),
        (Value::Int(v), DataType::Real) => Ok(Value::Real(i32_to_f32(*v))),
        (Value::Int(v), DataType::Double) => Ok(Value::Double(f64::from(*v))),
        (Value::Int(v), DataType::Text) => Ok(Value::Text(v.to_string())),
        (Value::Int(v), DataType::Boolean) => Ok(Value::Boolean(*v != 0)),

        // BigInt -> other types
        (Value::BigInt(v), DataType::Numeric) => Ok(Value::Numeric(NumericValue::from_i64(*v))),
        (Value::BigInt(v), DataType::Money) => i64::try_from(i128::from(*v) * 100)
            .map(Value::Money)
            .map_err(|_| DbError::out_of_range("money", &v.to_string())),
        (Value::BigInt(v), DataType::Int) => i32::try_from(*v).map(Value::Int).map_err(|_| {
            DbError::from_report(ErrorReport::new(
                SqlState::NumericValueOutOfRange,
                "integer out of range",
            ))
        }),
        (Value::BigInt(v), DataType::Real) => Ok(Value::Real(i64_to_f32(*v))),
        (Value::BigInt(v), DataType::Double) => Ok(Value::Double(i64_to_f64(*v))),
        (Value::BigInt(v), DataType::Text) => Ok(Value::Text(v.to_string())),

        // Real -> other types
        (Value::Real(v), DataType::Int) => float_to_i32(f64::from(*v)),
        (Value::Real(v), DataType::BigInt) => float_to_i64(f64::from(*v)),
        (Value::Real(v), DataType::Double) => Ok(Value::Double(f64::from(*v))),
        (Value::Real(v), DataType::Text) => Ok(Value::Text(v.to_string())),
        (Value::Real(v), DataType::Numeric) => float_to_numeric(f64::from(*v)),
        (Value::Real(v), DataType::Money) => float_to_money(f64::from(*v)).map(Value::Money),

        // Double -> other types
        (Value::Double(v), DataType::Int) => float_to_i32(*v),
        (Value::Double(v), DataType::BigInt) => float_to_i64(*v),
        (Value::Double(v), DataType::Real) => {
            let r = v.to_string().parse::<f32>().unwrap_or_else(|_| {
                if v.is_sign_negative() {
                    f32::NEG_INFINITY
                } else {
                    f32::INFINITY
                }
            });
            if r.is_infinite() && !v.is_infinite() {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    "real out of range",
                )));
            }
            Ok(Value::Real(r))
        }
        (Value::Double(v), DataType::Text) => Ok(Value::Text(v.to_string())),
        (Value::Double(v), DataType::Numeric) => float_to_numeric(*v),
        (Value::Double(v), DataType::Money) => float_to_money(*v).map(Value::Money),

        // Numeric -> other types
        (Value::Numeric(v), DataType::Text) => Ok(Value::Text(v.to_string())),
        (Value::Numeric(v), DataType::Money) => numeric_to_money(v).map(Value::Money),
        (Value::Numeric(v), DataType::Int) => numeric_to_i32(v),
        (Value::Numeric(v), DataType::BigInt) => numeric_to_i64(v),
        (Value::Numeric(v), DataType::Real) => {
            let f = numeric_to_f64(v);
            let r = f.to_string().parse::<f32>().unwrap_or_else(|_| {
                if f.is_sign_negative() {
                    f32::NEG_INFINITY
                } else {
                    f32::INFINITY
                }
            });
            if r.is_infinite() && !f.is_infinite() {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    "real out of range",
                )));
            }
            Ok(Value::Real(r))
        }
        (Value::Numeric(v), DataType::Double) => Ok(Value::Double(numeric_to_f64(v))),
        (Value::Numeric(v), DataType::PgLsn) => {
            if v.is_nan() || v.is_infinite() || v.scale != 0 || v.is_big() {
                return Err(pg_lsn_out_of_range());
            }
            let raw = u64::try_from(v.coefficient).map_err(|_| pg_lsn_out_of_range())?;
            Ok(Value::PgLsn(PgLsnValue::new(raw)))
        }
        (Value::Money(v), DataType::Text) => Ok(Value::Text(Value::Money(*v).to_string())),
        (Value::Money(v), DataType::Numeric) => Ok(Value::Numeric(money_to_numeric(*v))),
        (Value::Money(v), DataType::Real) => Ok(Value::Real(i64_to_f32(*v) / 100.0)),
        (Value::Money(v), DataType::Double) => Ok(Value::Double(i64_to_f64(*v) / 100.0)),

        (Value::PgLsn(v), DataType::Text) => Ok(Value::Text(v.to_string())),

        // Text -> Int (with hex/octal/binary support)
        (Value::Text(s), DataType::Int) => {
            let t = s.trim();
            if t.is_empty() {
                return Err(DbError::invalid_input_syntax("integer", s));
            }
            match numeric::parse_pg_int_literal(t) {
                numeric::PgIntParseResult::Ok(v) => i32::try_from(v)
                    .map(Value::Int)
                    .map_err(|_| DbError::out_of_range("integer", t)),
                numeric::PgIntParseResult::Overflow => Err(DbError::out_of_range("integer", t)),
                numeric::PgIntParseResult::Invalid => {
                    Err(DbError::invalid_input_syntax("integer", s))
                }
            }
        }

        // Text -> BigInt (with hex/octal/binary support)
        (Value::Text(s), DataType::BigInt) => {
            let t = s.trim();
            if t.is_empty() {
                return Err(DbError::invalid_input_syntax("bigint", s));
            }
            match numeric::parse_pg_int_literal(t) {
                numeric::PgIntParseResult::Ok(v) => i64::try_from(v)
                    .map(Value::BigInt)
                    .map_err(|_| DbError::out_of_range("bigint", t)),
                numeric::PgIntParseResult::Overflow => Err(DbError::out_of_range("bigint", t)),
                numeric::PgIntParseResult::Invalid => {
                    Err(DbError::invalid_input_syntax("bigint", s))
                }
            }
        }

        (Value::Text(s), DataType::Real) => {
            let t = s.trim();
            match t.parse::<f32>() {
                Ok(v)
                    if v.is_infinite()
                        && !t.eq_ignore_ascii_case("infinity")
                        && !t.eq_ignore_ascii_case("+infinity")
                        && !t.eq_ignore_ascii_case("-infinity")
                        && !t.eq_ignore_ascii_case("inf")
                        && !t.eq_ignore_ascii_case("+inf")
                        && !t.eq_ignore_ascii_case("-inf") =>
                {
                    Err(DbError::out_of_range("real", t))
                }
                Ok(v) => Ok(Value::Real(v)),
                Err(_) => Err(DbError::invalid_input_syntax("real", s)),
            }
        }

        (Value::Text(s), DataType::Double) => {
            let t = s.trim();
            match t.parse::<f64>() {
                Ok(v)
                    if v.is_infinite()
                        && !t.eq_ignore_ascii_case("infinity")
                        && !t.eq_ignore_ascii_case("+infinity")
                        && !t.eq_ignore_ascii_case("-infinity")
                        && !t.eq_ignore_ascii_case("inf")
                        && !t.eq_ignore_ascii_case("+inf")
                        && !t.eq_ignore_ascii_case("-inf") =>
                {
                    Err(DbError::out_of_range("double precision", t))
                }
                Ok(v) if v == 0.0 && float_input_is_nonzero(t) => {
                    // Underflow: value parsed to zero but input is nonzero
                    Err(DbError::out_of_range("double precision", t))
                }
                Ok(v) => Ok(Value::Double(v)),
                Err(_) => Err(DbError::invalid_input_syntax("double precision", s)),
            }
        }

        (Value::Text(s), DataType::Numeric) => {
            let t = s.trim();
            if t.is_empty() {
                return Err(DbError::invalid_input_syntax("numeric", s));
            }
            t.parse::<NumericValue>().map(Value::Numeric).map_err(|e| {
                if e.contains("out of range") || e.contains("overflow") {
                    DbError::from_report(aiondb_core::ErrorReport::new(
                        aiondb_core::SqlState::NumericValueOutOfRange,
                        "value overflows numeric format",
                    ))
                } else {
                    DbError::invalid_input_syntax("numeric", s)
                }
            })
        }
        (Value::Text(s), DataType::Money) => parse_money_text(s).map(Value::Money),

        (Value::Text(s), DataType::PgLsn) => PgLsnValue::parse(s)
            .map(Value::PgLsn)
            .ok_or_else(|| DbError::invalid_input_syntax("pg_lsn", s)),
        (Value::Text(s), DataType::Tid) => TidValue::parse(s)
            .map(Value::Tid)
            .ok_or_else(|| DbError::invalid_input_syntax("tid", s)),

        (Value::Text(s), DataType::Boolean) => {
            // Avoid `to_ascii_lowercase()` String alloc - match the
            // small accept-list via `eq_ignore_ascii_case`.
            let trimmed = s.trim();
            const TRUE_TOKENS: &[&str] = &["true", "t", "1", "yes", "on", "y"];
            const FALSE_TOKENS: &[&str] = &["false", "f", "0", "no", "off", "n", "of"];
            if TRUE_TOKENS.iter().any(|t| trimmed.eq_ignore_ascii_case(t)) {
                Ok(Value::Boolean(true))
            } else if FALSE_TOKENS.iter().any(|t| trimmed.eq_ignore_ascii_case(t)) {
                Ok(Value::Boolean(false))
            } else {
                Err(DbError::invalid_input_syntax("boolean", s))
            }
        }
        (Value::Text(s), DataType::MacAddr) => MacAddr::parse(s)
            .map(Value::MacAddr)
            .ok_or_else(|| DbError::invalid_input_syntax("macaddr", s)),
        (Value::Text(s), DataType::MacAddr8) => MacAddr8::parse(s)
            .map(Value::MacAddr8)
            .ok_or_else(|| DbError::invalid_input_syntax("macaddr8", s)),

        // Text -> Date
        (Value::Text(s), DataType::Date) => {
            let s = s.trim();
            if s.eq_ignore_ascii_case("infinity") || s.eq_ignore_ascii_case("+infinity") {
                let d = max_sentinel_date();
                Ok(Value::Date(d))
            } else if s.eq_ignore_ascii_case("-infinity") {
                let d = min_sentinel_date();
                Ok(Value::Date(d))
            } else if s.eq_ignore_ascii_case("today") || s.eq_ignore_ascii_case("now") {
                let now = time::OffsetDateTime::now_utc();
                Ok(Value::Date(now.date()))
            } else if s.eq_ignore_ascii_case("epoch") {
                Ok(Value::Date(epoch_date()?))
            } else if s.eq_ignore_ascii_case("yesterday") {
                let d = time::OffsetDateTime::now_utc().date() - time::Duration::days(1);
                Ok(Value::Date(d))
            } else if s.eq_ignore_ascii_case("tomorrow") {
                let d = time::OffsetDateTime::now_utc().date() + time::Duration::days(1);
                Ok(Value::Date(d))
            } else {
                parse_date(s).map_err(|error| match error {
                    DateParseError::OutOfRange => date_out_of_range_value(s),
                    DateParseError::Datestyle => maybe_datestyle_field_out_of_range(s)
                        .unwrap_or_else(|| date_time_field_out_of_range_with_datestyle_hint(s)),
                    DateParseError::FieldOutOfRange => date_time_field_out_of_range(s),
                    DateParseError::Invalid => DbError::invalid_datetime_syntax("date", s),
                })
            }
        }

        // Text -> Timestamp
        (Value::Text(s), DataType::Timestamp) => {
            let s = s.trim();
            if s.eq_ignore_ascii_case("infinity")
                || s.eq_ignore_ascii_case("+infinity")
                || s.eq_ignore_ascii_case("-infinity")
                || s.eq_ignore_ascii_case("epoch")
            {
                if s.eq_ignore_ascii_case("epoch") {
                    let d = epoch_date()?;
                    Ok(Value::Timestamp(time::PrimitiveDateTime::new(
                        d,
                        time::Time::MIDNIGHT,
                    )))
                } else {
                    let d = if s.starts_with('-') {
                        min_sentinel_date()
                    } else {
                        max_sentinel_date()
                    };
                    Ok(Value::Timestamp(time::PrimitiveDateTime::new(
                        d,
                        time::Time::MIDNIGHT,
                    )))
                }
            } else {
                if let Some(err) = timezone_not_recognized(s) {
                    return Err(err);
                }
                if let Some(err) = timezone_displacement_out_of_range(s) {
                    return Err(err);
                }
                let parsed = parse_timestamp(s).map_err(|_| {
                    if likely_datetime_field_overflow(s) {
                        return date_time_field_out_of_range(s);
                    }
                    maybe_datestyle_field_out_of_range(s)
                        .unwrap_or_else(|| DbError::invalid_input_syntax("timestamp", s))
                })?;
                match parsed {
                    Value::Timestamp(ts) => ensure_timestamp_in_range(ts)
                        .map(Value::Timestamp)
                        .map_err(|_| timestamp_out_of_range_value(s)),
                    other => Ok(other),
                }
            }
        }

        // Text -> TimestampTz
        (Value::Text(s), DataType::TimestampTz) => {
            let s = s.trim();
            if s.eq_ignore_ascii_case("infinity")
                || s.eq_ignore_ascii_case("+infinity")
                || s.eq_ignore_ascii_case("-infinity")
                || s.eq_ignore_ascii_case("epoch")
            {
                if s.eq_ignore_ascii_case("epoch") {
                    let d = epoch_date()?;
                    Ok(Value::TimestampTz(
                        time::PrimitiveDateTime::new(d, time::Time::MIDNIGHT).assume_utc(),
                    ))
                } else {
                    let d = if s.starts_with('-') {
                        min_sentinel_date()
                    } else {
                        max_sentinel_date()
                    };
                    Ok(Value::TimestampTz(
                        time::PrimitiveDateTime::new(d, time::Time::MIDNIGHT).assume_utc(),
                    ))
                }
            } else {
                if let Some(err) = timezone_not_recognized(s) {
                    return Err(err);
                }
                if let Some(err) = timezone_displacement_out_of_range(s) {
                    return Err(err);
                }
                let parsed = parse_timestamp_tz(s).map_err(|_| {
                    if likely_datetime_field_overflow(s) {
                        return date_time_field_out_of_range(s);
                    }
                    maybe_datestyle_field_out_of_range(s).unwrap_or_else(|| {
                        DbError::invalid_input_syntax("timestamp with time zone", s)
                    })
                })?;
                match parsed {
                    Value::TimestampTz(ts) => ensure_timestamptz_in_range(ts)
                        .map(Value::TimestampTz)
                        .map_err(|_| timestamp_out_of_range_value(s)),
                    other => Ok(other),
                }
            }
        }

        // Text -> Time
        (Value::Text(s), DataType::Time) => {
            let s = s.trim();
            if has_date_dependent_zone_without_date(s) {
                return Err(DbError::invalid_datetime_syntax("time", s));
            }
            match parse_pg_time_components(s) {
                Ok(parsed) => return Ok(Value::Time(parsed.time)),
                Err(TimeParseError::OutOfRange) => return Err(date_time_field_out_of_range(s)),
                Err(TimeParseError::Invalid) => {}
            }
            match parse_timetz_detailed(s) {
                Ok(Value::TimeTz(time, _)) => return Ok(Value::Time(time)),
                Ok(other) => return Ok(other),
                Err(TimeParseError::OutOfRange) => return Err(date_time_field_out_of_range(s)),
                Err(TimeParseError::Invalid) => {}
            }
            if let Ok(v) = parse_timestamp(s).map(|v| match v {
                Value::Timestamp(ts) => Value::Time(ts.time()),
                _ => v,
            }) {
                return Ok(v);
            }
            if let Ok(v) = parse_timestamp_tz(s).map(|v| match v {
                Value::TimestampTz(odt) => Value::Time(odt.time()),
                _ => v,
            }) {
                return Ok(v);
            }
            Err(DbError::invalid_datetime_syntax("time", s))
        }

        // Text -> TimeTz
        (Value::Text(s), DataType::TimeTz) => {
            let s = s.trim();
            parse_timetz_detailed(s).map_err(|err| match err {
                TimeParseError::OutOfRange => date_time_field_out_of_range(s),
                TimeParseError::Invalid => {
                    DbError::invalid_datetime_syntax("time with time zone", s)
                }
            })
        }

        // Text -> Interval
        (Value::Text(s), DataType::Interval) => parse_interval(s),

        // Time -> Text
        (Value::Time(_), DataType::Text) => Ok(Value::Text(value.to_string())),

        // TimeTz -> Text
        (Value::TimeTz(_, _), DataType::Text) => Ok(Value::Text(value.to_string())),

        // Date -> Text
        (Value::Date(d), DataType::Text) => Ok(Value::Text(format!("{d}"))),
        (Value::LargeDate(d), DataType::Text) => Ok(Value::Text(d.to_string())),

        // Timestamp -> Text
        (Value::Timestamp(ts), DataType::Text) => Ok(Value::Text(format!("{ts}"))),

        // TimestampTz -> Text
        (Value::TimestampTz(odt), DataType::Text) => Ok(Value::Text(format!("{odt}"))),

        // Interval -> Text
        (Value::Interval(iv), DataType::Text) => {
            Ok(Value::Text(Value::Interval(iv.clone()).to_string()))
        }
        (Value::Tid(v), DataType::Text) => Ok(Value::Text(v.to_string())),
        (Value::MacAddr(v), DataType::Text) => Ok(Value::Text(v.to_string())),
        (Value::MacAddr8(v), DataType::Text) => Ok(Value::Text(v.to_string())),
        (Value::MacAddr(v), DataType::MacAddr8) => Ok(Value::MacAddr8(v.to_macaddr8())),
        (Value::MacAddr8(v), DataType::MacAddr) => {
            v.to_macaddr().map(Value::MacAddr).ok_or_else(|| {
                DbError::from_report(ErrorReport::new(
                    SqlState::InvalidTextRepresentation,
                    "macaddr8 not convertible to macaddr (bytes 3-4 must be ff:fe)",
                ))
            })
        }

        // Date -> Timestamp (midnight on that date)
        (Value::Date(d), DataType::Timestamp) => {
            Ok(Value::Timestamp(ensure_timestamp_from_date_in_range(
                time::PrimitiveDateTime::new(*d, time::Time::MIDNIGHT),
            )?))
        }
        (Value::LargeDate(_), DataType::Timestamp) => Err(date_out_of_range_for_timestamp()),

        // Timestamp -> Date (extract date portion)
        (Value::Timestamp(ts), DataType::Date) => Ok(Value::Date(ts.date())),

        // Timestamp -> Time (extract time portion)
        (Value::Timestamp(ts), DataType::Time) => Ok(Value::Time(ts.time())),

        // Timestamp -> TimeTz (attach default session-like offset)
        (Value::Timestamp(ts), DataType::TimeTz) => Ok(Value::TimeTz(
            ts.time(),
            default_timetz_offset(Some(ts.date())),
        )),

        // TimestampTz -> Time (extract time portion from UTC)
        (Value::TimestampTz(odt), DataType::Time) => {
            let local = odt.to_offset(default_timetz_offset(Some(odt.date())));
            Ok(Value::Time(local.time()))
        }

        // TimestampTz -> TimeTz (project into the current session time zone)
        (Value::TimestampTz(odt), DataType::TimeTz) => {
            let local = odt.to_offset(default_timetz_offset(Some(odt.date())));
            Ok(Value::TimeTz(local.time(), local.offset()))
        }

        // Time -> TimeTz (attach default offset)
        (Value::Time(t), DataType::TimeTz) => Ok(Value::TimeTz(*t, default_timetz_offset(None))),

        // TimeTz -> Time (strip offset)
        (Value::TimeTz(t, _), DataType::Time) => Ok(Value::Time(*t)),

        // Date -> TimeTz (midnight at default offset)
        (Value::Date(d), DataType::TimeTz) => Ok(Value::TimeTz(
            time::Time::MIDNIGHT,
            default_timetz_offset(Some(*d)),
        )),
        (Value::LargeDate(_), DataType::TimeTz) => Ok(Value::TimeTz(
            time::Time::MIDNIGHT,
            default_timetz_offset(None),
        )),

        // Time -> Interval (convert to interval with micros only)
        (Value::Time(t), DataType::Interval) => {
            let micros = i64::from(t.hour()) * 3_600_000_000
                + i64::from(t.minute()) * 60_000_000
                + i64::from(t.second()) * 1_000_000
                + i64::from(t.microsecond());
            Ok(Value::Interval(IntervalValue::new(0, 0, micros)))
        }

        // Interval -> Time (extract time portion)
        (Value::Interval(iv), DataType::Time) => {
            let total_micros = iv.micros.unsigned_abs();
            let hours = (total_micros / 3_600_000_000) % 24;
            let remaining = total_micros % 3_600_000_000;
            let minutes = remaining / 60_000_000;
            let remaining = remaining % 60_000_000;
            let seconds = remaining / 1_000_000;
            let micro = remaining % 1_000_000;
            let iv_err = || DbError::out_of_range("time without time zone", &format!("{iv:?}"));
            let hours = u8::try_from(hours).map_err(|_| iv_err())?;
            let minutes = u8::try_from(minutes).map_err(|_| iv_err())?;
            let seconds = u8::try_from(seconds).map_err(|_| iv_err())?;
            let micro = u32::try_from(micro).map_err(|_| iv_err())?;
            time::Time::from_hms_micro(hours, minutes, seconds, micro)
                .map(Value::Time)
                .map_err(|_| DbError::out_of_range("time without time zone", &format!("{iv:?}")))
        }

        // Timestamp -> TimestampTz (interpret in the current session time zone)
        (Value::Timestamp(ts), DataType::TimestampTz) => {
            let value = if is_infinity_timestamp(*ts) {
                if ts.date() == neg_infinity_date() {
                    neg_infinity_timestamptz()
                } else {
                    pos_infinity_timestamptz()
                }
            } else {
                ts.assume_offset(default_timetz_offset(Some(ts.date())))
            };
            Ok(Value::TimestampTz(ensure_timestamptz_in_range(value)?))
        }

        // TimestampTz -> Timestamp (convert to the current session time zone, strip offset)
        (Value::TimestampTz(odt), DataType::Timestamp) => {
            let local = if is_infinity_timestamptz(*odt) {
                *odt
            } else {
                odt.to_offset(default_timetz_offset(Some(odt.date())))
            };
            Ok(Value::Timestamp(ensure_timestamp_in_range(
                time::PrimitiveDateTime::new(local.date(), local.time()),
            )?))
        }

        // Date -> TimestampTz (midnight in the current session time zone)
        (Value::Date(d), DataType::TimestampTz) => {
            let ts = time::PrimitiveDateTime::new(*d, time::Time::MIDNIGHT);
            let value = if is_infinity_date(*d) {
                if *d == neg_infinity_date() {
                    neg_infinity_timestamptz()
                } else {
                    pos_infinity_timestamptz()
                }
            } else {
                ts.assume_offset(default_timetz_offset(Some(*d)))
            };
            Ok(Value::TimestampTz(ensure_timestamptz_from_date_in_range(
                value,
            )?))
        }
        (Value::LargeDate(_), DataType::TimestampTz) => Err(date_out_of_range_for_timestamp()),

        // TimestampTz -> Date
        (Value::TimestampTz(odt), DataType::Date) => {
            let local = odt.to_offset(default_timetz_offset(Some(odt.date())));
            Ok(Value::Date(local.date()))
        }

        // Boolean -> other types
        (Value::Boolean(b), DataType::Int) => Ok(Value::Int(i32::from(*b))),
        (Value::Boolean(b), DataType::Text) => {
            Ok(Value::Text(if *b { "true" } else { "false" }.to_string()))
        }

        // Blob -> Text (hex encoding)
        (Value::Blob(bytes), DataType::Text) => {
            // Stream `\xDEADBEEF` directly into one buffer.
            let mut out = String::with_capacity(2 + bytes.len() * 2);
            out.push('\\');
            out.push('x');
            aiondb_core::hex_encode_into(&bytes, &mut out);
            Ok(Value::Text(out))
        }

        // Text -> Blob (hex decoding, octal escapes, or plain text)
        (Value::Text(s), DataType::Blob) => {
            let trimmed = s.trim();
            if let Some(hex) = trimmed.strip_prefix("\\x") {
                parse_hex_bytes(hex)
                    .map(Value::Blob)
                    .map_err(|_| DbError::invalid_input_syntax("bytea", s))
            } else if trimmed.contains('\\') {
                // PG octal escape format: \NNN
                Ok(Value::Blob(parse_bytea_escape(trimmed)))
            } else {
                // Plain text -> bytes (UTF-8 encoding, PG compat)
                Ok(Value::Blob(trimmed.as_bytes().to_vec()))
            }
        }

        // Text -> UUID
        (Value::Text(s), DataType::Uuid) => {
            Value::uuid_from_str(s.trim()).ok_or_else(|| DbError::invalid_input_syntax("uuid", s))
        }

        // UUID -> Text
        (Value::Uuid(_), DataType::Text) => Ok(Value::Text(value.to_string())),

        // Text -> Vector
        (Value::Text(s), DataType::Vector { dims, .. }) => {
            let vec_val =
                VectorValue::parse(s).ok_or_else(|| DbError::invalid_input_syntax("vector", s))?;
            if *dims != 0 && vec_val.dims != *dims {
                return Err(DbError::internal(format!(
                    "expected {dims} dimensions, got {}",
                    vec_val.dims
                )));
            }
            Ok(Value::Vector(vec_val))
        }

        // Vector -> Text
        (Value::Vector(_), DataType::Text) => Ok(Value::Text(value.to_string())),

        // Vector -> floating array (pgvector-compatible explicit cast).
        (Value::Vector(vector), DataType::Array(element_type))
            if matches!(element_type.as_ref(), DataType::Real | DataType::Double) =>
        {
            let values = vector
                .values
                .iter()
                .map(|component| match element_type.as_ref() {
                    DataType::Real => Value::Real(*component),
                    DataType::Double => Value::Double(f64::from(*component)),
                    _ => unreachable!("guarded by floating array element type"),
                })
                .collect();
            Ok(Value::Array(values))
        }

        // Numeric array -> Vector (pgvector-compatible explicit cast).
        (Value::Array(elems), DataType::Vector { dims, .. }) => {
            let input_text = value.to_string();
            let mut values = Vec::with_capacity(elems.len());
            for elem in elems {
                if elem.is_null() || matches!(elem, Value::Array(_)) {
                    return Err(DbError::invalid_input_syntax("vector", &input_text));
                }
                let coerced = cast_value(elem.clone(), &DataType::Double)?;
                let Value::Double(number) = coerced else {
                    return Err(DbError::invalid_input_syntax("vector", &input_text));
                };
                let value = number as f32;
                if !value.is_finite() {
                    return Err(DbError::bind_error(
                        SqlState::NumericValueOutOfRange,
                        "vector element is out of range for float4",
                    ));
                }
                values.push(value);
            }
            let actual_dims = u32::try_from(values.len())
                .map_err(|_| DbError::program_limit("vector dimension count is out of range"))?;
            if *dims != 0 && actual_dims != *dims {
                return Err(DbError::internal(format!(
                    "expected {dims} dimensions, got {actual_dims}"
                )));
            }
            Ok(Value::Vector(VectorValue::new(actual_dims, values)))
        }

        // Text -> Jsonb. Use the bare PG message format (no `: "value"`
        // suffix) and stash the parser detail in client-detail so callers
        // see the same structure as PG's `ERROR / DETAIL` block.
        (Value::Text(s), DataType::Jsonb) => match serde_json::from_str::<serde_json::Value>(s) {
            Ok(v) => Ok(Value::Jsonb(v)),
            Err(err) => Err(DbError::from_report(
                aiondb_core::ErrorReport::new(
                    aiondb_core::SqlState::InvalidTextRepresentation,
                    "invalid input syntax for type json",
                )
                .with_client_detail(err.to_string()),
            )),
        },
        // Jsonb -> Text
        (Value::Jsonb(v), DataType::Text) => {
            Ok(Value::Text(aiondb_core::value::pg_jsonb_to_string(v)))
        }

        // Array -> Array: preserve the runtime shape and cast only leaf elements.
        (Value::Array(_), DataType::Array(_)) => {
            cast_array_preserving_shape(value, array_leaf_type(target))
        }

        // Text -> Array: parse PG array literal `{1,2,3}` into Value::Array
        // using the deepest scalar element type so dimension declarations stay lenient.
        (Value::Text(s), DataType::Array(_)) => {
            if array_leaf_type(target) == &DataType::Text {
                if let Some(values) = parse_composite_tuple_as_text_array(s) {
                    return Ok(Value::Array(
                        values
                            .into_iter()
                            .map(|value| value.map_or(Value::Null, Value::Text))
                            .collect(),
                    ));
                }
            }
            if has_explicit_array_bounds(s) {
                let trimmed = s.trim();
                let eq_pos = trimmed
                    .find('=')
                    .ok_or_else(|| DbError::invalid_input_syntax("array", trimmed))?;
                let prefix = trimmed[..eq_pos].trim();
                let canonical = self::parse_pg_array_text(trimmed, array_leaf_type(target))?;
                Ok(Value::Text(format!("{prefix}={canonical}")))
            } else {
                self::parse_pg_array_text(s, array_leaf_type(target))
            }
        }

        // Null -> Array
        (Value::Null, DataType::Array(_)) => Ok(Value::Null),

        // Array -> scalar: extract the first element and cast it.
        // An empty array yields NULL (like an empty result set).
        (Value::Array(elems), _) if !elems.is_empty() => cast_value(elems[0].clone(), target),
        (Value::Array(_), _) => Ok(Value::Null),

        // Vector identity cast with dimension validation
        (Value::Vector(ref v), DataType::Vector { dims, .. }) => {
            if *dims != 0 && v.dims != *dims {
                return Err(DbError::internal(format!(
                    "expected {dims} dimensions, got {}",
                    v.dims
                )));
            }
            Ok(value)
        }

        // Jsonb -> numeric types (extract number from JSON)
        (Value::Jsonb(v), DataType::Int) => match v {
            serde_json::Value::Number(n) => {
                let numeric = n
                    .to_string()
                    .parse::<NumericValue>()
                    .map_err(|_| DbError::invalid_input_syntax("integer", &n.to_string()))?;
                cast_value(Value::Numeric(numeric), target)
            }
            serde_json::Value::Bool(b) => cast_value(Value::Boolean(*b), target),
            _ => Err(DbError::invalid_input_syntax("integer", &v.to_string())),
        },
        (Value::Jsonb(v), DataType::BigInt) => match v {
            serde_json::Value::Number(n) => {
                let numeric = n
                    .to_string()
                    .parse::<NumericValue>()
                    .map_err(|_| DbError::invalid_input_syntax("bigint", &n.to_string()))?;
                cast_value(Value::Numeric(numeric), target)
            }
            _ => Err(DbError::invalid_input_syntax("bigint", &v.to_string())),
        },
        (Value::Jsonb(v), DataType::Double) => match v {
            serde_json::Value::Number(n) => cast_value(Value::Text(n.to_string()), target),
            _ => Err(DbError::invalid_input_syntax(
                "double precision",
                &v.to_string(),
            )),
        },
        (Value::Jsonb(v), DataType::Real) => match v {
            serde_json::Value::Number(n) => cast_value(Value::Text(n.to_string()), target),
            _ => Err(DbError::invalid_input_syntax("real", &v.to_string())),
        },
        (Value::Jsonb(v), DataType::Numeric) => match v {
            serde_json::Value::Number(n) => cast_value(Value::Text(n.to_string()), target),
            serde_json::Value::String(s) => cast_value(Value::Text(s.clone()), target),
            _ => Err(DbError::invalid_input_syntax("numeric", &v.to_string())),
        },
        (Value::Jsonb(v), DataType::Boolean) => match v {
            serde_json::Value::Bool(b) => Ok(Value::Boolean(*b)),
            _ => Err(DbError::invalid_input_syntax("boolean", &v.to_string())),
        },

        // BigInt -> Boolean
        (Value::BigInt(v), DataType::Boolean) => Ok(Value::Boolean(*v != 0)),
        // Boolean -> BigInt
        (Value::Boolean(b), DataType::BigInt) => Ok(Value::BigInt(i64::from(*b))),

        // Int -> Timestamp (interpret as epoch seconds)
        (Value::Int(v), DataType::Timestamp) => {
            let epoch = epoch_date()?;
            let ts = time::PrimitiveDateTime::new(epoch, time::Time::MIDNIGHT)
                .checked_add(time::Duration::seconds(i64::from(*v)))
                .ok_or_else(timestamp_out_of_range)?;
            Ok(Value::Timestamp(ts))
        }
        // BigInt -> Timestamp (interpret as epoch seconds)
        (Value::BigInt(v), DataType::Timestamp) => {
            let epoch = epoch_date()?;
            let ts = time::PrimitiveDateTime::new(epoch, time::Time::MIDNIGHT)
                .checked_add(time::Duration::seconds(*v))
                .ok_or_else(timestamp_out_of_range)?;
            Ok(Value::Timestamp(ts))
        }
        // Int -> Date (interpret as epoch days)
        (Value::Int(v), DataType::Date) => {
            let epoch = epoch_date()?;
            let days = i64::from(*v);
            if days.abs() > 3_652_424 {
                return Err(DbError::out_of_range("date", &v.to_string()));
            }
            Ok(Value::Date(
                epoch
                    .checked_add(time::Duration::days(days))
                    .ok_or_else(date_out_of_range)?,
            ))
        }

        // Numeric -> Boolean (non-zero = true)
        (Value::Numeric(n), DataType::Boolean) => {
            let v = numeric_to_f64(n);
            Ok(Value::Boolean(v != 0.0))
        }

        // Boolean -> Numeric
        (Value::Boolean(b), DataType::Numeric) => {
            Ok(Value::Numeric(NumericValue::from_i32(i32::from(*b))))
        }
        // Boolean -> Double
        (Value::Boolean(b), DataType::Double) => Ok(Value::Double(if *b { 1.0 } else { 0.0 })),
        // Boolean -> Real
        (Value::Boolean(b), DataType::Real) => Ok(Value::Real(if *b { 1.0 } else { 0.0 })),

        // Int -> Interval (interpret as seconds)
        (Value::Int(v), DataType::Interval) => Ok(Value::Interval(IntervalValue::new(
            0,
            0,
            i64::from(*v) * 1_000_000,
        ))),
        // BigInt -> Interval (interpret as microseconds)
        (Value::BigInt(v), DataType::Interval) => Ok(Value::Interval(IntervalValue::new(0, 0, *v))),
        // Double -> Interval (interpret as seconds)
        (Value::Double(v), DataType::Interval) => {
            let micros = *v * 1_000_000.0;
            if !micros.is_finite()
                || micros < i64_to_f64(i64::MIN)
                || micros >= i64_to_f64(i64::MAX)
            {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    "interval out of range",
                )));
            }
            Ok(Value::Interval(IntervalValue::new(
                0,
                0,
                f64_to_i64(micros)?,
            )))
        }

        // Date -> Int (epoch days since 1970-01-01)
        (Value::Date(d), DataType::Int) => {
            let epoch = epoch_date()?;
            let days = (*d - epoch).whole_days();
            i32::try_from(days)
                .map(Value::Int)
                .map_err(|_| DbError::out_of_range("integer", &days.to_string()))
        }
        // Date -> BigInt (epoch days since 1970-01-01)
        (Value::Date(d), DataType::BigInt) => {
            let epoch = epoch_date()?;
            let days = (*d - epoch).whole_days();
            Ok(Value::BigInt(days))
        }

        // Interval -> Int (total seconds)
        (Value::Interval(iv), DataType::Int) => {
            let total_secs = i64::from(iv.months) * 30 * 86400
                + i64::from(iv.days) * 86400
                + iv.micros / 1_000_000;
            i32::try_from(total_secs)
                .map(Value::Int)
                .map_err(|_| DbError::out_of_range("integer", &total_secs.to_string()))
        }
        // Interval -> BigInt (total microseconds)
        (Value::Interval(iv), DataType::BigInt) => {
            let total_micros = i64::from(iv.months) * 30 * 86400 * 1_000_000
                + i64::from(iv.days) * 86400 * 1_000_000
                + iv.micros;
            Ok(Value::BigInt(total_micros))
        }
        // Interval -> Double (total seconds as float)
        (Value::Interval(iv), DataType::Double) => {
            let total_secs = f64::from(iv.months) * 30.0 * 86400.0
                + f64::from(iv.days) * 86400.0
                + i64_to_f64(iv.micros) / 1_000_000.0;
            Ok(Value::Double(total_secs))
        }
        (Value::TimeTz(_, _), DataType::Interval) => {
            Err(cannot_cast_type("time with time zone", "interval"))
        }
        (Value::Interval(_), DataType::TimeTz) => {
            Err(cannot_cast_type("interval", "time with time zone"))
        }

        // Real -> Boolean
        (Value::Real(v), DataType::Boolean) => Ok(Value::Boolean(*v != 0.0)),
        // Double -> Boolean
        (Value::Double(v), DataType::Boolean) => Ok(Value::Boolean(*v != 0.0)),

        // Any -> Text (Display-based fallback)
        (_, DataType::Text) => Ok(Value::Text(value.to_string())),

        _ => Err(DbError::invalid_input_syntax(
            target.pg_type_name(),
            &format!("{value}"),
        )),
    }
}

fn array_leaf_type(data_type: &DataType) -> &DataType {
    let mut current = data_type;
    while let DataType::Array(inner) = current {
        current = inner.as_ref();
    }
    current
}

fn cast_array_preserving_shape(value: Value, leaf_target: &DataType) -> DbResult<Value> {
    cast_array_preserving_shape_at_depth(value, leaf_target, 0)
}

const CAST_ARRAY_MAX_DEPTH: u32 = 256;

fn cast_array_preserving_shape_at_depth(
    value: Value,
    leaf_target: &DataType,
    depth: u32,
) -> DbResult<Value> {
    if depth > CAST_ARRAY_MAX_DEPTH {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::ProgramLimitExceeded,
            format!("array nesting depth {depth} exceeds limit {CAST_ARRAY_MAX_DEPTH}"),
        )));
    }
    match value {
        Value::Null => Ok(Value::Null),
        Value::Array(elements) => {
            let casted = elements
                .into_iter()
                .map(|element| {
                    cast_array_preserving_shape_at_depth(element, leaf_target, depth + 1)
                })
                .collect::<DbResult<Vec<_>>>()?;
            Ok(Value::Array(casted))
        }
        scalar => cast_value(scalar, leaf_target),
    }
}

fn parse_composite_tuple_as_text_array(input: &str) -> Option<Vec<Option<String>>> {
    let trimmed = input.trim();
    let inner = trimmed.strip_prefix('(')?.strip_suffix(')')?;
    let mut out = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut was_quoted = false;
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek().copied() == Some('"') {
                    field.push('"');
                    let _ = chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }
        match ch {
            '"' => {
                in_quotes = true;
                was_quoted = true;
            }
            ',' => {
                if !was_quoted && field.is_empty() {
                    out.push(None);
                } else {
                    out.push(Some(std::mem::take(&mut field)));
                }
                field.clear();
                was_quoted = false;
            }
            _ => field.push(ch),
        }
    }
    if in_quotes {
        return None;
    }
    if !was_quoted && field.is_empty() {
        out.push(None);
    } else {
        out.push(Some(field));
    }
    Some(out)
}
