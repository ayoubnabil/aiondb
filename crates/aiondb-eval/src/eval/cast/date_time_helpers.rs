#![allow(clippy::many_single_char_names)]

use crate::eval::scalar_functions::value_convert::{f64_to_i64, i64_to_f64};
use crate::eval::session::current_time_zone;
use aiondb_core::{TimeZoneSetting, Value};

use super::date_time::MONTHS;
use super::time_parse::{parse_pg_time_components, ParsedTime, TimeParseError};

/// Parse time components supporting HH:MM, HH:MM:SS, HH:MM:SS.frac,
/// and compact HHMM, HHMMSS, HHMMSS.frac formats.
/// Also strips optional timezone suffix and optional 'T' prefix.
pub(super) fn parse_time_components(s: &str) -> Result<time::Time, ()> {
    parse_pg_time_components(s)
        .map(|parsed| parsed.time)
        .map_err(|_| ())
}

#[cfg(test)]
pub(super) fn parse_timetz(s: &str) -> Result<Value, ()> {
    parse_timetz_detailed(s).map_err(|_| ())
}

pub(super) fn parse_timetz_detailed(s: &str) -> Result<Value, TimeParseError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(TimeParseError::Invalid);
    }

    let (base, zone_token) = split_timetz_zone(s);
    let (date, time) = parse_timetz_base(base)?;

    let offset = match zone_token {
        Some(token) => {
            if date.is_none() && zone_token_requires_date(token) {
                return Err(TimeParseError::Invalid);
            }
            resolve_timetz_offset(token, date)?
        }
        None => default_timetz_offset(date),
    };

    Ok(Value::TimeTz(time.time, offset))
}

fn session_timezone() -> TimeZoneSetting {
    current_time_zone()
}

fn session_now_local() -> time::OffsetDateTime {
    let timezone = session_timezone();
    let now = time::OffsetDateTime::now_utc();
    let (offset, _) = timezone.parts_for_utc(now);
    now.to_offset(offset)
}

fn is_valid_timestamp_zone_token(token: &str) -> bool {
    token.eq_ignore_ascii_case("z")
        || token.eq_ignore_ascii_case("zulu")
        || TimeZoneSetting::try_parse(token).is_some()
        || parse_utc_offset(token).is_ok()
}

fn strip_timestamp_zone_or_error(input: &str) -> Result<&str, ()> {
    let trimmed = input.trim();
    if let Some((base, token)) = split_trailing_zone_token(trimmed) {
        if token.eq_ignore_ascii_case("BC") || token.eq_ignore_ascii_case("AD") {
            return Ok(trimmed);
        }
        if is_valid_timestamp_zone_token(token) {
            return Ok(base.trim_end());
        }
        if token.contains('/') {
            return Err(());
        }
        return Ok(trimmed);
    }
    Ok(trimmed)
}

pub(super) fn parse_timestamp(s: &str) -> Result<Value, ()> {
    use super::date_time::parse_date_components;
    let s = s.trim();

    if s.eq_ignore_ascii_case("now") || s.eq_ignore_ascii_case("today") {
        let now = session_now_local();
        return Ok(Value::Timestamp(time::PrimitiveDateTime::new(
            now.date(),
            if s.eq_ignore_ascii_case("now") {
                now.time()
            } else {
                time::Time::MIDNIGHT
            },
        )));
    }
    if s.eq_ignore_ascii_case("yesterday") {
        let d = session_now_local().date() - time::Duration::days(1);
        return Ok(Value::Timestamp(time::PrimitiveDateTime::new(
            d,
            time::Time::MIDNIGHT,
        )));
    }
    if s.eq_ignore_ascii_case("tomorrow") {
        let d = session_now_local().date() + time::Duration::days(1);
        return Ok(Value::Timestamp(time::PrimitiveDateTime::new(
            d,
            time::Time::MIDNIGHT,
        )));
    }

    if let Some(relative) = try_parse_relative_keyword_timestamp(s) {
        return Ok(relative);
    }

    let s = strip_timestamp_zone_or_error(s)?;
    if s.eq_ignore_ascii_case("now") || s.eq_ignore_ascii_case("today") {
        let now = session_now_local();
        return Ok(Value::Timestamp(time::PrimitiveDateTime::new(
            now.date(),
            if s.eq_ignore_ascii_case("now") {
                now.time()
            } else {
                time::Time::MIDNIGHT
            },
        )));
    }
    if s.eq_ignore_ascii_case("yesterday") {
        let d = session_now_local().date() - time::Duration::days(1);
        return Ok(Value::Timestamp(time::PrimitiveDateTime::new(
            d,
            time::Time::MIDNIGHT,
        )));
    }
    if s.eq_ignore_ascii_case("tomorrow") {
        let d = session_now_local().date() + time::Duration::days(1);
        return Ok(Value::Timestamp(time::PrimitiveDateTime::new(
            d,
            time::Time::MIDNIGHT,
        )));
    }
    if let Some(relative) = try_parse_relative_keyword_timestamp(s) {
        return Ok(relative);
    }

    let s_clean = strip_trailing_tz_name(s);
    if let Ok(date) = parse_date_components(s_clean) {
        return Ok(Value::Timestamp(time::PrimitiveDateTime::new(
            date,
            time::Time::MIDNIGHT,
        )));
    }
    if let Some((date_str, time_str)) = s_clean.split_once(' ') {
        if time_str.trim().eq_ignore_ascii_case("allballs") {
            let date = parse_date_components(date_str.trim())?;
            return Ok(Value::Timestamp(time::PrimitiveDateTime::new(
                date,
                time::Time::MIDNIGHT,
            )));
        }
    }

    // Try PG ctime format: [DayOfWeek] Month Day HH:MM:SS[.frac] Year [TZ]
    // e.g., "Mon Feb 10 17:32:01 1997" or "Feb 10 17:32:01 1997"
    if let Some(v) = try_parse_ctime_timestamp(s_clean) {
        return Ok(v);
    }

    // Try abbreviated format: "97FEB10 5:32:01PM"
    if let Some(v) = try_parse_abbrev_timestamp(s_clean) {
        return Ok(v);
    }

    if let Some((date_str, time_str)) = split_timestamp_date_time(s_clean) {
        let mut date_str = date_str.trim().to_owned();
        let mut time_str = time_str.trim().to_owned();
        if let Some(stripped) = time_str
            .strip_suffix(" BC")
            .or_else(|| time_str.strip_suffix(" bc"))
        {
            date_str.push_str(" BC");
            time_str = stripped.trim_end().to_owned();
        } else if let Some(stripped) = time_str
            .strip_suffix(" AD")
            .or_else(|| time_str.strip_suffix(" ad"))
        {
            date_str.push_str(" AD");
            time_str = stripped.trim_end().to_owned();
        }
        // Avoid the `to_ascii_lowercase()` String alloc - match the
        // 4-token sentinel set via `eq_ignore_ascii_case`.
        if date_str.eq_ignore_ascii_case("epoch")
            || date_str.eq_ignore_ascii_case("infinity")
            || date_str.eq_ignore_ascii_case("+infinity")
            || date_str.eq_ignore_ascii_case("-infinity")
        {
            return Err(());
        }
        if time_str.is_empty() {
            let date = parse_date_components(&date_str)?;
            return Ok(Value::Timestamp(time::PrimitiveDateTime::new(
                date,
                time::Time::MIDNIGHT,
            )));
        }
        if let Ok(date) = parse_date_components(&date_str) {
            if let Ok(time) = parse_time_components(&time_str) {
                return Ok(Value::Timestamp(time::PrimitiveDateTime::new(date, time)));
            }
        }
        // Fallback: try ctime on the full string.
        if let Some(v) = try_parse_ctime_timestamp(s_clean) {
            return Ok(v);
        }
        Err(())
    } else {
        // Date-only string: treat as midnight
        let date = parse_date_components(s_clean)?;
        Ok(Value::Timestamp(time::PrimitiveDateTime::new(
            date,
            time::Time::MIDNIGHT,
        )))
    }
}

pub(super) fn parse_timestamp_tz(s: &str) -> Result<Value, ()> {
    let timezone = session_timezone();
    if s.eq_ignore_ascii_case("now") {
        return Ok(Value::TimestampTz(time::OffsetDateTime::now_utc()));
    }
    if s.eq_ignore_ascii_case("today")
        || s.eq_ignore_ascii_case("yesterday")
        || s.eq_ignore_ascii_case("tomorrow")
    {
        let base = session_now_local().date();
        // Same `eq_ignore_ascii_case` checks as the outer guard  -
        // avoid the redundant `to_ascii_lowercase()` String alloc.
        let date = if s.eq_ignore_ascii_case("today") {
            base
        } else if s.eq_ignore_ascii_case("yesterday") {
            base - time::Duration::days(1)
        } else if s.eq_ignore_ascii_case("tomorrow") {
            base + time::Duration::days(1)
        } else {
            return Err(());
        };
        return Ok(Value::TimestampTz(timezone.apply_to_local(
            time::PrimitiveDateTime::new(date, time::Time::MIDNIGHT),
        )));
    }

    // Also handle trailing 'Z' as UTC
    if let Some(base) = s.strip_suffix('Z') {
        let ts = parse_timestamp(base)?;
        let Value::Timestamp(pdt) = ts else {
            return Err(());
        };
        return Ok(Value::TimestampTz(pdt.assume_utc()));
    }

    // Strip "zulu" suffix
    let s = if let Some(base) = s
        .strip_suffix("zulu")
        .or_else(|| s.strip_suffix("Zulu"))
        .or_else(|| s.strip_suffix("ZULU"))
    {
        base.trim()
    } else {
        s
    };

    if let Some(base) = s.strip_suffix(" MET DST") {
        let ts = parse_timestamp(base.trim_end())?;
        let Value::Timestamp(pdt) = ts else {
            return Err(());
        };
        let offset = time::UtcOffset::from_hms(2, 0, 0).map_err(|_| ())?;
        return Ok(Value::TimestampTz(pdt.assume_offset(offset)));
    }

    if let Some((base, token)) = split_trailing_zone_token(s) {
        if token.eq_ignore_ascii_case("BC") || token.eq_ignore_ascii_case("AD") {
            // Era marker, not a timezone token.
        } else if let Some(zone) = TimeZoneSetting::try_parse(token) {
            let ts = parse_timestamp(base)?;
            let Value::Timestamp(pdt) = ts else {
                return Err(());
            };
            return Ok(Value::TimestampTz(zone.apply_to_local(pdt)));
        } else if let Ok(offset) = parse_utc_offset(token) {
            let ts = parse_timestamp(base)?;
            let Value::Timestamp(pdt) = ts else {
                return Err(());
            };
            return Ok(Value::TimestampTz(pdt.assume_offset(offset)));
        } else if token.contains('/') {
            return Err(());
        }
    }

    // Handle ctime-style with embedded TZ before year:
    // "[DayOfWeek] Month Day HH:MM:SS TzName[+/-offset] Year"
    // e.g., "Wed Jul 11 10:51:14 GMT-4 2001", "Mon Feb 10 17:32:01 PST 1997"
    if let Some(result) = self::try_parse_ctime_timestamp_tz(s) {
        return Ok(result);
    }

    if let Some(result) = try_parse_fractional_julian_timestamptz(s) {
        return Ok(result);
    }

    // Find offset separator: look for +/- after position 10 (past YYYY-MM-DD)
    let min_pos = if s.starts_with('J') || s.starts_with('j') {
        1
    } else {
        10.min(s.len())
    };
    let offset_start = s[min_pos..]
        .rfind('+')
        .or_else(|| s[min_pos..].rfind('-'))
        .map(|pos| pos + min_pos);
    // If no timezone offset found, treat as UTC
    let Some(offset_start) = offset_start else {
        let ts = parse_timestamp(s)?;
        let Value::Timestamp(pdt) = ts else {
            return Err(());
        };
        if contains_allballs_time(s) {
            return Ok(Value::TimestampTz(pdt.assume_utc()));
        }
        return Ok(Value::TimestampTz(timezone.apply_to_local(pdt)));
    };

    let datetime_str = s[..offset_start].trim_end();
    let offset_str = &s[offset_start..];

    let datetime_input = if let Some(era) = offset_str
        .split_whitespace()
        .find(|token| token.eq_ignore_ascii_case("BC") || token.eq_ignore_ascii_case("AD"))
    {
        format!("{datetime_str} {era}")
    } else {
        datetime_str.to_owned()
    };

    let ts = parse_timestamp(&datetime_input)?;
    let Value::Timestamp(pdt) = ts else {
        return Err(());
    };

    let offset = parse_utc_offset(offset_str)?;
    Ok(Value::TimestampTz(pdt.assume_offset(offset)))
}

fn try_parse_fractional_julian_timestamptz(s: &str) -> Option<Value> {
    if !s.starts_with('J') && !s.starts_with('j') {
        return None;
    }
    let (base, offset_str) = split_inline_numeric_offset(s)?;
    let julian: f64 = base[1..].parse().ok()?;
    if julian.fract() == 0.0 {
        return None;
    }

    // Bounds-check before narrowing: valid PostgreSQL Julian days fit well within i64,
    // but guard against NaN/infinity or extreme float values.
    if !julian.is_finite() || julian < i64_to_f64(i64::MIN) || julian > i64_to_f64(i64::MAX) {
        return None;
    }
    let whole_days = f64_to_i64(julian.floor()).ok()?;
    let date = julian_to_date(whole_days).ok()?;
    let day_fraction = julian - i64_to_f64(whole_days);
    // day_fraction is in [0, 1) because julian.fract() != 0 was checked above and
    // whole_days = floor(julian).  The multiplication result is in [0, 86_400_000_000).
    let total_micros_f = (day_fraction * 86_400_000_000.0).round();
    if !(0.0..86_400_000_000.0).contains(&total_micros_f) {
        return None;
    }
    let total_micros = f64_to_i64(total_micros_f).ok()?;
    // All divisions produce values in the safe u8/u32 ranges given total_micros ∈ [0, 86_400_000_000).
    let hours = u8::try_from(total_micros / 3_600_000_000).ok()?;
    let minutes = u8::try_from((total_micros % 3_600_000_000) / 60_000_000).ok()?;
    let seconds = u8::try_from((total_micros % 60_000_000) / 1_000_000).ok()?;
    let micros = u32::try_from(total_micros % 1_000_000).ok()?;
    let time = time::Time::from_hms_micro(hours, minutes, seconds, micros).ok()?;
    let offset = parse_utc_offset(offset_str).ok()?;
    Some(Value::TimestampTz(
        time::PrimitiveDateTime::new(date, time).assume_offset(offset),
    ))
}

pub(super) fn parse_utc_offset(s: &str) -> Result<time::UtcOffset, ()> {
    let (sign, rest) = if let Some(r) = s.strip_prefix('+') {
        (1i8, r)
    } else if let Some(r) = s.strip_prefix('-') {
        (-1i8, r)
    } else {
        return Err(());
    };
    // Strip trailing timezone name if present (e.g., "+03:00" in "PST+03:00")
    let rest = rest
        .split_whitespace()
        .next()
        .unwrap_or(rest)
        .trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let parts: Vec<&str> = rest.split(':').collect();
    let (hours, minutes, seconds) = if parts.len() > 1 {
        let hours = parts[0].parse::<i8>().map_err(|_| ())? * sign;
        let minutes = parts[1].parse::<i8>().map_err(|_| ())? * sign;
        (hours, minutes, 0)
    } else {
        match rest.len() {
            1 | 2 => (rest.parse::<i8>().map_err(|_| ())? * sign, 0, 0),
            4 => {
                let hours = rest[..2].parse::<i8>().map_err(|_| ())? * sign;
                let minutes = rest[2..4].parse::<i8>().map_err(|_| ())? * sign;
                (hours, minutes, 0)
            }
            6 => {
                let hours = rest[..2].parse::<i8>().map_err(|_| ())? * sign;
                let minutes = rest[2..4].parse::<i8>().map_err(|_| ())? * sign;
                let seconds = rest[4..6].parse::<i8>().map_err(|_| ())? * sign;
                (hours, minutes, seconds)
            }
            _ => return Err(()),
        }
    };
    time::UtcOffset::from_hms(hours, minutes, seconds).map_err(|_| ())
}

fn parse_timetz_base(s: &str) -> Result<(Option<time::Date>, ParsedTime), TimeParseError> {
    match parse_pg_time_components(s) {
        Ok(time) => return Ok((None, time)),
        Err(TimeParseError::OutOfRange) => return Err(TimeParseError::OutOfRange),
        Err(TimeParseError::Invalid) => {}
    }
    if let Ok(Value::Timestamp(pdt)) = parse_timestamp(s) {
        return Ok((
            Some(pdt.date()),
            ParsedTime {
                time: pdt.time(),
                display_end_of_day: false,
            },
        ));
    }
    Err(TimeParseError::Invalid)
}

fn split_timetz_zone(s: &str) -> (&str, Option<&str>) {
    if let Some((base, token)) = split_trailing_zone_token(s) {
        return (base, Some(token));
    }
    if let Some((base, token)) = split_inline_numeric_offset(s) {
        return (base, Some(token));
    }
    (s, None)
}

fn split_trailing_zone_token(s: &str) -> Option<(&str, &str)> {
    let (base, token) = s.rsplit_once(char::is_whitespace)?;
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    if token.eq_ignore_ascii_case("AM") || token.eq_ignore_ascii_case("PM") {
        return None;
    }
    let looks_like_zone = token.contains('/')
        || token.starts_with('+')
        || token.starts_with('-')
        || token.chars().any(|ch| ch.is_ascii_alphabetic())
            && token
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '/' | '+' | '-' | ':'));
    looks_like_zone.then_some((base.trim_end(), token))
}

fn split_inline_numeric_offset(s: &str) -> Option<(&str, &str)> {
    let search_start = s
        .rfind(char::is_whitespace)
        .map_or(0, |idx| idx.saturating_add(1));
    let suffix = &s[search_start..];
    let plus = suffix.rfind('+').map(|idx| idx + search_start);
    let minus = suffix.rfind('-').map(|idx| idx + search_start);
    let candidate = plus.into_iter().chain(minus).max()?;
    let token = &s[candidate..];
    if !looks_like_numeric_offset(token) {
        return None;
    }
    Some((s[..candidate].trim_end(), token))
}

fn looks_like_numeric_offset(token: &str) -> bool {
    let Some(rest) = token.strip_prefix(['+', '-']) else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    let parts: Vec<&str> = rest.split(':').collect();
    if parts.len() > 2 {
        return false;
    }
    parts
        .iter()
        .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))
}

pub(super) fn has_date_dependent_zone_without_date(s: &str) -> bool {
    let Some((base, token)) = split_trailing_zone_token(s.trim()) else {
        return false;
    };
    zone_token_requires_date(token)
        && super::date_time::parse_date_components(base.trim()).is_err()
        && parse_timestamp(base.trim()).is_err()
}

fn zone_token_requires_date(token: &str) -> bool {
    token.contains('/')
}

fn split_timestamp_date_time(s: &str) -> Option<(&str, &str)> {
    if let Some((date_str, time_str)) = s.split_once(char::is_whitespace) {
        return Some((date_str.trim_end(), time_str.trim_start()));
    }

    let (date_str, time_str) = s.split_once('T')?;
    if date_str.is_empty() || time_str.is_empty() {
        return None;
    }
    super::date_time::parse_date_components(date_str.trim()).ok()?;
    Some((date_str.trim_end(), time_str.trim_start()))
}

fn resolve_timetz_offset(
    token: &str,
    date: Option<time::Date>,
) -> Result<time::UtcOffset, TimeParseError> {
    let zone = TimeZoneSetting::try_parse(token).ok_or(TimeParseError::Invalid)?;
    let date = date.unwrap_or_else(|| session_now_local().date());
    Ok(zone.offset_for_date(date))
}

pub(super) fn default_timetz_offset(date: Option<time::Date>) -> time::UtcOffset {
    let date = date.unwrap_or_else(|| session_now_local().date());
    session_timezone().offset_for_date(date)
}

fn try_parse_relative_keyword_timestamp(input: &str) -> Option<Value> {
    let trimmed = input.trim();
    for keyword in ["today", "tomorrow", "yesterday"] {
        if let Some(rest) = trimmed.strip_prefix(keyword) {
            let date = relative_keyword_date(keyword)?;
            let rest = rest.trim();
            if rest.is_empty() {
                return Some(Value::Timestamp(time::PrimitiveDateTime::new(
                    date,
                    time::Time::MIDNIGHT,
                )));
            }
            let time = parse_time_components(rest).ok()?;
            return Some(Value::Timestamp(time::PrimitiveDateTime::new(date, time)));
        }
        if let Some(rest) = trimmed.strip_suffix(keyword) {
            let date = relative_keyword_date(keyword)?;
            let time = parse_time_components(rest.trim()).ok()?;
            return Some(Value::Timestamp(time::PrimitiveDateTime::new(date, time)));
        }
    }
    None
}

fn relative_keyword_date(keyword: &str) -> Option<time::Date> {
    let today = session_now_local().date();
    match keyword {
        "today" => Some(today),
        "tomorrow" => Some(today + time::Duration::days(1)),
        "yesterday" => Some(today - time::Duration::days(1)),
        _ => None,
    }
}

fn contains_allballs_time(input: &str) -> bool {
    input
        .split_whitespace()
        .any(|token| token.eq_ignore_ascii_case("allballs"))
}

// ── Internal helpers ──────────────────────────────────────────────────

/// Strip " BC" suffix from date/timestamp strings. Returns the cleaned string
/// and whether BC was found.
pub(super) fn strip_bc(s: &str) -> (&str, bool) {
    if let Some(base) = s.strip_suffix(" BC") {
        (base, true)
    } else if let Some(base) = s.strip_suffix(" bc") {
        (base, true)
    } else if let Some(base) = s.strip_suffix(" AD") {
        (base, false)
    } else if let Some(base) = s.strip_suffix(" ad") {
        (base, false)
    } else {
        (s, false)
    }
}

/// Strip trailing timezone names from a time portion.
pub(super) fn strip_tz_suffix(s: &str) -> &str {
    if let Some((base, _)) = split_trailing_zone_token(s) {
        return base.trim_end();
    }
    if let Some((base, _)) = split_inline_numeric_offset(s) {
        return base.trim_end();
    }

    // Strip common timezone name suffixes
    let tz_names = [
        "PDT", "PST", "EDT", "EST", "CDT", "CST", "MDT", "MST", "UTC", "GMT", "IST", "JST",
    ];
    for tz in &tz_names {
        if let Some(base) = s.strip_suffix(tz) {
            return base.trim_end();
        }
    }
    // Strip IANA tz names like "America/Los_Angeles"
    if let Some(pos) = s.rfind(' ') {
        let candidate = &s[pos + 1..];
        if candidate.contains('/') {
            return s[..pos].trim_end();
        }
    }
    s
}

/// Strip trailing timezone name from a full timestamp string.
fn strip_trailing_tz_name(s: &str) -> &str {
    let tz_names = [
        "UTC", "GMT", "EST", "EDT", "CST", "CDT", "MST", "MDT", "PST", "PDT", "CET", "CEST", "EET",
        "EEST", "IST", "JST", "KST", "AEST", "AEDT", "HST", "BST",
    ];
    for tz in &tz_names {
        if let Some(base) = s.strip_suffix(tz) {
            return base.trim_end();
        }
    }
    // Strip "zulu"
    if let Some(base) = s
        .strip_suffix("zulu")
        .or_else(|| s.strip_suffix("Zulu"))
        .or_else(|| s.strip_suffix("ZULU"))
    {
        return base.trim_end();
    }
    s
}

/// Convert a two-digit year to a four-digit year.
/// If already 4+ digits, return as-is.
pub(super) fn infer_century(year: i32) -> i32 {
    if year >= 100 {
        year
    } else if year < 70 {
        2000 + year
    } else {
        1900 + year
    }
}

/// Keep years intact and let the `time` crate enforce the actual supported range.
pub(super) fn clamp_year(year: i32) -> i32 {
    year
}

/// Try to parse PG ctime-style timestamps:
/// `[DayOfWeek] Month Day HH:MM:SS[.frac] Year`
/// e.g., "Mon Feb 10 17:32:01 1997", "Feb 10 17:32:01 1997",
/// "Feb 16 17:32:01 1997", "Feb 10 5:32PM 1997"
fn try_parse_ctime_timestamp(s: &str) -> Option<Value> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }

    // Find the month token - skip leading day-of-week if present
    let mut idx = 0;
    let day_names = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let first_lower = parts[0].to_ascii_lowercase();
    if day_names.iter().any(|d| first_lower.starts_with(d)) {
        idx = 1;
    }

    if idx + 3 > parts.len() {
        return None;
    }

    // parts[idx] should be month name
    let month_lower = parts[idx].to_ascii_lowercase();
    let month_num = MONTHS
        .iter()
        .find(|(name, _)| month_lower.starts_with(name))
        .map(|(_, n)| *n)?;

    let day: u8 = parts[idx + 1].parse().ok()?;

    // parts[idx+2] should be time component (contains ':')
    let time_str = parts[idx + 2];
    if !time_str.contains(':') {
        return None;
    }

    // Handle AM/PM suffix: "5:32PM" -> separate time parsing
    let time = parse_time_with_ampm(time_str)?;

    // parts[idx+3] should be year (possibly negative)
    let year = parse_ctime_year(parts.get(idx + 3)?, parts.get(idx + 4).copied())?;

    let month = time::Month::try_from(month_num).ok()?;
    let date = time::Date::from_calendar_date(clamp_year(year), month, day).ok()?;
    Some(Value::Timestamp(time::PrimitiveDateTime::new(date, time)))
}

/// Try to parse PG ctime-style timestamps **with timezone**:
/// `[DayOfWeek] Month Day HH:MM:SS[.frac] TzName[+/-offset] Year`
/// e.g., "Wed Jul 11 10:51:14 GMT-4 2001", "Mon Feb 10 17:32:01 PST 1997"
///
/// In POSIX convention, `GMT-4` means UTC+4 (sign is inverted).
fn try_parse_ctime_timestamp_tz(s: &str) -> Option<Value> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 5 {
        return None;
    }

    // Find the month token - skip leading day-of-week if present
    let mut idx = 0;
    let day_names = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let first_lower = parts[0].to_ascii_lowercase();
    if day_names.iter().any(|d| first_lower.starts_with(d)) {
        idx = 1;
    }

    // We need at least: Month Day Time TZ Year
    if idx + 4 >= parts.len() {
        return None;
    }

    // parts[idx] should be month name
    let month_lower = parts[idx].to_ascii_lowercase();
    let month_num = MONTHS
        .iter()
        .find(|(name, _)| month_lower.starts_with(name))
        .map(|(_, n)| *n)?;

    let day: u8 = parts[idx + 1].parse().ok()?;

    // parts[idx+2] should be time component (contains ':')
    let time_str = parts[idx + 2];
    if !time_str.contains(':') {
        return None;
    }

    let time = parse_time_with_ampm(time_str)?;

    // parts[idx+3] could be a timezone token (letters or letters+/-digits)
    // parts[idx+4] should be the year
    let tz_token = parts[idx + 3];
    let year_token = parts.get(idx + 4)?;

    // If tz_token is purely numeric, this is not a tz - it's the year (no tz).
    // That case is handled by try_parse_ctime_timestamp.
    if tz_token.chars().all(|c| c.is_ascii_digit() || c == '-')
        && (tz_token.starts_with(|c: char| c.is_ascii_digit()) || tz_token.starts_with('-'))
    {
        // Looks like a year, not a tz name.  Try only if year_token is also valid.
        // But let the normal ctime handler deal with the 4-token case.
        return None;
    }

    let year = parse_ctime_year(year_token, parts.get(idx + 5).copied())?;

    // Parse the timezone token.  It may be:
    //   "GMT", "PST", "UTC" - a bare timezone name (offset 0 for simplicity)
    //   "GMT-4", "PST+03:00", "UTC+5:30" - tz name with offset
    //
    // In POSIX convention, the sign in "GMT-4" is inverted compared to ISO:
    //   GMT-4 means UTC+4.
    let month = time::Month::try_from(month_num).ok()?;
    let date = time::Date::from_calendar_date(clamp_year(year), month, day).ok()?;
    let pdt = time::PrimitiveDateTime::new(date, time);
    let timezone = TimeZoneSetting::try_parse(tz_token)?;
    Some(Value::TimestampTz(timezone.apply_to_local(pdt)))
}

/// Parse time with optional AM/PM suffix.
fn parse_time_with_ampm(s: &str) -> Option<time::Time> {
    parse_time_components(s).ok()
}

fn parse_ctime_year(year_token: &str, era_token: Option<&str>) -> Option<i32> {
    let raw_year: i32 = year_token.parse().ok()?;
    let digits = year_token.trim_start_matches('-').len();
    let mut year = if digits <= 2 {
        infer_century(raw_year)
    } else {
        raw_year
    };
    if era_token.is_some_and(|token| token.eq_ignore_ascii_case("BC")) {
        year = 1 - year;
    }
    Some(year)
}

/// Try to parse a PG-style timestamp with abbreviated year/month format.
/// e.g., "97/02/10 17:32:01" or "97FEB10 5:32:01PM"
fn try_parse_abbrev_timestamp(s: &str) -> Option<Value> {
    // "97FEB10 5:32:01PM" - letters embedded in date portion
    let lower = s.to_ascii_lowercase();
    for &(name, num) in &MONTHS {
        if let Some(pos) = lower.find(name) {
            let before = &s[..pos];
            let after = &s[pos + 3..];
            // before = year digits, after starts with day digits
            let year: i32 = before.parse().ok()?;
            // Extract day (digits before space) and time (after space)
            let (day_str, time_str) = after.split_once(' ')?;
            let day: u8 = day_str.parse().ok()?;
            let time = parse_time_with_ampm(time_str)?;
            let month = time::Month::try_from(num).ok()?;
            let date = time::Date::from_calendar_date(infer_century(year), month, day).ok()?;
            return Some(Value::Timestamp(time::PrimitiveDateTime::new(date, time)));
        }
    }
    None
}

/// Convert Julian Day Number to a Date.
pub(super) fn julian_to_date(jd: i64) -> Result<time::Date, ()> {
    // Julian day 0 = November 24, 4714 BC (proleptic Gregorian)
    // Algorithm from Meeus, "Astronomical Algorithms"
    let jd = jd + 32044;
    let g = jd / 146097;
    let dg = jd % 146097;
    let c = (dg / 36524 + 1) * 3 / 4;
    let dc = dg - c * 36524;
    let b = dc / 1461;
    let db = dc % 1461;
    let a = (db / 365 + 1) * 3 / 4;
    let da = db - a * 365;
    let y = g * 400 + c * 100 + b * 4 + a;
    let m = (da * 5 + 308) / 153 - 2;
    let d = da - (m + 4) * 153 / 5 + 122;
    let year_i64 = y - 4800 + (m + 2) / 12;
    let month_i64 = (m + 2) % 12 + 1;
    let day_i64 = d + 1;
    // year must fit i32; month is 1..=12; day is 1..=31 for any calendar.
    let year = i32::try_from(year_i64).map_err(|_| ())?;
    let month = u8::try_from(month_i64).map_err(|_| ())?;
    let day = u8::try_from(day_i64).map_err(|_| ())?;
    let month = time::Month::try_from(month).map_err(|_| ())?;
    time::Date::from_calendar_date(clamp_year(year), month, day).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{with_session_context, EvalSessionContext};
    use aiondb_core::{temporal::pg_timestamp_min, DataType};
    use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

    #[test]
    fn parse_time_components_accepts_space_before_offset() {
        let parsed = parse_time_components("T040506.789 +08").expect("time");
        assert_eq!(parsed, Time::from_hms_micro(4, 5, 6, 789_000).unwrap());
    }

    #[test]
    fn parse_timestamp_tz_accepts_julian_date_with_offset() {
        let parsed = parse_timestamp_tz("J2452271+08").expect("timestamptz");
        let Value::TimestampTz(ts) = parsed else {
            panic!("expected timestamptz");
        };
        let expected = PrimitiveDateTime::new(
            Date::from_calendar_date(2001, Month::December, 27).unwrap(),
            Time::MIDNIGHT,
        )
        .assume_offset(UtcOffset::from_hms(8, 0, 0).unwrap());
        assert_eq!(ts, expected);
    }

    #[test]
    fn parse_timestamp_accepts_allballs_keyword() {
        let parsed = parse_timestamp("2001-12-27 allballs").expect("timestamp");
        let Value::Timestamp(ts) = parsed else {
            panic!("expected timestamp");
        };
        let expected = PrimitiveDateTime::new(
            Date::from_calendar_date(2001, Month::December, 27).unwrap(),
            Time::MIDNIGHT,
        );
        assert_eq!(ts, expected);
    }

    #[test]
    fn parse_timestamp_tz_accepts_met_dst_suffix() {
        let parsed = parse_timestamp_tz("2001-12-27 04:05:06.789 MET DST").expect("timestamptz");
        let Value::TimestampTz(ts) = parsed else {
            panic!("expected timestamptz");
        };
        let expected = PrimitiveDateTime::new(
            Date::from_calendar_date(2001, Month::December, 27).unwrap(),
            Time::from_hms_micro(4, 5, 6, 789_000).unwrap(),
        )
        .assume_offset(UtcOffset::from_hms(2, 0, 0).unwrap());
        assert_eq!(ts, expected);
    }

    #[test]
    fn parse_timestamp_tz_preserves_inline_numeric_offset() {
        let parsed = parse_timestamp_tz("2000-11-27 00:00:00-08").expect("timestamptz");
        let Value::TimestampTz(ts) = parsed else {
            panic!("expected timestamptz");
        };
        let expected = PrimitiveDateTime::new(
            Date::from_calendar_date(2000, Month::November, 27).unwrap(),
            Time::MIDNIGHT,
        )
        .assume_offset(UtcOffset::from_hms(-8, 0, 0).unwrap());
        assert_eq!(ts, expected);
    }

    #[test]
    fn parse_timestamp_tz_accepts_compact_numeric_offsets() {
        for input in [
            "1997-02-10 17:32:01-0800",
            "19970210 173201 -0800",
            "Feb 10 17:32:01 1997 -0800",
            "1997/02/10 17:32:01-0800",
        ] {
            let parsed = parse_timestamp_tz(input).expect("timestamptz");
            let Value::TimestampTz(ts) = parsed else {
                panic!("expected timestamptz");
            };
            let expected = PrimitiveDateTime::new(
                Date::from_calendar_date(1997, Month::February, 10).unwrap(),
                Time::from_hms(17, 32, 1).unwrap(),
            )
            .assume_offset(UtcOffset::from_hms(-8, 0, 0).unwrap());
            assert_eq!(ts, expected, "input: {input}");
        }
    }

    #[test]
    fn split_trailing_zone_token_rejects_plain_time_of_day() {
        assert_eq!(split_trailing_zone_token("2000-11-27 12:00"), None);
    }

    #[test]
    fn parse_timestamp_tz_keeps_clock_time_when_no_zone_token_is_present() {
        let context = EvalSessionContext::from_settings(Some("Postgres, MDY"), Some("PST8PDT"));
        let parsed = with_session_context(context, || {
            parse_timestamp_tz("2000-11-27 12:00").expect("timestamptz")
        });
        let Value::TimestampTz(ts) = parsed else {
            panic!("expected timestamptz");
        };
        let expected = PrimitiveDateTime::new(
            Date::from_calendar_date(2000, Month::November, 27).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        )
        .assume_offset(UtcOffset::from_hms(-8, 0, 0).unwrap());
        assert_eq!(ts, expected);
    }

    #[test]
    fn parse_timestamp_ignores_posix_timezone_suffixes_for_plain_timestamp() {
        for (input, expected_time) in [
            (
                "2000-03-15 08:14:01 GMT+8",
                Time::from_hms(8, 14, 1).unwrap(),
            ),
            (
                "2000-03-15 13:14:02 GMT-1",
                Time::from_hms(13, 14, 2).unwrap(),
            ),
            (
                "2000-03-15 12:14:03 GMT-2",
                Time::from_hms(12, 14, 3).unwrap(),
            ),
            (
                "2000-03-15 03:14:04 PST+8",
                Time::from_hms(3, 14, 4).unwrap(),
            ),
            (
                "2000-03-15 02:14:05 MST+7:00",
                Time::from_hms(2, 14, 5).unwrap(),
            ),
        ] {
            let parsed = parse_timestamp(input).expect("timestamp");
            let Value::Timestamp(ts) = parsed else {
                panic!("expected timestamp");
            };
            let expected = PrimitiveDateTime::new(
                Date::from_calendar_date(2000, Month::March, 15).unwrap(),
                expected_time,
            );
            assert_eq!(ts, expected, "input={input}");
        }
    }

    #[test]
    fn parse_time_components_strips_compound_zone_token() {
        let parsed = parse_time_components("03:14:04 PST+8").expect("time");
        assert_eq!(parsed, Time::from_hms(3, 14, 4).unwrap());

        let parsed = parse_time_components("02:14:05 MST+7:00").expect("time");
        assert_eq!(parsed, Time::from_hms(2, 14, 5).unwrap());
    }

    #[test]
    fn parse_time_components_keeps_pm_suffix_as_meridiem() {
        let parsed = parse_time_components("11:59:59.99 PM").expect("time");
        assert_eq!(parsed, Time::from_hms_micro(23, 59, 59, 990_000).unwrap());
    }

    #[test]
    fn parse_timetz_keeps_pm_suffix_before_timezone_token() {
        let parsed = parse_timetz("11:59:59.99 PM PDT").expect("timetz");
        let Value::TimeTz(time, offset) = parsed else {
            panic!("expected timetz");
        };
        assert_eq!(time, Time::from_hms_micro(23, 59, 59, 990_000).unwrap());
        assert_eq!(offset, UtcOffset::from_hms(-7, 0, 0).unwrap());
    }

    #[test]
    fn parse_timestamp_accepts_iso_separator_without_corrupting_timezone_names() {
        let parsed = parse_timestamp("2001-09-22T18:19:20").expect("timestamp");
        let Value::Timestamp(ts) = parsed else {
            panic!("expected timestamp");
        };
        let expected = PrimitiveDateTime::new(
            Date::from_calendar_date(2001, Month::September, 22).unwrap(),
            Time::from_hms(18, 19, 20).unwrap(),
        );
        assert_eq!(ts, expected);

        let parsed = parse_timestamp("2000-03-15 08:14:01 GMT+8").expect("timestamp");
        let Value::Timestamp(ts) = parsed else {
            panic!("expected timestamp");
        };
        let expected = PrimitiveDateTime::new(
            Date::from_calendar_date(2000, Month::March, 15).unwrap(),
            Time::from_hms(8, 14, 1).unwrap(),
        );
        assert_eq!(ts, expected);
    }

    #[test]
    fn parse_timetz_rejects_date_dependent_zone_without_date() {
        assert!(parse_timetz("T040506.789 America/Los_Angeles").is_err());
        assert!(has_date_dependent_zone_without_date(
            "15:36:39 America/New_York"
        ));
    }

    #[test]
    fn parse_timetz_accepts_date_dependent_zone_with_date() {
        let parsed = parse_timetz("2001-12-27 T040506.789 America/Los_Angeles").expect("timetz");
        let Value::TimeTz(time, offset) = parsed else {
            panic!("expected timetz");
        };
        assert_eq!(time, Time::from_hms_micro(4, 5, 6, 789_000).unwrap());
        assert_eq!(offset, UtcOffset::from_hms(-8, 0, 0).unwrap());
        assert!(!has_date_dependent_zone_without_date(
            "2003-03-07 15:36:39 America/New_York"
        ));
    }

    #[test]
    fn parse_timestamp_tz_uses_dst_for_unzoned_summer_literals() {
        let context = EvalSessionContext::from_settings(Some("Postgres, MDY"), Some("PST8PDT"));
        let parsed = with_session_context(context, || {
            parse_timestamp_tz("2001-09-22T18:19:20").expect("timestamptz")
        });
        let Value::TimestampTz(ts) = parsed else {
            panic!("expected timestamptz");
        };
        let expected = PrimitiveDateTime::new(
            Date::from_calendar_date(2001, Month::September, 22).unwrap(),
            Time::from_hms(18, 19, 20).unwrap(),
        )
        .assume_offset(UtcOffset::from_hms(-7, 0, 0).unwrap());
        assert_eq!(ts, expected);
    }

    #[test]
    fn parse_timestamp_tz_uses_dst_for_named_new_york_summer_literals() {
        let parsed = parse_timestamp_tz("19970710 173201 America/New_York").expect("timestamptz");
        let Value::TimestampTz(ts) = parsed else {
            panic!("expected timestamptz");
        };
        let expected = PrimitiveDateTime::new(
            Date::from_calendar_date(1997, Month::July, 10).unwrap(),
            Time::from_hms(17, 32, 1).unwrap(),
        )
        .assume_offset(UtcOffset::from_hms(-4, 0, 0).unwrap());
        assert_eq!(ts, expected);
    }

    #[test]
    fn parse_timestamp_bc_min_boundary_minus_one_stays_below_pg_min() {
        let parsed = parse_timestamp("4714-11-23 23:59:59 BC").expect("timestamp");
        let Value::Timestamp(ts) = parsed else {
            panic!("expected timestamp");
        };
        assert!(ts < pg_timestamp_min());
    }

    #[test]
    fn parse_timestamp_tz_keeps_era_after_numeric_offset() {
        let parsed = parse_timestamp_tz("4714-11-23 23:59:59+00 BC").expect("timestamptz");
        let Value::TimestampTz(ts) = parsed else {
            panic!("expected timestamptz");
        };
        assert!(ts.year() <= 0);
    }

    #[test]
    fn cast_timestamp_rejects_bc_value_before_pg_min() {
        let err = crate::eval::cast::cast_value(
            Value::Text("4714-11-23 23:59:59 BC".to_owned()),
            &DataType::Timestamp,
        )
        .expect_err("expected out-of-range error");
        assert_eq!(
            err.report().message,
            "timestamp out of range: \"4714-11-23 23:59:59 BC\""
        );
    }

    #[test]
    fn cast_timestamptz_rejects_bc_value_before_pg_min() {
        let err = crate::eval::cast::cast_value(
            Value::Text("4714-11-23 23:59:59+00 BC".to_owned()),
            &DataType::TimestampTz,
        )
        .expect_err("expected out-of-range error");
        assert_eq!(
            err.report().message,
            "timestamp out of range: \"4714-11-23 23:59:59+00 BC\""
        );
    }
}
