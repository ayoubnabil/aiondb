//! Cypher temporal operations: truncation, property access, duration.between,
//! and range().

use std::borrow::Cow;

use aiondb_core::{DbError, DbResult, IntervalValue, Value};
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

use super::super::value_convert::i64_to_i32;
use super::{
    format_utc_offset, iso_week_year, json_i32, json_i64, json_u8, parse_tz_offset, trunc_err,
    u8_to_month, value_type_name, MICROS_PER_DAY, MICROS_PER_HOUR, MICROS_PER_MILLI,
    MICROS_PER_MILLI_U32, MICROS_PER_MINUTE, MICROS_PER_SECOND, MILLIS_PER_DAY, MILLIS_PER_HOUR,
    MILLIS_PER_MINUTE, MILLIS_PER_SECOND, NANOS_PER_MICRO, SECONDS_PER_DAY, SECONDS_PER_HOUR,
    SECONDS_PER_MINUTE,
};

// ── Temporal property access ────────────────────────────────────────────

/// Extract a temporal property from a Value. Matching is case-insensitive
/// so the lexer-folded `d.weekyear` resolves the same way as `d.weekYear`.
pub(crate) fn temporal_property_access(base: &Value, field: &str) -> Option<Value> {
    let canon = canonical_temporal_field_name(field);
    match base {
        Value::Date(d) => date_property(d, &canon),
        Value::Time(t) => time_property(t, &canon),
        Value::TimeTz(t, off) => timetz_property(t, off, &canon),
        Value::Timestamp(ts) => timestamp_property(ts, &canon),
        Value::TimestampTz(odt) => timestamptz_property(odt, &canon),
        Value::Interval(iv) => interval_property(iv, &canon),
        _ => None,
    }
}

/// Map any case variant of a Cypher temporal accessor name to the canonical
/// camelCase form the per-type tables match against. Unknown names are
/// returned unchanged (borrowed).
fn canonical_temporal_field_name(field: &str) -> Cow<'_, str> {
    // Each known accessor matches case-insensitively but resolves to a
    // fixed `&'static str` that the per-type property tables compare
    // against. Returning `Cow::Borrowed(&'static str)` skips both the
    // `to_ascii_lowercase()` allocation and the `to_owned()` of the
    // canonical form on every property access.
    if field.eq_ignore_ascii_case("year") {
        Cow::Borrowed("year")
    } else if field.eq_ignore_ascii_case("month") {
        Cow::Borrowed("month")
    } else if field.eq_ignore_ascii_case("day") {
        Cow::Borrowed("day")
    } else if field.eq_ignore_ascii_case("ordinalday") {
        Cow::Borrowed("ordinalDay")
    } else if field.eq_ignore_ascii_case("quarter") {
        Cow::Borrowed("quarter")
    } else if field.eq_ignore_ascii_case("dayofweek") || field.eq_ignore_ascii_case("weekday") {
        Cow::Borrowed("dayOfWeek")
    } else if field.eq_ignore_ascii_case("dayofyear") {
        Cow::Borrowed("dayOfYear")
    } else if field.eq_ignore_ascii_case("week") {
        Cow::Borrowed("week")
    } else if field.eq_ignore_ascii_case("weekofyear") {
        Cow::Borrowed("weekOfYear")
    } else if field.eq_ignore_ascii_case("weekyear") {
        Cow::Borrowed("weekYear")
    } else if field.eq_ignore_ascii_case("dayofquarter") {
        Cow::Borrowed("dayOfQuarter")
    } else if field.eq_ignore_ascii_case("hour") {
        Cow::Borrowed("hour")
    } else if field.eq_ignore_ascii_case("minute") {
        Cow::Borrowed("minute")
    } else if field.eq_ignore_ascii_case("second") {
        Cow::Borrowed("second")
    } else if field.eq_ignore_ascii_case("millisecond") {
        Cow::Borrowed("millisecond")
    } else if field.eq_ignore_ascii_case("microsecond") {
        Cow::Borrowed("microsecond")
    } else if field.eq_ignore_ascii_case("nanosecond") {
        Cow::Borrowed("nanosecond")
    } else if field.eq_ignore_ascii_case("timezone") {
        Cow::Borrowed("timezone")
    } else if field.eq_ignore_ascii_case("offset") {
        Cow::Borrowed("offset")
    } else if field.eq_ignore_ascii_case("offsetminutes") {
        Cow::Borrowed("offsetMinutes")
    } else if field.eq_ignore_ascii_case("offsetseconds") {
        Cow::Borrowed("offsetSeconds")
    } else if field.eq_ignore_ascii_case("epochmillis") {
        Cow::Borrowed("epochMillis")
    } else if field.eq_ignore_ascii_case("epochseconds") {
        Cow::Borrowed("epochSeconds")
    } else if field.eq_ignore_ascii_case("years") {
        Cow::Borrowed("years")
    } else if field.eq_ignore_ascii_case("quarters") {
        Cow::Borrowed("quarters")
    } else if field.eq_ignore_ascii_case("quartersofyear") {
        Cow::Borrowed("quartersOfYear")
    } else if field.eq_ignore_ascii_case("months") {
        Cow::Borrowed("months")
    } else if field.eq_ignore_ascii_case("monthsofyear") {
        Cow::Borrowed("monthsOfYear")
    } else if field.eq_ignore_ascii_case("monthsofquarter") {
        Cow::Borrowed("monthsOfQuarter")
    } else if field.eq_ignore_ascii_case("weeks") {
        Cow::Borrowed("weeks")
    } else if field.eq_ignore_ascii_case("weeksofquarter") {
        Cow::Borrowed("weeksOfQuarter")
    } else if field.eq_ignore_ascii_case("days") {
        Cow::Borrowed("days")
    } else if field.eq_ignore_ascii_case("daysofweek") {
        Cow::Borrowed("daysOfWeek")
    } else if field.eq_ignore_ascii_case("daysofquarter") {
        Cow::Borrowed("daysOfQuarter")
    } else if field.eq_ignore_ascii_case("hours") {
        Cow::Borrowed("hours")
    } else if field.eq_ignore_ascii_case("minutes") {
        Cow::Borrowed("minutes")
    } else if field.eq_ignore_ascii_case("minutesofhour") {
        Cow::Borrowed("minutesOfHour")
    } else if field.eq_ignore_ascii_case("seconds") {
        Cow::Borrowed("seconds")
    } else if field.eq_ignore_ascii_case("secondsofminute") {
        Cow::Borrowed("secondsOfMinute")
    } else if field.eq_ignore_ascii_case("milliseconds") {
        Cow::Borrowed("milliseconds")
    } else if field.eq_ignore_ascii_case("millisecondsofsecond")
        || field.eq_ignore_ascii_case("millisofsecond")
    {
        Cow::Borrowed("millisecondsOfSecond")
    } else if field.eq_ignore_ascii_case("microseconds") {
        Cow::Borrowed("microseconds")
    } else if field.eq_ignore_ascii_case("microsecondsofsecond")
        || field.eq_ignore_ascii_case("microsofsecond")
    {
        Cow::Borrowed("microsecondsOfSecond")
    } else if field.eq_ignore_ascii_case("nanoseconds") {
        Cow::Borrowed("nanoseconds")
    } else if field.eq_ignore_ascii_case("nanosecondsofsecond")
        || field.eq_ignore_ascii_case("nanosofsecond")
    {
        Cow::Borrowed("nanosecondsOfSecond")
    } else {
        Cow::Borrowed(field)
    }
}

fn date_property(d: &Date, field: &str) -> Option<Value> {
    match field {
        "year" => Some(Value::BigInt(i64::from(d.year()))),
        "month" => Some(Value::BigInt(i64::from(u8::from(d.month())))),
        "day" => Some(Value::BigInt(i64::from(d.day()))),
        "ordinalDay" => Some(Value::BigInt(i64::from(d.ordinal()))),
        "quarter" => Some(Value::BigInt(i64::from((u8::from(d.month()) - 1) / 3 + 1))),
        "dayOfWeek" => Some(Value::BigInt(i64::from(d.weekday().number_from_monday()))),
        "dayOfYear" => Some(Value::BigInt(i64::from(d.ordinal()))),
        "week" | "weekOfYear" => Some(Value::BigInt(i64::from(d.iso_week()))),
        "weekYear" => Some(Value::BigInt(i64::from(iso_week_year(d)))),
        "dayOfQuarter" => {
            // Days into the calendar quarter starting at 1.
            let month_in_quarter = (u8::from(d.month()) - 1) % 3;
            let quarter_start_month = u8::from(d.month()) - month_in_quarter;
            let m = u8_to_month(quarter_start_month)?;
            let q_start = Date::from_calendar_date(d.year(), m, 1).ok()?;
            let diff = (*d - q_start).whole_days() + 1;
            Some(Value::BigInt(diff))
        }
        _ => None,
    }
}

fn time_property(t: &Time, field: &str) -> Option<Value> {
    let nano = t.nanosecond();
    match field {
        "hour" => Some(Value::BigInt(i64::from(t.hour()))),
        "minute" => Some(Value::BigInt(i64::from(t.minute()))),
        "second" => Some(Value::BigInt(i64::from(t.second()))),
        "millisecond" => Some(Value::BigInt(i64::from(nano / 1_000_000))),
        "microsecond" => Some(Value::BigInt(i64::from(nano / 1_000))),
        "nanosecond" => Some(Value::BigInt(i64::from(nano))),
        _ => None,
    }
}

fn timetz_property(t: &Time, off: &UtcOffset, field: &str) -> Option<Value> {
    match field {
        "offset" | "timezone" => Some(Value::Text(format_utc_offset(off))),
        "offsetMinutes" => {
            let (h, m, _) = off.as_hms();
            Some(Value::BigInt(i64::from(h) * 60 + i64::from(m)))
        }
        "offsetSeconds" => Some(Value::BigInt(i64::from(off.whole_seconds()))),
        _ => time_property(t, field),
    }
}

fn timestamp_property(ts: &PrimitiveDateTime, field: &str) -> Option<Value> {
    match field {
        "year" | "month" | "day" | "ordinalDay" | "quarter" | "dayOfWeek" | "dayOfYear"
        | "week" | "weekOfYear" | "weekYear" | "dayOfQuarter" => date_property(&ts.date(), field),
        "hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond" => {
            time_property(&ts.time(), field)
        }
        "epochMillis" => {
            let epoch = Date::from_ordinal_date(1970, 1).ok()?;
            let days = (ts.date() - epoch).whole_days();
            let day_millis = days * MILLIS_PER_DAY;
            let time_millis = i64::from(ts.time().hour()) * MILLIS_PER_HOUR
                + i64::from(ts.time().minute()) * MILLIS_PER_MINUTE
                + i64::from(ts.time().second()) * MILLIS_PER_SECOND
                + i64::from(ts.time().microsecond()) / MICROS_PER_MILLI;
            Some(Value::BigInt(day_millis + time_millis))
        }
        "epochSeconds" => {
            let epoch = Date::from_ordinal_date(1970, 1).ok()?;
            let days = (ts.date() - epoch).whole_days();
            let day_secs = days * SECONDS_PER_DAY;
            let time_secs = i64::from(ts.time().hour()) * SECONDS_PER_HOUR
                + i64::from(ts.time().minute()) * SECONDS_PER_MINUTE
                + i64::from(ts.time().second());
            Some(Value::BigInt(day_secs + time_secs))
        }
        _ => None,
    }
}

fn timestamptz_property(odt: &time::OffsetDateTime, field: &str) -> Option<Value> {
    match field {
        "year" | "month" | "day" | "ordinalDay" | "quarter" | "dayOfWeek" | "dayOfYear"
        | "week" | "weekOfYear" | "weekYear" | "dayOfQuarter" => date_property(&odt.date(), field),
        "hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond" => {
            time_property(&odt.time(), field)
        }
        "offset" | "timezone" => Some(Value::Text(format_utc_offset(&odt.offset()))),
        "offsetMinutes" => {
            let (h, m, _) = odt.offset().as_hms();
            Some(Value::BigInt(i64::from(h) * 60 + i64::from(m)))
        }
        "offsetSeconds" => Some(Value::BigInt(i64::from(odt.offset().whole_seconds()))),
        "epochMillis" => Some(Value::BigInt(
            odt.unix_timestamp() * MILLIS_PER_SECOND
                + i64::from(odt.microsecond() / MICROS_PER_MILLI_U32),
        )),
        "epochSeconds" => Some(Value::BigInt(odt.unix_timestamp())),
        _ => None,
    }
}

fn interval_property(iv: &IntervalValue, field: &str) -> Option<Value> {
    // Cypher duration accessors return *cumulative* values for the basic
    // names (`months`, `days`, `hours`, ...) and only the *within-larger-unit*
    // residue for the `XxxOfYy` family. Use floor division so negative
    // durations split correctly (`-23h59m59.9s` totals `-86400` seconds with
    // `+0.1s` of fractional remainder, not `-86399` and `-0.9s`).
    let div_floor = |a: i64, b: i64| -> i64 {
        let q = a / b;
        let r = a % b;
        if (r != 0) && ((r < 0) != (b < 0)) {
            q - 1
        } else {
            q
        }
    };
    let mod_floor = |a: i64, b: i64| -> i64 { a - div_floor(a, b) * b };
    let total_months = i64::from(iv.months);
    let years = div_floor(total_months, 12);
    let total_days_calendar = i64::from(iv.days);
    let total_seconds = div_floor(iv.micros, MICROS_PER_SECOND);
    let total_minutes = div_floor(iv.micros, MICROS_PER_MINUTE);
    let total_hours = div_floor(iv.micros, MICROS_PER_HOUR);
    let total_millis = div_floor(iv.micros, MICROS_PER_MILLI);
    let total_micros = iv.micros;
    let total_nanos = iv.micros.saturating_mul(NANOS_PER_MICRO);
    match field {
        "years" => Some(Value::BigInt(years)),
        "quarters" => Some(Value::BigInt(total_months / 3)),
        "months" => Some(Value::BigInt(total_months)),
        "weeks" => Some(Value::BigInt(total_days_calendar / 7)),
        "days" => Some(Value::BigInt(total_days_calendar)),
        "hours" => Some(Value::BigInt(total_hours)),
        "minutes" => Some(Value::BigInt(total_minutes)),
        "seconds" => Some(Value::BigInt(total_seconds)),
        "milliseconds" => Some(Value::BigInt(total_millis)),
        "microseconds" => Some(Value::BigInt(total_micros)),
        "nanoseconds" => Some(Value::BigInt(total_nanos)),
        // Within-larger-unit residues - use floor mod so negative
        // durations report a positive sub-unit remainder.
        "quartersOfYear" => Some(Value::BigInt(mod_floor(total_months, 12) / 3)),
        "monthsOfYear" => Some(Value::BigInt(mod_floor(total_months, 12))),
        "monthsOfQuarter" => Some(Value::BigInt(mod_floor(total_months, 3))),
        "weeksOfQuarter" => Some(Value::BigInt(mod_floor(total_days_calendar, 7))),
        "daysOfWeek" => Some(Value::BigInt(mod_floor(total_days_calendar, 7))),
        "daysOfQuarter" => Some(Value::BigInt(total_days_calendar)),
        "minutesOfHour" => Some(Value::BigInt(
            mod_floor(iv.micros, MICROS_PER_HOUR) / MICROS_PER_MINUTE,
        )),
        "secondsOfMinute" => Some(Value::BigInt(
            mod_floor(iv.micros, MICROS_PER_MINUTE) / MICROS_PER_SECOND,
        )),
        "millisecondsOfSecond" => Some(Value::BigInt(
            mod_floor(iv.micros, MICROS_PER_SECOND) / MICROS_PER_MILLI,
        )),
        "microsecondsOfSecond" => Some(Value::BigInt(mod_floor(iv.micros, MICROS_PER_SECOND))),
        "nanosecondsOfSecond" => Some(Value::BigInt(
            mod_floor(iv.micros, MICROS_PER_SECOND).saturating_mul(NANOS_PER_MICRO),
        )),
        _ => None,
    }
}

// ── Truncation helpers ──────────────────────────────────────────────────

fn extract_date(v: &Value) -> DbResult<Date> {
    match v {
        Value::Date(d) => Ok(*d),
        Value::Timestamp(ts) => Ok(ts.date()),
        Value::TimestampTz(odt) => Ok(odt.date()),
        _ => Err(DbError::internal(format!(
            "Cannot extract date from {} value",
            value_type_name(v)
        ))),
    }
}

fn extract_time(v: &Value) -> Time {
    match v {
        Value::Time(t) => *t,
        Value::TimeTz(t, _) => *t,
        Value::Timestamp(ts) => ts.time(),
        Value::TimestampTz(odt) => odt.time(),
        _ => Time::MIDNIGHT,
    }
}

fn extract_offset(v: &Value) -> UtcOffset {
    match v {
        Value::TimeTz(_, off) => *off,
        Value::TimestampTz(odt) => odt.offset(),
        _ => UtcOffset::UTC,
    }
}

fn truncate_date_to_unit(d: &Date, unit: &str) -> DbResult<Date> {
    let year = d.year();
    match unit {
        "millennium" => {
            let trunc_year = year.div_euclid(1000) * 1000;
            Ok(Date::from_calendar_date(trunc_year, Month::January, 1)
                .map_err(trunc_err("date"))?)
        }
        "century" => {
            let trunc_year = year.div_euclid(100) * 100;
            Ok(Date::from_calendar_date(trunc_year, Month::January, 1)
                .map_err(trunc_err("date"))?)
        }
        "decade" => {
            let trunc_year = year.div_euclid(10) * 10;
            Ok(Date::from_calendar_date(trunc_year, Month::January, 1)
                .map_err(trunc_err("date"))?)
        }
        "year" => Ok(Date::from_calendar_date(year, Month::January, 1).map_err(trunc_err("date"))?),
        "weekyear" | "weekYear" => {
            let iso_year = iso_week_year(d);
            Ok(Date::from_iso_week_date(iso_year, 1, time::Weekday::Monday)
                .map_err(trunc_err("date"))?)
        }
        "quarter" => {
            let q = (u8::from(d.month()) - 1) / 3;
            let first_month = q * 3 + 1;
            let m = u8_to_month(first_month)
                .ok_or_else(|| DbError::internal("Invalid quarter month"))?;
            Ok(Date::from_calendar_date(year, m, 1).map_err(trunc_err("date"))?)
        }
        "month" => Ok(Date::from_calendar_date(year, d.month(), 1).map_err(trunc_err("date"))?),
        "week" => {
            let dow = d.weekday().number_from_monday();
            let days_back = i64::from(dow - 1);
            let monday = *d - time::Duration::days(days_back);
            Ok(monday)
        }
        "day" => Ok(*d),
        // they only affect the time component when truncating a
        // datetime/localdatetime, and the Date stays the same.
        "hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond" => Ok(*d),
        _ => Err(DbError::internal(format!(
            "Unsupported truncation unit for date: '{unit}'"
        ))),
    }
}

fn truncate_time_to_unit(t: &Time, unit: &str) -> DbResult<Time> {
    let nano = t.nanosecond();
    match unit {
        "millennium" | "century" | "decade" | "year" | "weekyear" | "weekYear" | "quarter"
        | "month" | "week" | "day" => Ok(Time::MIDNIGHT),
        "hour" => Time::from_hms_nano(t.hour(), 0, 0, 0).map_err(trunc_err("time")),
        "minute" => Time::from_hms_nano(t.hour(), t.minute(), 0, 0).map_err(trunc_err("time")),
        "second" => {
            Time::from_hms_nano(t.hour(), t.minute(), t.second(), 0).map_err(trunc_err("time"))
        }
        "millisecond" => {
            // Keep millisecond resolution: zero out microseconds and
            // nanoseconds beyond the millisecond boundary.
            let trunc_nano = (nano / 1_000_000) * 1_000_000;
            Time::from_hms_nano(t.hour(), t.minute(), t.second(), trunc_nano)
                .map_err(trunc_err("time"))
        }
        "microsecond" => {
            let trunc_nano = (nano / 1_000) * 1_000;
            Time::from_hms_nano(t.hour(), t.minute(), t.second(), trunc_nano)
                .map_err(trunc_err("time"))
        }
        "nanosecond" => Ok(*t),
        _ => Err(DbError::internal(format!(
            "Unsupported truncation unit for time: '{unit}'"
        ))),
    }
}

fn apply_map_overrides_to_date(d: Date, map: &Value) -> DbResult<Date> {
    let obj = match map {
        Value::Jsonb(v) => match v.as_object() {
            Some(obj) if !obj.is_empty() => obj,
            _ => return Ok(d),
        },
        _ => return Ok(d),
    };
    let mut year = d.year();
    let mut month = u8::from(d.month());
    let mut day = d.day();
    if let Some(v) = json_i32(obj, "year") {
        year = v;
    }
    if let Some(v) = json_u8(obj, "month") {
        month = v;
    }
    if let Some(v) = json_u8(obj, "day") {
        day = v;
    }
    if let Some(dow) = json_u8(obj, "dayOfWeek") {
        let current_dow = i32::from(d.weekday().number_from_monday());
        let target_dow = i32::from(dow);
        let diff = target_dow - current_dow;
        let adjusted = d + time::Duration::days(i64::from(diff));
        return Ok(adjusted);
    }
    let m = u8_to_month(month)
        .ok_or_else(|| DbError::internal(format!("Invalid month value: {month}")))?;
    Date::from_calendar_date(year, m, day)
        .map_err(|e| DbError::internal(format!("Invalid date after map overrides: {e}")))
}

fn apply_map_overrides_to_time(t: Time, map: &Value) -> DbResult<Time> {
    let obj = match map {
        Value::Jsonb(v) => match v.as_object() {
            Some(obj) if !obj.is_empty() => obj,
            _ => return Ok(t),
        },
        _ => return Ok(t),
    };
    let hour = json_u8(obj, "hour").unwrap_or(t.hour());
    let minute = json_u8(obj, "minute").unwrap_or(t.minute());
    let second = json_u8(obj, "second").unwrap_or(t.second());
    // Cypher sub-second overrides treat ms/us/ns as independent residues
    // within the second. When the user only overrides `nanosecond`, keep
    // the base time's ms/us components (else `time.truncate('ms', t,
    // {nanosecond: 2})` would zero out the truncated millisecond).
    let has_sub = obj.contains_key("millisecond")
        || obj.contains_key("microsecond")
        || obj.contains_key("nanosecond");
    let base_nano = t.nanosecond();
    let nano = if has_sub {
        let base_ms = i64::from(base_nano / 1_000_000);
        let base_us = i64::from((base_nano / 1_000) % 1_000);
        let base_ns = i64::from(base_nano % 1_000);
        let ms = json_i64(obj, "millisecond").unwrap_or(base_ms);
        let us = json_i64(obj, "microsecond").unwrap_or(base_us);
        let ns = json_i64(obj, "nanosecond").unwrap_or(base_ns);
        let total = ms.saturating_mul(1_000_000) + us.saturating_mul(1_000) + ns;
        u32::try_from(total)
            .map_err(|_| DbError::internal("subsecond override out of range for time"))?
    } else {
        base_nano
    };
    Time::from_hms_nano(hour, minute, second, nano)
        .map_err(|e| DbError::internal(format!("Invalid time after map overrides: {e}")))
}

fn extract_tz_override(map: &Value) -> Option<UtcOffset> {
    let obj = match map {
        Value::Jsonb(v) => v.as_object()?,
        _ => return None,
    };
    if let Some(tz_val) = obj.get("timezone") {
        if let Some(tz_str) = tz_val.as_str() {
            return parse_tz_offset(tz_str);
        }
    }
    None
}

pub(crate) fn eval_date_truncate(args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let unit = match &args[0] {
        Value::Text(s) => s.as_str(),
        _ => return Err(DbError::internal("date.truncate() unit must be a string")),
    };
    let input_date = extract_date(&args[1])?;
    let truncated = truncate_date_to_unit(&input_date, unit)?;
    let result = if args.len() > 2 {
        apply_map_overrides_to_date(truncated, &args[2])?
    } else {
        truncated
    };
    Ok(Value::Date(result))
}

pub(crate) fn eval_datetime_truncate(args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let unit = match &args[0] {
        Value::Text(s) => s.as_str(),
        _ => {
            return Err(DbError::internal(
                "datetime.truncate() unit must be a string",
            ));
        }
    };
    let input = &args[1];
    let input_date = extract_date(input)?;
    let input_time = extract_time(input);
    let input_offset = extract_offset(input);
    let truncated_date = truncate_date_to_unit(&input_date, unit)?;
    let truncated_time = truncate_time_to_unit(&input_time, unit)?;
    let map = if args.len() > 2 {
        &args[2]
    } else {
        &Value::Null
    };
    let result_date = apply_map_overrides_to_date(truncated_date, map)?;
    let result_time = apply_map_overrides_to_time(truncated_time, map)?;
    let result_offset = extract_tz_override(map).unwrap_or(input_offset);
    let pdt = PrimitiveDateTime::new(result_date, result_time);
    Ok(Value::TimestampTz(pdt.assume_offset(result_offset)))
}

pub(crate) fn eval_localdatetime_truncate(args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let unit = match &args[0] {
        Value::Text(s) => s.as_str(),
        _ => {
            return Err(DbError::internal(
                "localdatetime.truncate() unit must be a string",
            ));
        }
    };
    let input = &args[1];
    let input_date = extract_date(input)?;
    let input_time = extract_time(input);
    let truncated_date = truncate_date_to_unit(&input_date, unit)?;
    let truncated_time = truncate_time_to_unit(&input_time, unit)?;
    let map = if args.len() > 2 {
        &args[2]
    } else {
        &Value::Null
    };
    let result_date = apply_map_overrides_to_date(truncated_date, map)?;
    let result_time = apply_map_overrides_to_time(truncated_time, map)?;
    Ok(Value::Timestamp(PrimitiveDateTime::new(
        result_date,
        result_time,
    )))
}

pub(crate) fn eval_time_truncate(args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let unit = match &args[0] {
        Value::Text(s) => s.as_str(),
        _ => return Err(DbError::internal("time.truncate() unit must be a string")),
    };
    let input = &args[1];
    let input_time = extract_time(input);
    let input_offset = extract_offset(input);
    let truncated_time = truncate_time_to_unit(&input_time, unit)?;
    let map = if args.len() > 2 {
        &args[2]
    } else {
        &Value::Null
    };
    let result_time = apply_map_overrides_to_time(truncated_time, map)?;
    let result_offset = extract_tz_override(map).unwrap_or(input_offset);
    Ok(Value::TimeTz(result_time, result_offset))
}

pub(crate) fn eval_localtime_truncate(args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let unit = match &args[0] {
        Value::Text(s) => s.as_str(),
        _ => {
            return Err(DbError::internal(
                "localtime.truncate() unit must be a string",
            ));
        }
    };
    let input = &args[1];
    let input_time = extract_time(input);
    let truncated_time = truncate_time_to_unit(&input_time, unit)?;
    let map = if args.len() > 2 {
        &args[2]
    } else {
        &Value::Null
    };
    let result_time = apply_map_overrides_to_time(truncated_time, map)?;
    Ok(Value::Time(result_time))
}

// ── duration.between() ──────────────────────────────────────────────────

struct TemporalComponents {
    date: Option<Date>,
    time: Time,
    offset: UtcOffset,
}

fn decompose_temporal(v: &Value) -> DbResult<TemporalComponents> {
    match v {
        Value::Date(d) => Ok(TemporalComponents {
            date: Some(*d),
            time: Time::MIDNIGHT,
            offset: UtcOffset::UTC,
        }),
        Value::Time(t) => Ok(TemporalComponents {
            date: None,
            time: *t,
            offset: UtcOffset::UTC,
        }),
        Value::TimeTz(t, off) => Ok(TemporalComponents {
            date: None,
            time: *t,
            offset: *off,
        }),
        Value::Timestamp(ts) => Ok(TemporalComponents {
            date: Some(ts.date()),
            time: ts.time(),
            offset: UtcOffset::UTC,
        }),
        Value::TimestampTz(odt) => Ok(TemporalComponents {
            date: Some(odt.date()),
            time: odt.time(),
            offset: odt.offset(),
        }),
        _ => Err(DbError::internal(format!(
            "duration.between() does not accept {} values",
            value_type_name(v)
        ))),
    }
}

fn compute_duration_between(v1: &Value, v2: &Value) -> DbResult<IntervalValue> {
    let c1 = decompose_temporal(v1)?;
    let c2 = decompose_temporal(v2)?;
    if let (Some(d1), Some(d2)) = (c1.date, c2.date) {
        let t1_micros =
            time_to_micros(&c1.time) - i64::from(c1.offset.whole_seconds()) * MICROS_PER_SECOND;
        let t2_micros =
            time_to_micros(&c2.time) - i64::from(c2.offset.whole_seconds()) * MICROS_PER_SECOND;
        let mut months = (d2.year() - d1.year()) * 12
            + (i32::from(u8::from(d2.month())) - i32::from(u8::from(d1.month())));
        let d1_shifted = add_months_to_date(d1, months);
        let mut day_diff = days_between(&d1_shifted, &d2);
        if months > 0 && day_diff < 0 {
            months -= 1;
            let d1_shifted2 = add_months_to_date(d1, months);
            day_diff = days_between(&d1_shifted2, &d2);
        } else if months < 0 && day_diff > 0 {
            months += 1;
            let d1_shifted2 = add_months_to_date(d1, months);
            day_diff = days_between(&d1_shifted2, &d2);
        }
        let time_diff_micros = t2_micros - t1_micros;
        let (final_days, final_micros) = if day_diff > 0 && time_diff_micros < 0 {
            (day_diff - 1, time_diff_micros + MICROS_PER_DAY)
        } else if day_diff < 0 && time_diff_micros > 0 {
            (day_diff + 1, time_diff_micros - MICROS_PER_DAY)
        } else {
            (day_diff, time_diff_micros)
        };
        Ok(IntervalValue {
            months,
            days: i64_to_i32(final_days)?,
            micros: final_micros,
        })
    } else {
        let t1_micros =
            time_to_micros(&c1.time) - i64::from(c1.offset.whole_seconds()) * MICROS_PER_SECOND;
        let t2_micros =
            time_to_micros(&c2.time) - i64::from(c2.offset.whole_seconds()) * MICROS_PER_SECOND;
        let date_days = match (c1.date, c2.date) {
            (Some(d1), Some(d2)) => days_between(&d1, &d2),
            _ => 0,
        };
        let total_micros = date_days * MICROS_PER_DAY + (t2_micros - t1_micros);
        Ok(IntervalValue {
            months: 0,
            days: 0,
            micros: total_micros,
        })
    }
}

fn time_to_micros(t: &Time) -> i64 {
    i64::from(t.hour()) * MICROS_PER_HOUR
        + i64::from(t.minute()) * MICROS_PER_MINUTE
        + i64::from(t.second()) * MICROS_PER_SECOND
        + i64::from(t.microsecond())
}

fn days_between(d1: &Date, d2: &Date) -> i64 {
    (*d2 - *d1).whole_days()
}

fn add_months_to_date(d: Date, months: i32) -> Date {
    let total_months = d
        .year()
        .saturating_mul(12)
        .saturating_add(i32::from(u8::from(d.month())) - 1)
        .saturating_add(months);
    let new_year = total_months.div_euclid(12);
    let new_month = u8::try_from(total_months.rem_euclid(12) + 1).unwrap_or(1);
    let m = u8_to_month(new_month).unwrap_or(Month::January);
    let max_day = days_in_month(new_year, m);
    let day = d.day().min(max_day);
    Date::from_calendar_date(new_year, m, day).unwrap_or(d)
}

fn days_in_month(year: i32, month: Month) -> u8 {
    match month {
        Month::January
        | Month::March
        | Month::May
        | Month::July
        | Month::August
        | Month::October
        | Month::December => 31,
        Month::April | Month::June | Month::September | Month::November => 30,
        Month::February => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
    }
}

pub(crate) fn eval_duration_between(args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let iv = compute_duration_between(&args[0], &args[1])?;
    Ok(Value::Interval(iv))
}

pub(crate) fn eval_duration_in_months(args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let c1 = decompose_temporal(&args[0])?;
    let c2 = decompose_temporal(&args[1])?;
    if let (Some(d1), Some(d2)) = (c1.date, c2.date) {
        let months = (d2.year() - d1.year()) * 12
            + (i32::from(u8::from(d2.month())) - i32::from(u8::from(d1.month())));
        let d1_shifted = add_months_to_date(d1, months);
        let remaining = days_between(&d1_shifted, &d2);
        let final_months = if months > 0 && remaining < 0 {
            months - 1
        } else if months < 0 && remaining > 0 {
            months + 1
        } else {
            months
        };
        Ok(Value::Interval(IntervalValue {
            months: final_months,
            days: 0,
            micros: 0,
        }))
    } else {
        Ok(Value::Interval(IntervalValue {
            months: 0,
            days: 0,
            micros: 0,
        }))
    }
}

pub(crate) fn eval_duration_in_days(args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let c1 = decompose_temporal(&args[0])?;
    let c2 = decompose_temporal(&args[1])?;
    if let (Some(d1), Some(d2)) = (c1.date, c2.date) {
        let total_days = days_between(&d1, &d2);
        Ok(Value::Interval(IntervalValue {
            months: 0,
            days: i64_to_i32(total_days)?,
            micros: 0,
        }))
    } else {
        Ok(Value::Interval(IntervalValue {
            months: 0,
            days: 0,
            micros: 0,
        }))
    }
}

pub(crate) fn eval_duration_in_seconds(args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let c1 = decompose_temporal(&args[0])?;
    let c2 = decompose_temporal(&args[1])?;
    let t1_micros =
        time_to_micros(&c1.time) - i64::from(c1.offset.whole_seconds()) * MICROS_PER_SECOND;
    let t2_micros =
        time_to_micros(&c2.time) - i64::from(c2.offset.whole_seconds()) * MICROS_PER_SECOND;
    let date_micros = match (c1.date, c2.date) {
        (Some(d1), Some(d2)) => days_between(&d1, &d2) * MICROS_PER_DAY,
        _ => 0,
    };
    let total_micros = date_micros + (t2_micros - t1_micros);
    Ok(Value::Interval(IntervalValue {
        months: 0,
        days: 0,
        micros: total_micros,
    }))
}

// ── Cypher range(start, end [, step]) ─────────────────────────────────

pub(crate) fn eval_cypher_range(args: &[Value]) -> DbResult<Value> {
    if args.len() < 2 || args.len() > 3 {
        return Err(DbError::internal("range() requires 2 or 3 arguments"));
    }
    if args.iter().any(|a| matches!(a, Value::Null)) {
        return Ok(Value::Null);
    }
    let start = value_to_i64(&args[0])
        .ok_or_else(|| DbError::internal("range() start must be an integer"))?;
    let end = value_to_i64(&args[1])
        .ok_or_else(|| DbError::internal("range() end must be an integer"))?;
    // Cypher default step is always 1 (positive). When start > end and the
    // user didn't pass a negative step, the range is empty.
    let step = if args.len() == 3 {
        value_to_i64(&args[2])
            .ok_or_else(|| DbError::internal("range() step must be an integer"))?
    } else {
        1
    };
    if step == 0 {
        return Err(DbError::internal("range() step must not be zero"));
    }
    // Pre-size the output to the predicted row count so the Vec
    // doesn't grow through doublings on a large `range(1, 1_000_000)`.
    let predicted_rows = predicted_cypher_range_rows(start, end, step);
    let mut result = Vec::with_capacity(predicted_rows.min(MAX_CYPHER_RANGE_ROWS));
    let mut current = start;
    if step > 0 {
        while current <= end {
            result.push(Value::BigInt(current));
            current += step;
        }
    } else {
        while current >= end {
            result.push(Value::BigInt(current));
            current += step;
        }
    }
    Ok(Value::Array(result))
}

/// Cap on the pre-size hint for `range()` results to keep the
/// initial allocation bounded for absurdly wide ranges.
const MAX_CYPHER_RANGE_ROWS: usize = 1_000_000;

fn predicted_cypher_range_rows(start: i64, end: i64, step: i64) -> usize {
    if step == 0 {
        return 0;
    }
    let span = i128::from(end) - i128::from(start);
    let step_i128 = i128::from(step);
    if (step_i128 > 0 && span < 0) || (step_i128 < 0 && span > 0) {
        return 0;
    }
    let count = (span / step_i128).unsigned_abs().saturating_add(1);
    usize::try_from(count).unwrap_or(usize::MAX)
}

fn value_to_i64(v: &Value) -> Option<i64> {
    // Cypher's range() rejects every non-integer type at the runtime
    // type level - even values like `0.0` or `-0.0` whose magnitude
    // happens to be representable as an integer must produce
    // `InvalidArgumentType`.
    match v {
        Value::Int(n) => Some(i64::from(*n)),
        Value::BigInt(n) => Some(*n),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{IntervalValue, Value};
    use time::{Date, Month, Time};

    #[test]
    fn duration_between_dates() {
        let d1 = Value::Date(Date::from_calendar_date(2020, Month::January, 1).unwrap());
        let d2 = Value::Date(Date::from_calendar_date(2020, Month::March, 15).unwrap());
        let result = eval_duration_between(&[d1, d2]).unwrap();
        if let Value::Interval(iv) = result {
            assert_eq!(iv.months, 2);
            assert_eq!(iv.days, 14);
        } else {
            panic!("Expected Interval");
        }
    }
    #[test]
    fn duration_between_null_returns_null() {
        let d1 = Value::Date(Date::from_calendar_date(2020, Month::January, 1).unwrap());
        assert_eq!(
            eval_duration_between(&[d1, Value::Null]).unwrap(),
            Value::Null
        );
    }
    #[test]
    fn date_truncate_to_month() {
        let d = Value::Date(Date::from_calendar_date(2017, Month::November, 11).unwrap());
        let result = eval_date_truncate(&[Value::Text("month".into()), d]).unwrap();
        assert_eq!(
            result,
            Value::Date(Date::from_calendar_date(2017, Month::November, 1).unwrap())
        );
    }
    #[test]
    fn date_truncate_to_year() {
        let d = Value::Date(Date::from_calendar_date(2017, Month::November, 11).unwrap());
        let result = eval_date_truncate(&[Value::Text("year".into()), d]).unwrap();
        assert_eq!(
            result,
            Value::Date(Date::from_calendar_date(2017, Month::January, 1).unwrap())
        );
    }
    #[test]
    fn date_truncate_to_week() {
        let d = Value::Date(Date::from_calendar_date(2017, Month::November, 11).unwrap());
        let result = eval_date_truncate(&[Value::Text("week".into()), d]).unwrap();
        assert_eq!(
            result,
            Value::Date(Date::from_calendar_date(2017, Month::November, 6).unwrap())
        );
    }
    #[test]
    fn date_truncate_null_returns_null() {
        assert_eq!(
            eval_date_truncate(&[Value::Text("month".into()), Value::Null]).unwrap(),
            Value::Null
        );
    }
    #[test]
    fn date_property_year() {
        let d = Date::from_calendar_date(2021, Month::March, 15).unwrap();
        assert_eq!(
            temporal_property_access(&Value::Date(d), "year"),
            Some(Value::BigInt(2021))
        );
    }
    #[test]
    fn date_property_day_of_week() {
        let d = Date::from_calendar_date(2021, Month::March, 15).unwrap();
        assert_eq!(
            temporal_property_access(&Value::Date(d), "dayOfWeek"),
            Some(Value::BigInt(1))
        );
    }
    #[test]
    fn interval_property_hours() {
        let iv = IntervalValue {
            months: 0,
            days: 0,
            micros: 7_200_000_000,
        };
        assert_eq!(
            temporal_property_access(&Value::Interval(iv), "hours"),
            Some(Value::BigInt(2))
        );
    }
    #[test]
    fn time_property_minute() {
        let t = Time::from_hms_micro(14, 30, 45, 0).unwrap();
        assert_eq!(
            temporal_property_access(&Value::Time(t), "minute"),
            Some(Value::BigInt(30))
        );
    }
    #[test]
    fn unknown_property_returns_none() {
        let d = Date::from_calendar_date(2021, Month::January, 1).unwrap();
        assert_eq!(
            temporal_property_access(&Value::Date(d), "nonexistent"),
            None
        );
    }
    #[test]
    fn range_ascending() {
        let result = eval_cypher_range(&[Value::BigInt(0), Value::BigInt(3)]).unwrap();
        assert_eq!(
            result,
            Value::Array(vec![
                Value::BigInt(0),
                Value::BigInt(1),
                Value::BigInt(2),
                Value::BigInt(3)
            ])
        );
    }
    #[test]
    fn range_with_step() {
        let result =
            eval_cypher_range(&[Value::BigInt(0), Value::BigInt(10), Value::BigInt(3)]).unwrap();
        assert_eq!(
            result,
            Value::Array(vec![
                Value::BigInt(0),
                Value::BigInt(3),
                Value::BigInt(6),
                Value::BigInt(9)
            ])
        );
    }
    #[test]
    fn range_zero_step_errors() {
        assert!(
            eval_cypher_range(&[Value::BigInt(0), Value::BigInt(5), Value::BigInt(0)]).is_err()
        );
    }
}
