#![allow(dead_code)]
//! Cypher temporal constructor functions: date(), time(), localtime(),
//! datetime(), localdatetime(), duration().
//!
//! Each constructor accepts either:
//! - No arguments → current date/time
//! - A string argument → parse an ISO 8601 string
//! - A JSONB map argument → extract fields (year, month, day, hour, minute, second, etc.)

use aiondb_core::{DbError, DbResult, ErrorReport, IntervalValue, SqlState, Value};
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

use super::value_convert::{f64_to_i32, f64_to_i64, i64_to_i32};

#[path = "cypher_temporal/operations.rs"]
pub(crate) mod operations;

/// Average number of days per month (Gregorian calendar: 365.2425 / 12).
pub(super) const DAYS_PER_MONTH_AVG: f64 = 30.436875;

/// Microseconds per day.
pub(super) const MICROS_PER_DAY: i64 = 86_400_000_000;

/// Microseconds per hour.
pub(super) const MICROS_PER_HOUR: i64 = 3_600_000_000;

/// Microseconds per minute.
pub(super) const MICROS_PER_MINUTE: i64 = 60_000_000;

/// Microseconds per second.
pub(super) const MICROS_PER_SECOND: i64 = 1_000_000;

/// Floating-point variants for duration parsing where fractional values
/// are accumulated before casting to i64.
const MICROS_PER_DAY_F: f64 = 86_400_000_000.0;
const MICROS_PER_HOUR_F: f64 = 3_600_000_000.0;
const MICROS_PER_MINUTE_F: f64 = 60_000_000.0;
pub(super) const MICROS_PER_SECOND_F: f64 = 1_000_000.0;

/// Milliseconds per day (used in epochMillis computation).
pub(super) const MILLIS_PER_DAY: i64 = 86_400_000;
/// Milliseconds per hour.
pub(super) const MILLIS_PER_HOUR: i64 = 3_600_000;
/// Milliseconds per minute.
pub(super) const MILLIS_PER_MINUTE: i64 = 60_000;
/// Milliseconds per second.
pub(super) const MILLIS_PER_SECOND: i64 = 1_000;

/// Microseconds per millisecond.
pub(super) const MICROS_PER_MILLI: i64 = 1_000;
pub(super) const MICROS_PER_MILLI_U32: u32 = 1_000;
/// Nanoseconds per microsecond.
pub(super) const NANOS_PER_MICRO: i64 = 1_000;

/// Seconds per day.
pub(super) const SECONDS_PER_DAY: i64 = 86_400;
/// Seconds per hour.
pub(super) const SECONDS_PER_HOUR: i64 = 3_600;
/// Seconds per minute.
pub(super) const SECONDS_PER_MINUTE: i64 = 60;

fn f64_to_u32(value: f64, error: &str) -> DbResult<u32> {
    let as_i64 = f64_to_i64(value).map_err(|_| DbError::internal(error))?;
    u32::try_from(as_i64).map_err(|_| DbError::internal(error))
}

/// Map a `time` crate error into "Invalid truncated {kind}: {e}".
/// Eliminates 11 identical `.map_err(...)` chains.
pub(super) fn trunc_err(kind: &str) -> impl FnOnce(time::error::ComponentRange) -> DbError + '_ {
    move |e| DbError::internal(format!("Invalid truncated {kind}: {e}"))
}

// ── date() ──────────────────────────────────────────────────────────────

pub(crate) fn eval_cypher_date(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        let now = OffsetDateTime::now_utc();
        return Ok(Value::Date(now.date()));
    }
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => {
            let trimmed = s.trim();
            if trimmed.starts_with('{') {
                match serde_json::from_str::<serde_json::Value>(trimmed) {
                    Ok(v) => build_date_from_map(&v),
                    Err(_) => parse_date_string(s).map(Value::Date),
                }
            } else {
                parse_date_string(s).map(Value::Date)
            }
        }
        Value::Jsonb(map) => build_date_from_map(map),
        Value::Date(d) => Ok(Value::Date(*d)),
        Value::Timestamp(pdt) => Ok(Value::Date(pdt.date())),
        Value::TimestampTz(odt) => Ok(Value::Date(odt.date())),
        _ => Err(DbError::internal(format!(
            "date() does not accept {} values",
            value_type_name(&args[0])
        ))),
    }
}

fn parse_date_string(s: &str) -> DbResult<Date> {
    let s = s.trim();
    // Try various ISO 8601 date formats

    // YYYY-MM-DD
    if let Some(d) = try_parse_calendar_date(s) {
        return Ok(d);
    }
    // YYYYMMDD
    if s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()) {
        let year: i32 = s[0..4]
            .parse()
            .map_err(|_| DbError::internal("invalid date component: year"))?;
        let month: u8 = s[4..6]
            .parse()
            .map_err(|_| DbError::internal("invalid date component: month"))?;
        let day: u8 = s[6..8]
            .parse()
            .map_err(|_| DbError::internal("invalid date component: day"))?;
        if let Some(m) = u8_to_month(month) {
            if let Ok(d) = Date::from_calendar_date(year, m, day) {
                return Ok(d);
            }
        }
        return Err(DbError::internal(format!("invalid date string: '{s}'")));
    }
    // YYYY-MM (month only)
    if s.len() == 7 && s.as_bytes()[4] == b'-' {
        let year: i32 = s[0..4]
            .parse()
            .map_err(|_| DbError::internal("invalid date component: year"))?;
        let month: u8 = s[5..7]
            .parse()
            .map_err(|_| DbError::internal("invalid date component: month"))?;
        if let Some(m) = u8_to_month(month) {
            if let Ok(d) = Date::from_calendar_date(year, m, 1) {
                return Ok(d);
            }
        }
        return Err(DbError::internal(format!("invalid date string: '{s}'")));
    }
    // YYYYMM (month only, no dash)
    if s.len() == 6 && s.chars().all(|c| c.is_ascii_digit()) {
        let year: i32 = s[0..4]
            .parse()
            .map_err(|_| DbError::internal("invalid date component: year"))?;
        let month: u8 = s[4..6]
            .parse()
            .map_err(|_| DbError::internal("invalid date component: month"))?;
        if let Some(m) = u8_to_month(month) {
            if let Ok(d) = Date::from_calendar_date(year, m, 1) {
                return Ok(d);
            }
        }
        return Err(DbError::internal(format!("invalid date string: '{s}'")));
    }
    // YYYY (year only)
    if s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()) {
        let year: i32 = s
            .parse()
            .map_err(|_| DbError::internal("invalid date component: year"))?;
        if let Ok(d) = Date::from_calendar_date(year, Month::January, 1) {
            return Ok(d);
        }
        return Err(DbError::internal(format!("invalid date string: '{s}'")));
    }
    // YYYY-Www-D or YYYYWwwD (ISO week date)
    if let Some(d) = try_parse_week_date(s) {
        return Ok(d);
    }
    // YYYY-DDD or YYYYDDD (ordinal date)
    if let Some(d) = try_parse_ordinal_date(s) {
        return Ok(d);
    }

    Err(DbError::internal(format!(
        "Cannot parse date from string '{s}'"
    )))
}

fn try_parse_calendar_date(s: &str) -> Option<Date> {
    // YYYY-MM-DD
    if s.len() >= 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' {
        let year: i32 = s[0..4].parse().ok()?;
        let month: u8 = s[5..7].parse().ok()?;
        let day: u8 = s[8..10].parse().ok()?;
        let m = u8_to_month(month)?;
        return Date::from_calendar_date(year, m, day).ok();
    }
    None
}

fn try_parse_week_date(s: &str) -> Option<Date> {
    // YYYY-Www-D or YYYY-Www
    if s.contains('W') || s.contains('w') {
        let s_upper = s.replace('w', "W");
        let parts: Vec<&str> = s_upper.split('-').collect();
        if parts.len() >= 2 {
            let year: i32 = parts[0].parse().ok()?;
            let week_part = parts[1].strip_prefix('W')?;
            let week: u8 = week_part.parse().ok()?;
            let day_of_week = if parts.len() >= 3 {
                parts[2].parse::<u8>().ok()?
            } else {
                1 // Monday
            };
            let weekday = match day_of_week {
                1 => time::Weekday::Monday,
                2 => time::Weekday::Tuesday,
                3 => time::Weekday::Wednesday,
                4 => time::Weekday::Thursday,
                5 => time::Weekday::Friday,
                6 => time::Weekday::Saturday,
                7 => time::Weekday::Sunday,
                _ => return None,
            };
            return Date::from_iso_week_date(year, week, weekday).ok();
        }
        // YYYYWwwD or YYYYWww (compact form)
        if let Some(w_pos) = s_upper.find('W') {
            let year: i32 = s_upper[..w_pos].parse().ok()?;
            let rest = &s_upper[w_pos + 1..];
            let (week, day_of_week) = if rest.len() >= 3 {
                (
                    rest[..2].parse::<u8>().ok()?,
                    rest[2..3].parse::<u8>().ok()?,
                )
            } else if rest.len() == 2 {
                (rest.parse::<u8>().ok()?, 1u8)
            } else {
                return None;
            };
            let weekday = match day_of_week {
                1 => time::Weekday::Monday,
                2 => time::Weekday::Tuesday,
                3 => time::Weekday::Wednesday,
                4 => time::Weekday::Thursday,
                5 => time::Weekday::Friday,
                6 => time::Weekday::Saturday,
                7 => time::Weekday::Sunday,
                _ => return None,
            };
            return Date::from_iso_week_date(year, week, weekday).ok();
        }
    }
    None
}

fn try_parse_ordinal_date(s: &str) -> Option<Date> {
    // YYYY-DDD
    if s.len() == 8 && s.as_bytes()[4] == b'-' {
        let year: i32 = s[0..4].parse().ok()?;
        let ordinal: u16 = s[5..8].parse().ok()?;
        return Date::from_ordinal_date(year, ordinal).ok();
    }
    // YYYYDDD
    if s.len() == 7 && s.chars().all(|c| c.is_ascii_digit()) {
        let year: i32 = s[0..4].parse().ok()?;
        let ordinal: u16 = s[4..7].parse().ok()?;
        return Date::from_ordinal_date(year, ordinal).ok();
    }
    None
}

fn build_date_from_map(map: &serde_json::Value) -> DbResult<Value> {
    let obj = map
        .as_object()
        .ok_or_else(|| DbError::internal("date() map argument must be a JSON object"))?;

    // Map projection: `date({date: other, year: ..., ...})` extracts the
    // calendar date from `other` and selectively overrides fields. We
    // accept the base under any of `date`, `datetime`, or
    // `localdatetime` (Cypher allows selecting from any temporal that
    // has a date part).
    let base: Option<Date> = ["date", "datetime", "localdatetime"]
        .iter()
        .find_map(|k| obj.get(*k))
        .and_then(|v| v.as_str())
        .and_then(parse_date_prefix);

    if let Some(base_date) = base {
        if let Some(ord_val) = obj
            .get("ordinalDay")
            .or_else(|| obj.get("ordinalday"))
            .and_then(|v| v.as_i64())
        {
            let year = json_i32(obj, "year").unwrap_or(base_date.year());
            let ordinal = u16::try_from(ord_val)
                .map_err(|_| DbError::internal("Invalid ordinal date: out of range"))?;
            let date = Date::from_ordinal_date(year, ordinal)
                .map_err(|e| DbError::internal(format!("Invalid ordinal date: {e}")))?;
            return Ok(Value::Date(date));
        }
        if let Some(week_val) = obj.get("week") {
            // `{date: other, week: 1}` keeps the source's day-of-week
            // so a Sunday base picks Sunday-of-week-1 rather than the
            // ISO Monday default.
            let mut clone_obj = obj.clone();
            clone_obj
                .entry("year".to_string())
                .or_insert(serde_json::Value::Number(base_date.year().into()));
            let base_dow = match base_date.weekday() {
                time::Weekday::Monday => 1u8,
                time::Weekday::Tuesday => 2,
                time::Weekday::Wednesday => 3,
                time::Weekday::Thursday => 4,
                time::Weekday::Friday => 5,
                time::Weekday::Saturday => 6,
                time::Weekday::Sunday => 7,
            };
            if !clone_obj.contains_key("dayOfWeek") && !clone_obj.contains_key("dayofweek") {
                clone_obj.insert(
                    "dayOfWeek".to_string(),
                    serde_json::Value::Number(serde_json::Number::from(base_dow)),
                );
            }
            return build_week_date(&clone_obj, week_val);
        }
        if let Some(q) = obj.get("quarter").and_then(|v| v.as_i64()) {
            let year = json_i32(obj, "year").unwrap_or(base_date.year());
            let start_month: u8 = match q {
                1 => 1,
                2 => 4,
                3 => 7,
                4 => 10,
                _ => return Err(DbError::internal(format!("Invalid quarter: {q}"))),
            };
            // Two semantics:
            //   `{quarter: q}` keeps the base's month-in-quarter and day:
            //       1984-11-11 + quarter:3 → 1984-08-11 (Q4 mo2 → Q3 mo2).
            //   `{quarter: q, dayOfQuarter: n}` walks n days from quarter
            //   start and ignores base month/day entirely.
            if let Some(dq_raw) = obj
                .get("dayOfQuarter")
                .or_else(|| obj.get("dayofquarter"))
                .and_then(|v| v.as_i64())
            {
                let m_start = u8_to_month(start_month)
                    .ok_or_else(|| DbError::internal("quarter month is invalid"))?;
                let q_start = Date::from_calendar_date(year, m_start, 1)
                    .map_err(|e| DbError::internal(format!("Invalid quarter date: {e}")))?;
                let date = q_start
                    .checked_add(time::Duration::days(dq_raw - 1))
                    .ok_or_else(|| DbError::internal("dayOfQuarter overflows"))?;
                return Ok(Value::Date(date));
            }
            let mq = (u8::from(base_date.month()) - 1) % 3; // month-in-quarter (0..=2)
            let new_month = start_month + mq;
            let day = json_u8(obj, "day").unwrap_or(base_date.day());
            let m = u8_to_month(new_month)
                .ok_or_else(|| DbError::internal("quarter month is invalid"))?;
            let date = Date::from_calendar_date(year, m, day)
                .map_err(|e| DbError::internal(format!("Invalid date: {e}")))?;
            return Ok(Value::Date(date));
        }
        let year = json_i32(obj, "year").unwrap_or(base_date.year());
        let month = json_u8(obj, "month").unwrap_or(base_date.month() as u8);
        let day = json_u8(obj, "day").unwrap_or(base_date.day());
        let m = u8_to_month(month)
            .ok_or_else(|| DbError::internal(format!("Invalid month value: {month}")))?;
        let date = Date::from_calendar_date(year, m, day)
            .map_err(|e| DbError::internal(format!("Invalid date: {e}")))?;
        return Ok(Value::Date(date));
    }

    if let Some(week_val) = obj.get("week") {
        return build_week_date(obj, week_val);
    }
    if let Some(ord_val) = obj
        .get("ordinalDay")
        .or_else(|| obj.get("ordinalday"))
        .and_then(|v| v.as_i64())
    {
        let year = json_i32(obj, "year").unwrap_or(1970);
        let ordinal = u16::try_from(ord_val)
            .map_err(|_| DbError::internal("Invalid ordinal date: out of range"))?;
        let date = Date::from_ordinal_date(year, ordinal)
            .map_err(|e| DbError::internal(format!("Invalid ordinal date: {e}")))?;
        return Ok(Value::Date(date));
    }
    if let Some(q) = obj.get("quarter").and_then(|v| v.as_i64()) {
        let year = json_i32(obj, "year").unwrap_or(1970);
        let start_month: u8 = match q {
            1 => 1,
            2 => 4,
            3 => 7,
            4 => 10,
            _ => return Err(DbError::internal(format!("Invalid quarter: {q}"))),
        };
        let dq_raw = json_i64(obj, "dayOfQuarter").unwrap_or(1);
        let m_start = u8_to_month(start_month)
            .ok_or_else(|| DbError::internal("quarter month is invalid"))?;
        let q_start = Date::from_calendar_date(year, m_start, 1)
            .map_err(|e| DbError::internal(format!("Invalid quarter date: {e}")))?;
        let date = q_start
            .checked_add(time::Duration::days(dq_raw - 1))
            .ok_or_else(|| DbError::internal("dayOfQuarter overflows"))?;
        return Ok(Value::Date(date));
    }

    let year = json_i32(obj, "year").unwrap_or(1970);
    let month = json_u8(obj, "month").unwrap_or(1);
    let day = json_u8(obj, "day").unwrap_or(1);

    let m = u8_to_month(month)
        .ok_or_else(|| DbError::internal(format!("Invalid month value: {month}")))?;
    let date = Date::from_calendar_date(year, m, day)
        .map_err(|e| DbError::internal(format!("Invalid date: {e}")))?;

    Ok(Value::Date(date))
}

fn parse_date_prefix(s: &str) -> Option<Date> {
    let s = s.trim();
    let head = s.find('T').map_or(s, |idx| &s[..idx]);
    parse_date_string(head).ok()
}

fn build_week_date(
    obj: &serde_json::Map<String, serde_json::Value>,
    week_val: &serde_json::Value,
) -> DbResult<Value> {
    let year = json_i32(obj, "year")
        .ok_or_else(|| DbError::internal("week date requires 'year' field"))?;
    let week_i64 = week_val
        .as_i64()
        .or_else(|| {
            week_val.as_f64().and_then(|f| {
                if f.is_finite() && f >= 0.0 && f <= f64::from(u8::MAX) {
                    f64_to_i64(f).ok()
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| DbError::internal("'week' field must be an integer"))?;
    let week = u8::try_from(week_i64)
        .map_err(|_| DbError::internal("'week' field out of range (must be 1-53)"))?;
    let dow = json_u8(obj, "dayOfWeek").unwrap_or(1);

    let weekday = match dow {
        1 => time::Weekday::Monday,
        2 => time::Weekday::Tuesday,
        3 => time::Weekday::Wednesday,
        4 => time::Weekday::Thursday,
        5 => time::Weekday::Friday,
        6 => time::Weekday::Saturday,
        7 => time::Weekday::Sunday,
        _ => return Err(DbError::internal(format!("Invalid dayOfWeek value: {dow}"))),
    };

    let date = Date::from_iso_week_date(year, week, weekday)
        .map_err(|e| DbError::internal(format!("Invalid week date: {e}")))?;

    Ok(Value::Date(date))
}

// ── time() ──────────────────────────────────────────────────────────────

pub(crate) fn eval_cypher_time(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        let now = OffsetDateTime::now_utc();
        return Ok(Value::TimeTz(now.time(), now.offset()));
    }
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => {
            let trimmed = s.trim();
            if trimmed.starts_with('{') {
                match serde_json::from_str::<serde_json::Value>(trimmed) {
                    Ok(v) => build_time_from_map(&v),
                    Err(_) => parse_time_string(s).map(|(t, off)| Value::TimeTz(t, off)),
                }
            } else {
                parse_time_string(s).map(|(t, off)| Value::TimeTz(t, off))
            }
        }
        Value::Jsonb(map) => build_time_from_map(map),
        Value::TimeTz(t, off) => Ok(Value::TimeTz(*t, *off)),
        Value::Time(t) => Ok(Value::TimeTz(*t, UtcOffset::UTC)),
        Value::Timestamp(pdt) => Ok(Value::TimeTz(pdt.time(), UtcOffset::UTC)),
        Value::TimestampTz(odt) => Ok(Value::TimeTz(odt.time(), odt.offset())),
        _ => Err(DbError::internal(format!(
            "time() does not accept {} values",
            value_type_name(&args[0])
        ))),
    }
}

fn parse_time_string(s: &str) -> DbResult<(Time, UtcOffset)> {
    let s = s.trim();

    // Try to split timezone offset
    let (time_part, offset) = split_timezone_offset(s);

    let time = parse_time_component(time_part)?;
    Ok((time, offset))
}

/// Like [`parse_time_string`] but returns `None` for the offset when the
/// source carries no explicit timezone, so callers can distinguish a
/// localdatetime projection (no zone) from a datetime projection.
fn parse_time_string_opt(s: &str) -> DbResult<(Time, Option<UtcOffset>)> {
    let s = s.trim();
    let bytes = s.as_bytes();
    let has_z = s.ends_with('Z') || s.ends_with('z');
    let has_offset = has_z
        || (1..bytes.len())
            .rev()
            .any(|i| (bytes[i] == b'+' || bytes[i] == b'-') && bytes[i - 1].is_ascii_digit());
    let (time_part, off) = split_timezone_offset(s);
    let time = parse_time_component(time_part)?;
    Ok((time, has_offset.then_some(off)))
}

fn split_timezone_offset(s: &str) -> (&str, UtcOffset) {
    // Strip named timezone like [Europe/Stockholm] from the end
    let s = if let Some(bracket_pos) = s.find('[') {
        &s[..bracket_pos]
    } else {
        s
    };
    // Look for +/- timezone offset at the end
    // Handle: HH:MM:SS+OO:OO, HH:MM:SS+OO, HH:MM:SS-OO:OO
    if let Some(pos) = s.rfind('+').or_else(|| {
        // Find last '-' that's not at position 0 and comes after a digit
        let bytes = s.as_bytes();
        (1..bytes.len())
            .rev()
            .find(|&i| bytes[i] == b'-' && bytes[i - 1].is_ascii_digit())
    }) {
        if pos > 0 {
            let time_part = &s[..pos];
            let tz_part = &s[pos..];
            if let Some(offset) = parse_tz_offset(tz_part) {
                return (time_part, offset);
            }
        }
    }
    // Check for Z suffix
    if s.ends_with('Z') || s.ends_with('z') {
        return (&s[..s.len() - 1], UtcOffset::UTC);
    }
    (s, UtcOffset::UTC)
}

pub(super) fn parse_tz_offset(s: &str) -> Option<UtcOffset> {
    let (sign, rest) = if let Some(r) = s.strip_prefix('+') {
        (1i8, r)
    } else if let Some(r) = s.strip_prefix('-') {
        (-1i8, r)
    } else {
        return None;
    };

    if rest.contains(':') {
        // Format: HH:MM or HH:MM:SS
        let parts: Vec<&str> = rest.split(':').collect();
        let hours: i8 = parts.first()?.parse().ok()?;
        let minutes: i8 = parts.get(1).and_then(|m| m.parse().ok()).unwrap_or(0);
        UtcOffset::from_hms(sign * hours, sign * minutes, 0).ok()
    } else if rest.len() == 4 {
        // Compact format: HHMM (e.g., 0100 for +01:00)
        let hours: i8 = rest[0..2].parse().ok()?;
        let minutes: i8 = rest[2..4].parse().ok()?;
        UtcOffset::from_hms(sign * hours, sign * minutes, 0).ok()
    } else if rest.len() == 2 {
        // Just hours: HH (e.g., 01 for +01:00)
        let hours: i8 = rest.parse().ok()?;
        UtcOffset::from_hms(sign * hours, 0, 0).ok()
    } else {
        None
    }
}

fn parse_fraction_digits_to_units(frac: &str, target_digits: usize) -> u32 {
    let mut s = frac.to_string();
    if s.len() < target_digits {
        s.extend(std::iter::repeat('0').take(target_digits - s.len()));
    } else if s.len() > target_digits {
        s.truncate(target_digits);
    }
    s.parse().unwrap_or(0)
}

fn parse_seconds_with_fraction_nano(s: &str) -> DbResult<(u8, u32)> {
    if let Some(dot_pos) = s.find('.') {
        let sec: u8 = s[..dot_pos]
            .parse()
            .map_err(|_| DbError::internal("Invalid seconds"))?;
        let nano = parse_fraction_digits_to_units(&s[dot_pos + 1..], 9);
        Ok((sec, nano))
    } else {
        let sec: u8 = s
            .parse()
            .map_err(|_| DbError::internal("Invalid seconds"))?;
        Ok((sec, 0))
    }
}

fn parse_time_component(s: &str) -> DbResult<Time> {
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        let hour: u8 = parts[0]
            .parse()
            .map_err(|_| DbError::internal(format!("Invalid hour in time string '{s}'")))?;
        let minute: u8 = parts
            .get(1)
            .and_then(|m| m.split('.').next())
            .and_then(|m| m.parse().ok())
            .unwrap_or(0);
        let (second, nano) = if let Some(sec_str) = parts.get(2) {
            parse_seconds_with_fraction_nano(sec_str)?
        } else {
            (0, 0)
        };
        let nano = if nano == 0 && parts.len() == 2 {
            if let Some(dot_pos) = parts[1].find('.') {
                let frac_secs = parse_fraction_digits_to_units(&parts[1][dot_pos + 1..], 9);
                frac_secs.saturating_mul(60).min(999_999_999)
            } else {
                0
            }
        } else {
            nano
        };
        Time::from_hms_nano(hour, minute, second, nano)
            .map_err(|e| DbError::internal(format!("Invalid time: {e}")))
    } else {
        // Compact format: HHMMSS.fff, HHMMSS, HHMM, HH
        let (digits, frac) = if let Some(dot_pos) = s.find('.') {
            let frac_str = &s[dot_pos..];
            let frac: f64 = frac_str
                .parse()
                .map_err(|_| DbError::internal("Invalid fractional seconds"))?;
            (
                &s[..dot_pos],
                f64_to_u32(frac * MICROS_PER_SECOND_F, "Invalid fractional seconds")?,
            )
        } else {
            (s, 0u32)
        };
        match digits.len() {
            2 => {
                let hour: u8 = digits
                    .parse()
                    .map_err(|_| DbError::internal(format!("Invalid time string '{s}'")))?;
                Time::from_hms_micro(hour, 0, 0, frac)
                    .map_err(|e| DbError::internal(format!("Invalid time: {e}")))
            }
            4 => {
                let hour: u8 = digits[0..2].parse().unwrap_or(0);
                let minute: u8 = digits[2..4].parse().unwrap_or(0);
                Time::from_hms_micro(hour, minute, 0, frac)
                    .map_err(|e| DbError::internal(format!("Invalid time: {e}")))
            }
            6 => {
                let hour: u8 = digits[0..2].parse().unwrap_or(0);
                let minute: u8 = digits[2..4].parse().unwrap_or(0);
                let second: u8 = digits[4..6].parse().unwrap_or(0);
                Time::from_hms_micro(hour, minute, second, frac)
                    .map_err(|e| DbError::internal(format!("Invalid time: {e}")))
            }
            _ => Err(DbError::internal(format!(
                "Cannot parse time from string '{s}'"
            ))),
        }
    }
}

fn parse_seconds_with_fraction(s: &str) -> DbResult<(u8, u32)> {
    if let Some(dot_pos) = s.find('.') {
        let sec: u8 = s[..dot_pos]
            .parse()
            .map_err(|_| DbError::internal("Invalid seconds"))?;
        let frac_str = &s[dot_pos..];
        let frac: f64 = frac_str
            .parse()
            .map_err(|_| DbError::internal("Invalid fractional seconds"))?;
        Ok((
            sec,
            f64_to_u32(frac * MICROS_PER_SECOND_F, "Invalid fractional seconds")?,
        ))
    } else {
        let sec: u8 = s
            .parse()
            .map_err(|_| DbError::internal("Invalid seconds"))?;
        Ok((sec, 0))
    }
}

fn build_time_from_map(map: &serde_json::Value) -> DbResult<Value> {
    let obj = map
        .as_object()
        .ok_or_else(|| DbError::internal("time() map argument must be a JSON object"))?;

    // Map projection: `time({time: other, ...})` extracts the time of day
    // from `other` (which may itself be time/localtime/datetime/localdatetime
    // serialised as a JSON string) and selectively overrides components.
    let base: Option<(u8, u8, u8, u32, UtcOffset)> = ["time", "datetime", "localdatetime"]
        .iter()
        .find_map(|k| obj.get(*k))
        .and_then(|v| v.as_str())
        .and_then(parse_time_components_with_offset);

    let (mut hour, mut minute, mut second, mut nano, mut offset) =
        base.unwrap_or((0, 0, 0, 0, UtcOffset::UTC));

    if let Some(h) = json_u8(obj, "hour") {
        hour = h;
    }
    if let Some(m) = json_u8(obj, "minute") {
        minute = m;
    }
    if let Some(s) = json_u8(obj, "second") {
        second = s;
    }
    // Cypher allows specifying millisecond / microsecond / nanosecond
    // simultaneously: each represents the value at its own resolution
    // and they are combined into a single nanosecond field
    // (`{millisecond: 123, microsecond: 456, nanosecond: 789}` → `.123456789`).
    let has_sub_second = obj.contains_key("millisecond")
        || obj.contains_key("microsecond")
        || obj.contains_key("nanosecond");
    if has_sub_second {
        let ms = json_i64(obj, "millisecond").unwrap_or(0);
        let us = json_i64(obj, "microsecond").unwrap_or(0);
        let ns = json_i64(obj, "nanosecond").unwrap_or(0);
        let total = ms.saturating_mul(1_000_000) + us.saturating_mul(1_000) + ns;
        nano = u32::try_from(total)
            .map_err(|_| DbError::internal("subsecond components out of range"))?;
    }
    if let Some(tz_val) = obj.get("timezone") {
        if let Some(tz_secs) = tz_val.as_i64() {
            let tz_i32 = i32::try_from(tz_secs)
                .map_err(|_| DbError::internal("timezone offset out of range"))?;
            offset = UtcOffset::from_whole_seconds(tz_i32)
                .map_err(|e| DbError::internal(format!("Invalid timezone offset: {e}")))?;
        } else if let Some(tz_str) = tz_val.as_str() {
            if let Some(parsed) = parse_tz_offset_string(tz_str) {
                offset = parsed;
            }
        }
    }

    let time = Time::from_hms_nano(hour, minute, second, nano)
        .map_err(|e| DbError::internal(format!("Invalid time: {e}")))?;
    Ok(Value::TimeTz(time, offset))
}

fn parse_tz_offset_string(s: &str) -> Option<UtcOffset> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("Z") || s.eq_ignore_ascii_case("UTC") {
        return Some(UtcOffset::UTC);
    }
    let (sign, body) = match s.chars().next() {
        Some('+') => (1, &s[1..]),
        Some('-') => (-1, &s[1..]),
        _ => return None,
    };
    let parts: Vec<&str> = body.split(':').collect();
    let h: i32 = parts.first()?.parse().ok()?;
    let m: i32 = parts.get(1).map_or(Some(0), |p| p.parse().ok())?;
    UtcOffset::from_whole_seconds(sign * (h * 3600 + m * 60)).ok()
}

/// Parse a time-of-day prefix from a temporal string, returning
/// (hour, minute, second, nano, offset). Used for time/datetime map projection.
fn parse_time_components_with_offset(s: &str) -> Option<(u8, u8, u8, u32, UtcOffset)> {
    // Find the time portion: after 'T' or first space, otherwise whole string
    let after_date = if let Some(t_pos) = s.find('T') {
        &s[t_pos + 1..]
    } else if let Some(sp_pos) = s.find(' ') {
        &s[sp_pos + 1..]
    } else {
        s
    };
    // Split timezone suffix
    let (tp, offset) = split_time_and_offset(after_date);
    let parts: Vec<&str> = tp.split(':').collect();
    if parts.is_empty() {
        return None;
    }
    let hour: u8 = parts.first()?.parse().ok()?;
    let minute: u8 = parts.get(1).map_or(Some(0), |p| p.parse().ok())?;
    let (second, nano): (u8, u32) = if let Some(sec_str) = parts.get(2) {
        if let Some(dot) = sec_str.find('.') {
            let s_part: u8 = sec_str[..dot].parse().ok()?;
            let n = parse_fraction_digits_to_units(&sec_str[dot + 1..], 9);
            (s_part, n)
        } else {
            (sec_str.parse().ok()?, 0)
        }
    } else {
        (0, 0)
    };
    Some((hour, minute, second, nano, offset))
}

fn split_time_and_offset(s: &str) -> (&str, UtcOffset) {
    if let Some(stripped) = s.strip_suffix('Z').or_else(|| s.strip_suffix('z')) {
        return (stripped, UtcOffset::UTC);
    }
    // Look for + or - introducing the offset, but not as the first char
    // (which would be a sign on the hour, not an offset).
    let mut last_sign_idx: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if i > 0 && (c == '+' || c == '-') {
            last_sign_idx = Some(i);
        }
    }
    if let Some(idx) = last_sign_idx {
        if let Some(off) = parse_tz_offset_string(&s[idx..]) {
            return (&s[..idx], off);
        }
    }
    (s, UtcOffset::UTC)
}

// ── localtime() ─────────────────────────────────────────────────────────

pub(crate) fn eval_cypher_localtime(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        let now = OffsetDateTime::now_utc();
        return Ok(Value::Time(now.time()));
    }
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => {
            let (time, _offset) = parse_time_string(s)?;
            Ok(Value::Time(time))
        }
        Value::Jsonb(map) => {
            let val = build_time_from_map(map)?;
            // Strip timezone info, return just Time
            match val {
                Value::TimeTz(t, _) => Ok(Value::Time(t)),
                other => Ok(other),
            }
        }
        Value::Time(t) => Ok(Value::Time(*t)),
        Value::TimeTz(t, _) => Ok(Value::Time(*t)),
        Value::Timestamp(pdt) => Ok(Value::Time(pdt.time())),
        Value::TimestampTz(odt) => Ok(Value::Time(odt.time())),
        _ => Err(DbError::internal(format!(
            "localtime() does not accept {} values",
            value_type_name(&args[0])
        ))),
    }
}

// ── datetime() ──────────────────────────────────────────────────────────

pub(crate) fn eval_cypher_datetime(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        return Ok(Value::TimestampTz(OffsetDateTime::now_utc()));
    }
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => parse_datetime_string(s).map(Value::TimestampTz),
        Value::Jsonb(map) => build_datetime_from_map(map),
        Value::TimestampTz(ts) => Ok(Value::TimestampTz(*ts)),
        Value::Timestamp(ts) => {
            let odt = ts.assume_utc();
            Ok(Value::TimestampTz(odt))
        }
        _ => Err(DbError::internal(format!(
            "datetime() does not accept {} values",
            value_type_name(&args[0])
        ))),
    }
}

fn parse_datetime_string(s: &str) -> DbResult<OffsetDateTime> {
    let s = s.trim();
    // Split at 'T' or space to get date and time parts
    let (date_part, time_part) = if let Some(t_pos) = s.find('T') {
        (&s[..t_pos], Some(&s[t_pos + 1..]))
    } else if let Some(space_pos) = s.find(' ') {
        (&s[..space_pos], Some(&s[space_pos + 1..]))
    } else {
        (s, None)
    };

    let date = parse_date_string(date_part)?;

    let (time, offset) = if let Some(tp) = time_part {
        let (tp_cleaned, offset) = split_timezone_offset(tp);
        let time = parse_time_component(tp_cleaned)?;
        (time, offset)
    } else {
        (Time::MIDNIGHT, UtcOffset::UTC)
    };

    let pdt = PrimitiveDateTime::new(date, time);
    Ok(pdt.assume_offset(offset))
}

fn build_datetime_from_map(map: &serde_json::Value) -> DbResult<Value> {
    let obj = map
        .as_object()
        .ok_or_else(|| DbError::internal("datetime() map argument must be a JSON object"))?;

    // Build date part (reuse date map logic)
    let date_val = if obj.contains_key("week") {
        build_week_date(obj, &obj["week"])?
    } else {
        build_date_from_map(map)?
    };
    let Value::Date(date) = date_val else {
        return Err(DbError::internal("Failed to build date from map"));
    };

    // Pull the time-of-day from a sibling temporal projection if present
    // (`{date: x, time: other_dt}`), so e.g. localdatetime carries its hour
    // through a date+time projection.
    let base_time: Option<(u8, u8, u8, u32, UtcOffset)> = ["time", "datetime", "localdatetime"]
        .iter()
        .find_map(|k| obj.get(*k))
        .and_then(|v| v.as_str())
        .and_then(parse_time_components_with_offset);
    let (mut hour, mut minute, mut second, mut nano, mut offset) =
        base_time.unwrap_or((0, 0, 0, 0, UtcOffset::UTC));

    if let Some(h) = json_u8(obj, "hour") {
        hour = h;
    }
    if let Some(m) = json_u8(obj, "minute") {
        minute = m;
    }
    if let Some(s) = json_u8(obj, "second") {
        second = s;
    }
    let has_sub_second = obj.contains_key("millisecond")
        || obj.contains_key("microsecond")
        || obj.contains_key("nanosecond");
    if has_sub_second {
        let ms = json_i64(obj, "millisecond").unwrap_or(0);
        let us = json_i64(obj, "microsecond").unwrap_or(0);
        let ns = json_i64(obj, "nanosecond").unwrap_or(0);
        let total = ms.saturating_mul(1_000_000) + us.saturating_mul(1_000) + ns;
        nano = u32::try_from(total)
            .map_err(|_| DbError::internal("subsecond components out of range"))?;
    }
    if let Some(tz_val) = obj.get("timezone") {
        if let Some(tz_secs) = tz_val.as_i64() {
            let tz_i32 = i32::try_from(tz_secs)
                .map_err(|_| DbError::internal("timezone offset out of range"))?;
            offset = UtcOffset::from_whole_seconds(tz_i32)
                .map_err(|e| DbError::internal(format!("Invalid timezone offset: {e}")))?;
        } else if let Some(tz_str) = tz_val.as_str() {
            if let Some(parsed) = parse_tz_offset_string(tz_str) {
                offset = parsed;
            }
        }
    }

    let time = Time::from_hms_nano(hour, minute, second, nano)
        .map_err(|e| DbError::internal(format!("Invalid time: {e}")))?;
    let pdt = PrimitiveDateTime::new(date, time);
    Ok(Value::TimestampTz(pdt.assume_offset(offset)))
}

// ── localdatetime() ─────────────────────────────────────────────────────

pub(crate) fn eval_cypher_localdatetime(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        let now = OffsetDateTime::now_utc();
        return Ok(Value::Timestamp(PrimitiveDateTime::new(
            now.date(),
            now.time(),
        )));
    }
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => {
            let odt = parse_datetime_string(s)?;
            Ok(Value::Timestamp(PrimitiveDateTime::new(
                odt.date(),
                odt.time(),
            )))
        }
        Value::Jsonb(map) => {
            let val = build_datetime_from_map(map)?;
            match val {
                Value::TimestampTz(odt) => Ok(Value::Timestamp(PrimitiveDateTime::new(
                    odt.date(),
                    odt.time(),
                ))),
                other => Ok(other),
            }
        }
        Value::Timestamp(ts) => Ok(Value::Timestamp(*ts)),
        Value::TimestampTz(odt) => Ok(Value::Timestamp(PrimitiveDateTime::new(
            odt.date(),
            odt.time(),
        ))),
        _ => Err(DbError::internal(format!(
            "localdatetime() does not accept {} values",
            value_type_name(&args[0])
        ))),
    }
}

// ── duration() ──────────────────────────────────────────────────────────

// ── duration() ──────────────────────────────────────────────────────────

pub(crate) fn eval_cypher_duration(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        return Err(DbError::internal(
            "duration() requires at least one argument",
        ));
    }
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => parse_duration_string(s).map(Value::Interval),
        Value::Jsonb(map) => build_duration_from_map(map),
        Value::Interval(iv) => Ok(Value::Interval(iv.clone())),
        _ => Err(DbError::internal(format!(
            "duration() does not accept {} values",
            value_type_name(&args[0])
        ))),
    }
}

fn parse_duration_string(s: &str) -> DbResult<IntervalValue> {
    let s = s.trim();
    // ISO 8601 duration: P[nY][nM][nD][T[nH][nM][nS]]
    if s.starts_with('P') || s.starts_with('p') {
        return parse_iso_duration(&s[1..]);
    }
    Err(DbError::internal(format!(
        "Cannot parse duration from string '{s}'"
    )))
}

fn parse_iso_duration(s: &str) -> DbResult<IntervalValue> {
    // Check for date-like format: YYYY-MM-DDThh:mm:ss[.sss]
    if s.contains('-') && !s.starts_with('-') {
        return parse_date_like_duration(s);
    }

    let mut months: i32 = 0;
    let mut days: i32 = 0;
    let mut micros: i64 = 0;
    let mut in_time = false;
    let mut num_buf = String::new();

    for ch in s.chars() {
        match ch {
            'T' | 't' => {
                in_time = true;
            }
            '0'..='9' | '.' | '-' => {
                num_buf.push(ch);
            }
            'Y' | 'y' if !in_time => {
                let n: f64 = num_buf
                    .parse()
                    .map_err(|_| DbError::internal("Invalid year in duration"))?;
                months += f64_to_i32(n * 12.0)?;
                num_buf.clear();
            }
            'M' | 'm' if !in_time => {
                let n: f64 = num_buf
                    .parse()
                    .map_err(|_| DbError::internal("Invalid month in duration"))?;
                // Handle fractional months: 0.75M = 22.5 days
                let whole_months = f64_to_i32(n.trunc())?;
                let frac_months = n.fract();
                months += whole_months;
                if frac_months.abs() > 1e-9 {
                    let frac_days = frac_months * DAYS_PER_MONTH_AVG;
                    let whole_frac_days = f64_to_i32(frac_days.trunc())?;
                    let frac_day_remainder = frac_days.fract();
                    days += whole_frac_days;
                    micros += f64_to_i64(frac_day_remainder * MICROS_PER_DAY_F)
                        .map_err(|_| DbError::internal("duration month value out of range"))?;
                }
                num_buf.clear();
            }
            'W' | 'w' if !in_time => {
                let n: f64 = num_buf
                    .parse()
                    .map_err(|_| DbError::internal("Invalid week in duration"))?;
                // Handle fractional weeks: 2.5W = 17 days + 12 hours
                let total_days = n * 7.0;
                let whole_days = f64_to_i32(total_days.trunc())?;
                let frac_days = total_days.fract();
                days += whole_days;
                micros += f64_to_i64(frac_days * MICROS_PER_DAY_F)
                    .map_err(|_| DbError::internal("duration week value out of range"))?;
                num_buf.clear();
            }
            'D' | 'd' if !in_time => {
                let n: f64 = num_buf
                    .parse()
                    .map_err(|_| DbError::internal("Invalid day in duration"))?;
                // Handle fractional days: 1.5D = 1 day + 12 hours
                let whole_days = f64_to_i32(n.trunc())?;
                let frac_days = n.fract();
                days += whole_days;
                micros += f64_to_i64(frac_days * MICROS_PER_DAY_F)
                    .map_err(|_| DbError::internal("duration day value out of range"))?;
                num_buf.clear();
            }
            'H' | 'h' if in_time => {
                let n: f64 = num_buf
                    .parse()
                    .map_err(|_| DbError::internal("Invalid hour in duration"))?;
                micros += f64_to_i64(n * MICROS_PER_HOUR_F)
                    .map_err(|_| DbError::internal("duration hour value out of range"))?;
                num_buf.clear();
            }
            'M' | 'm' if in_time => {
                let n: f64 = num_buf
                    .parse()
                    .map_err(|_| DbError::internal("Invalid minute in duration"))?;
                micros += f64_to_i64(n * MICROS_PER_MINUTE_F)
                    .map_err(|_| DbError::internal("duration minute value out of range"))?;
                num_buf.clear();
            }
            'S' | 's' if in_time => {
                let n: f64 = num_buf
                    .parse()
                    .map_err(|_| DbError::internal("Invalid second in duration"))?;
                micros += f64_to_i64(n * MICROS_PER_SECOND_F)
                    .map_err(|_| DbError::internal("duration second value out of range"))?;
                num_buf.clear();
            }
            _ => {}
        }
    }

    Ok(IntervalValue {
        months,
        days,
        micros,
    })
}

/// Parse date-like duration format: YYYY-MM-DDThh:mm:ss[.sss]
/// e.g. "2012-02-02T14:37:21.545"
fn parse_date_like_duration(s: &str) -> DbResult<IntervalValue> {
    let (date_part, time_part) = if let Some(t_pos) = s.find('T') {
        (&s[..t_pos], Some(&s[t_pos + 1..]))
    } else {
        (s, None)
    };

    let date_parts: Vec<&str> = date_part.split('-').collect();
    let years: i32 = date_parts.first().and_then(|p| p.parse().ok()).unwrap_or(0);
    let month_val: i32 = date_parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
    let day_val: i32 = date_parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);

    let months = years * 12 + month_val;
    let days = day_val;
    let mut micros: i64 = 0;

    if let Some(tp) = time_part {
        let parts: Vec<&str> = tp.split(':').collect();
        let hours: i64 = parts.first().and_then(|p| p.parse().ok()).unwrap_or(0);
        let minutes: i64 = parts
            .get(1)
            .and_then(|p| p.split('.').next())
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        let (seconds, frac_micros) = if let Some(sec_str) = parts.get(2) {
            if let Some(dot_pos) = sec_str.find('.') {
                let sec: i64 = sec_str[..dot_pos].parse().unwrap_or(0);
                let frac_str = &sec_str[dot_pos..];
                let frac: f64 = frac_str.parse().unwrap_or(0.0);
                (
                    sec,
                    f64_to_i64(frac * MICROS_PER_SECOND_F)
                        .map_err(|_| DbError::internal("duration second value out of range"))?,
                )
            } else {
                (sec_str.parse().unwrap_or(0), 0i64)
            }
        } else {
            (0, 0)
        };
        micros = hours * MICROS_PER_HOUR
            + minutes * MICROS_PER_MINUTE
            + seconds * MICROS_PER_SECOND
            + frac_micros;
    }

    Ok(IntervalValue {
        months,
        days,
        micros,
    })
}

fn build_duration_from_map(map: &serde_json::Value) -> DbResult<Value> {
    let obj = map
        .as_object()
        .ok_or_else(|| DbError::internal("duration() map argument must be a JSON object"))?;

    let years = json_i64(obj, "years").unwrap_or(0);
    let months_val = json_i64(obj, "months").unwrap_or(0);
    let weeks = json_i64(obj, "weeks").unwrap_or(0);
    let days_val = json_i64(obj, "days").unwrap_or(0);
    let hours = json_i64(obj, "hours").unwrap_or(0);
    let minutes = json_i64(obj, "minutes").unwrap_or(0);
    let seconds = json_i64(obj, "seconds").unwrap_or(0);
    let milliseconds = json_i64(obj, "milliseconds").unwrap_or(0);
    let micros_val = json_i64(obj, "microseconds").unwrap_or(0);
    let nanoseconds = json_i64(obj, "nanoseconds").unwrap_or(0);

    let total_months = i64_to_i32(
        years
            .checked_mul(12)
            .and_then(|v| v.checked_add(months_val))
            .ok_or_else(|| {
                DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    "interval out of range",
                ))
            })?,
    )?;
    let total_days = i64_to_i32(
        weeks
            .checked_mul(7)
            .and_then(|v| v.checked_add(days_val))
            .ok_or_else(|| {
                DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    "interval out of range",
                ))
            })?,
    )?;

    let total_micros = hours * MICROS_PER_HOUR
        + minutes * MICROS_PER_MINUTE
        + seconds * MICROS_PER_SECOND
        + milliseconds * MICROS_PER_MILLI
        + micros_val
        + nanoseconds / NANOS_PER_MICRO;

    Ok(Value::Interval(IntervalValue {
        months: total_months,
        days: total_days,
        micros: total_micros,
    }))
}

// ── Helpers ─────────────────────────────────────────────────────────────

pub(super) fn u8_to_month(m: u8) -> Option<Month> {
    match m {
        1 => Some(Month::January),
        2 => Some(Month::February),
        3 => Some(Month::March),
        4 => Some(Month::April),
        5 => Some(Month::May),
        6 => Some(Month::June),
        7 => Some(Month::July),
        8 => Some(Month::August),
        9 => Some(Month::September),
        10 => Some(Month::October),
        11 => Some(Month::November),
        12 => Some(Month::December),
        _ => None,
    }
}

/// Look up a key in a JSON object case-insensitively. Cypher map literal
/// keys flow through the SQL lexer which folds unquoted identifiers to
/// lowercase, so a Cypher `{dayOfWeek: 3}` arrives in the temporal
/// builder under JSONB key `dayofweek`. Helpers therefore try the
/// original key first, then its lowercase form.
fn jget<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<&'a serde_json::Value> {
    obj.get(key).or_else(|| {
        let lower = key.to_ascii_lowercase();
        if lower == key {
            None
        } else {
            obj.get(&lower)
        }
    })
}

pub(super) fn json_i32(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<i32> {
    jget(obj, key).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_f64().and_then(|f| f64_to_i64(f).ok()))
            .and_then(|n| i32::try_from(n).ok())
    })
}

pub(super) fn json_u8(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<u8> {
    jget(obj, key).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_f64().and_then(|f| f64_to_i64(f).ok()))
            .and_then(|n| u8::try_from(n).ok())
    })
}

pub(super) fn json_i64(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<i64> {
    jget(obj, key).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_f64().and_then(|f| f64_to_i64(f).ok()))
    })
}

pub(crate) fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NULL",
        Value::Boolean(_) => "Boolean",
        Value::Int(_) | Value::BigInt(_) => "Integer",
        Value::Real(_) | Value::Double(_) => "Float",
        Value::Text(_) => "String",
        Value::Array(_) => "List",
        Value::Date(_) => "Date",
        Value::Time(_) | Value::TimeTz(_, _) => "Time",
        Value::Timestamp(_) => "LocalDateTime",
        Value::TimestampTz(_) => "DateTime",
        Value::Interval(_) => "Duration",
        Value::Jsonb(_) => "Map",
        _ => "unsupported",
    }
}

pub(super) fn format_utc_offset(off: &UtcOffset) -> String {
    let (h, m, _) = off.as_hms();
    if m == 0 {
        format!("{h:+03}:00")
    } else {
        format!("{h:+03}:{:02}", m.unsigned_abs())
    }
}

/// Compute the ISO week-numbering year for a given date.
pub(super) fn iso_week_year(d: &Date) -> i32 {
    let year = d.year();
    let ordinal = d.ordinal();
    let week = d.iso_week();
    if ordinal <= 3 && week > 50 {
        year - 1
    } else if ordinal >= 363 && week == 1 {
        year + 1
    } else {
        year
    }
}

#[cfg(test)]
mod tests;
