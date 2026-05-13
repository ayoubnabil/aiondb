//! Shared PostgreSQL-compatible temporal formatting helpers.
//!
//! These functions produce the canonical text representations used by both
//! the pgwire text protocol and the SQL evaluation layer, avoiding
//! duplicated formatting logic across crates.
//!
//! Each formatter ships in two variants:
//!
//! - `format_*` returns an owned `String` (convenient for SQL evaluators that
//!   build derived values).
//! - `write_*_into` writes directly into a caller-provided `Vec<u8>` so the
//!   pgwire `DataRow` encoder can serialise a row without per-cell `String`
//!   allocation.
//!
//! The String variants are thin wrappers over the buffer ones, so both forms
//! share a single source of truth for digit layout.

use super::bounds::{is_infinity_date, timestamp_infinity_label};

// ---------------------------------------------------------------------------
// Low-level digit emitters
// ---------------------------------------------------------------------------

#[inline]
fn ascii_digit(value: u32) -> u8 {
    debug_assert!(value < 10);
    b'0' + u8::try_from(value).unwrap_or_default()
}

/// Append a two-digit zero-padded value (00..=99). Wider values are wrapped
/// modulo 100 to keep the layout predictable; callers that need wider fields
/// must use a different helper.
#[inline]
fn push_pad2(buf: &mut Vec<u8>, v: u32) {
    let v = v % 100;
    buf.push(ascii_digit(v / 10));
    buf.push(ascii_digit(v % 10));
}

/// Append a four-digit zero-padded value (0000..=9999). Wider values fall
/// back to a fmt path so we always produce correct digits.
#[inline]
fn push_pad4(buf: &mut Vec<u8>, v: u32) {
    if v < 10_000 {
        buf.push(ascii_digit(v / 1000));
        buf.push(ascii_digit((v / 100) % 10));
        buf.push(ascii_digit((v / 10) % 10));
        buf.push(ascii_digit(v % 10));
    } else {
        use std::fmt::Write as _;
        let mut tmp = String::with_capacity(8);
        let _ = write!(tmp, "{v:04}");
        buf.extend_from_slice(tmp.as_bytes());
    }
}

/// Append a six-digit zero-padded value (000000..=999999) **with trailing
/// zeros trimmed**. PG renders fractional seconds as `.<digits>`, dropping any
/// trailing zeros; this helper writes exactly the kept digits and never the
/// dot itself.
#[inline]
fn push_micros_trimmed(buf: &mut Vec<u8>, micros: u32) {
    debug_assert!(micros <= 999_999);
    let mut digits = [0u8; 6];
    digits[0] = ascii_digit((micros / 100_000) % 10);
    digits[1] = ascii_digit((micros / 10_000) % 10);
    digits[2] = ascii_digit((micros / 1_000) % 10);
    digits[3] = ascii_digit((micros / 100) % 10);
    digits[4] = ascii_digit((micros / 10) % 10);
    digits[5] = ascii_digit(micros % 10);
    let mut end = 6;
    while end > 0 && digits[end - 1] == b'0' {
        end -= 1;
    }
    buf.extend_from_slice(&digits[..end]);
}

/// Append a year, sign-prefixed for negative values, padded to at least four
/// digits. Wider years (`time` crate's `large-dates` feature) are emitted
/// without padding beyond their natural width.
#[inline]
fn push_year(buf: &mut Vec<u8>, year: i32) {
    if year < 0 {
        buf.push(b'-');
        push_pad4(buf, year.unsigned_abs());
    } else {
        push_pad4(buf, year.cast_unsigned());
    }
}

/// Append `+OO` / `-OO` (whole-hour offset) followed by `:MM` when the minute
/// component is non-zero. Mirrors PG's `timestamptz` rendering. Sign is
/// derived from the *combined* offset: a sub-hour negative offset such as
/// `(0, -30)` (e.g. historical Liberia) must render as `-00:30`, not `+00:30`.
#[inline]
fn push_offset(buf: &mut Vec<u8>, offset_hours: i8, offset_minutes: i8) {
    let negative = offset_hours < 0 || offset_minutes < 0;
    buf.push(if negative { b'-' } else { b'+' });
    push_pad2(buf, u32::from(offset_hours.unsigned_abs()));
    if offset_minutes != 0 {
        buf.push(b':');
        push_pad2(buf, u32::from(offset_minutes.unsigned_abs()));
    }
}

// ---------------------------------------------------------------------------
// Vec<u8> writers (zero-alloc on the fast path)
// ---------------------------------------------------------------------------

/// Write a `PrimitiveDateTime` as ISO 8601 timestamp directly into `buf`.
pub fn write_timestamp_into(buf: &mut Vec<u8>, dt: &time::PrimitiveDateTime) {
    if let Some(label) = timestamp_infinity_label(dt.date(), dt.time()) {
        buf.extend_from_slice(label.as_bytes());
        return;
    }
    push_year(buf, dt.year());
    buf.push(b'-');
    push_pad2(buf, u32::from(u8::from(dt.month())));
    buf.push(b'-');
    push_pad2(buf, u32::from(dt.day()));
    buf.push(b' ');
    push_pad2(buf, u32::from(dt.hour()));
    buf.push(b':');
    push_pad2(buf, u32::from(dt.minute()));
    buf.push(b':');
    push_pad2(buf, u32::from(dt.second()));
    let micro = dt.microsecond();
    if micro != 0 {
        buf.push(b'.');
        push_micros_trimmed(buf, micro);
    }
}

/// Write a `Date` as ISO 8601 directly into `buf`.
pub fn write_date_into(buf: &mut Vec<u8>, d: time::Date) {
    if is_infinity_date(d) {
        if d.year() < 0 {
            buf.extend_from_slice(b"-infinity");
        } else {
            buf.extend_from_slice(b"infinity");
        }
        return;
    }
    push_year(buf, d.year());
    buf.push(b'-');
    push_pad2(buf, u32::from(u8::from(d.month())));
    buf.push(b'-');
    push_pad2(buf, u32::from(d.day()));
}

/// Write a `Time` as ISO 8601 directly into `buf`.
pub fn write_time_into(buf: &mut Vec<u8>, t: &time::Time) {
    push_pad2(buf, u32::from(t.hour()));
    buf.push(b':');
    push_pad2(buf, u32::from(t.minute()));
    buf.push(b':');
    push_pad2(buf, u32::from(t.second()));
    let micro = t.microsecond();
    if micro != 0 {
        buf.push(b'.');
        push_micros_trimmed(buf, micro);
    }
}

/// Write a `Time` with offset as `HH:MM:SS[.ffffff]+OO[:MM]` directly into `buf`.
pub fn write_timetz_into(buf: &mut Vec<u8>, t: &time::Time, offset: &time::UtcOffset) {
    write_time_into(buf, t);
    let (oh, om, _) = offset.as_hms();
    push_offset(buf, oh, om);
}

/// Write an `OffsetDateTime` as ISO 8601 timestamptz directly into `buf`.
pub fn write_timestamptz_into(buf: &mut Vec<u8>, odt: &time::OffsetDateTime) {
    push_year(buf, odt.year());
    buf.push(b'-');
    push_pad2(buf, u32::from(u8::from(odt.month())));
    buf.push(b'-');
    push_pad2(buf, u32::from(odt.day()));
    buf.push(b' ');
    push_pad2(buf, u32::from(odt.hour()));
    buf.push(b':');
    push_pad2(buf, u32::from(odt.minute()));
    buf.push(b':');
    push_pad2(buf, u32::from(odt.second()));
    let micro = odt.microsecond();
    if micro != 0 {
        buf.push(b'.');
        push_micros_trimmed(buf, micro);
    }
    let offset = odt.offset();
    let (oh, om, _) = offset.as_hms();
    push_offset(buf, oh, om);
}

// ---------------------------------------------------------------------------
// String wrappers (kept for SQL evaluator paths; share the writers above)
// ---------------------------------------------------------------------------

/// Format a `PrimitiveDateTime` as ISO 8601 timestamp: `YYYY-MM-DD HH:MM:SS[.ffffff]`.
///
/// Infinity sentinels are rendered as `"infinity"` / `"-infinity"`.
/// Trailing zeros in the fractional-second part are trimmed.
#[must_use]
pub fn format_timestamp(dt: &time::PrimitiveDateTime) -> String {
    let mut buf = Vec::with_capacity(32);
    write_timestamp_into(&mut buf, dt);
    String::from_utf8(buf).unwrap_or_default()
}

/// Format a `Date` as ISO 8601: `YYYY-MM-DD`.
///
/// Infinity sentinels are rendered as `"infinity"` / `"-infinity"`.
#[must_use]
pub fn format_date(d: time::Date) -> String {
    let mut buf = Vec::with_capacity(10);
    write_date_into(&mut buf, d);
    String::from_utf8(buf).unwrap_or_default()
}

/// Format a `Time` as ISO 8601 time: `HH:MM:SS[.ffffff]`.
///
/// Trailing zeros in the fractional-second part are trimmed.
#[must_use]
pub fn format_time(t: &time::Time) -> String {
    let mut buf = Vec::with_capacity(15);
    write_time_into(&mut buf, t);
    String::from_utf8(buf).unwrap_or_default()
}

/// Format a `Time` with timezone offset as `HH:MM:SS[.ffffff]+OO[:MM]`.
#[must_use]
pub fn format_timetz(t: &time::Time, offset: &time::UtcOffset) -> String {
    let mut buf = Vec::with_capacity(20);
    write_timetz_into(&mut buf, t, offset);
    String::from_utf8(buf).unwrap_or_default()
}

/// Format an `OffsetDateTime` as ISO 8601 with timezone: `YYYY-MM-DD HH:MM:SS[.ffffff]+OO[:MM]`.
///
/// Trailing zeros in the fractional-second part are trimmed.
#[must_use]
pub fn format_timestamptz(odt: &time::OffsetDateTime) -> String {
    let mut buf = Vec::with_capacity(35);
    write_timestamptz_into(&mut buf, odt);
    String::from_utf8(buf).unwrap_or_default()
}

/// Format a `PrimitiveDateTime` as the JSON/ISO 8601 timestamp expected by
/// `PostgreSQL`'s `to_json`/`to_jsonb`/`row_to_json`: `YYYY-MM-DDTHH:MM:SS[.ffffff]`.
///
/// Infinity sentinels are rendered as `"infinity"` / `"-infinity"`.
#[must_use]
pub fn format_timestamp_json(dt: &time::PrimitiveDateTime) -> String {
    if let Some(label) = timestamp_infinity_label(dt.date(), dt.time()) {
        return label.to_owned();
    }
    let mut out = String::with_capacity(30);
    write_timestamp_json_body(&mut out, dt.date(), dt.time(), dt.microsecond());
    out
}

/// Format an `OffsetDateTime` as the JSON/ISO 8601 timestamptz expected by
/// `PostgreSQL`'s `to_json`/`to_jsonb`/`row_to_json`: `YYYY-MM-DDTHH:MM:SS[.ffffff]+OO[:MM]`.
#[must_use]
pub fn format_timestamptz_json(odt: &time::OffsetDateTime) -> String {
    let date = odt.date();
    let time = odt.time();
    if let Some(label) = timestamp_infinity_label(date, time) {
        return label.to_owned();
    }
    let mut out = String::with_capacity(36);
    write_timestamp_json_body(&mut out, date, time, odt.microsecond());
    let (oh, om, _) = odt.offset().as_hms();
    let negative = oh < 0 || om < 0;
    let sign = if negative { '-' } else { '+' };
    let abs_hour_offset = oh.unsigned_abs();
    let abs_minute_offset = om.unsigned_abs();
    let _ = write!(out, "{sign}{abs_hour_offset:02}:{abs_minute_offset:02}");
    out
}

/// Push the `YYYY-MM-DDTHH:MM:SS[.fract]` body of a JSON-style timestamp
/// into `out`. Used by both `format_timestamp_json` and
/// `format_timestamptz_json` so the per-component `format!` cascade and
/// the previous `format!("{micro:06}").trim_end_matches('0')`
/// round-trip happen exactly once and into a shared accumulator.
fn write_timestamp_json_body(out: &mut String, date: time::Date, time: time::Time, micro: u32) {
    use std::fmt::Write as _;
    let _ = write!(
        out,
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        date.year(),
        u8::from(date.month()),
        date.day(),
        time.hour(),
        time.minute(),
        time.second(),
    );
    if micro == 0 {
        return;
    }
    // Trim trailing zeros from the 6-digit fractional part using a
    // stack scratch instead of `format!.trim_end_matches('0').to_owned()`.
    let mut digits = [0u8; 6];
    let mut n = micro;
    for slot in digits.iter_mut().rev() {
        *slot = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let mut end = digits.len();
    while end > 0 && digits[end - 1] == b'0' {
        end -= 1;
    }
    out.push('.');
    out.push_str(std::str::from_utf8(&digits[..end]).unwrap_or(""));
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

    #[test]
    fn timestamp_no_fractional() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::March, 15).unwrap(),
            Time::from_hms(10, 30, 45).unwrap(),
        );
        assert_eq!(format_timestamp(&dt), "2024-03-15 10:30:45");
    }

    #[test]
    fn timestamp_with_microseconds() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::January, 1).unwrap(),
            Time::from_hms_micro(0, 0, 0, 123_456).unwrap(),
        );
        assert_eq!(format_timestamp(&dt), "2024-01-01 00:00:00.123456");
    }

    #[test]
    fn timestamp_trailing_zeros_trimmed() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::June, 1).unwrap(),
            Time::from_hms_micro(12, 0, 0, 100_000).unwrap(),
        );
        assert_eq!(format_timestamp(&dt), "2024-06-01 12:00:00.1");
    }

    #[test]
    fn date_basic() {
        let d = Date::from_calendar_date(2024, Month::June, 15).unwrap();
        assert_eq!(format_date(d), "2024-06-15");
    }

    #[test]
    fn time_no_fractional() {
        let t = Time::from_hms(10, 30, 45).unwrap();
        assert_eq!(format_time(&t), "10:30:45");
    }

    #[test]
    fn time_with_microseconds() {
        let t = Time::from_hms_micro(12, 34, 56, 789_000).unwrap();
        assert_eq!(format_time(&t), "12:34:56.789");
    }

    #[test]
    fn timetz_positive_offset() {
        let t = Time::from_hms_micro(12, 34, 56, 789_000).unwrap();
        let off = UtcOffset::from_hms(5, 30, 0).unwrap();
        assert_eq!(format_timetz(&t, &off), "12:34:56.789+05:30");
    }

    #[test]
    fn timetz_negative_whole_hour() {
        let t = Time::from_hms(1, 2, 3).unwrap();
        let off = UtcOffset::from_hms(-8, 0, 0).unwrap();
        assert_eq!(format_timetz(&t, &off), "01:02:03-08");
    }

    #[test]
    fn timestamptz_utc() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::March, 15).unwrap(),
            Time::from_hms(10, 30, 45).unwrap(),
        );
        let odt = dt.assume_utc();
        assert_eq!(format_timestamptz(&odt), "2024-03-15 10:30:45+00");
    }

    #[test]
    fn timestamptz_half_hour_offset() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::March, 15).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        );
        let odt = dt.assume_offset(UtcOffset::from_hms(5, 30, 0).unwrap());
        assert_eq!(format_timestamptz(&odt), "2024-03-15 12:00:00+05:30");
    }

    /// POC: a sub-hour NEGATIVE offset (e.g. historical Liberia at
    /// `-00:44:30`, or `(0, -30)` from `UtcOffset::as_hms`) must render
    /// the sign as `-`, not `+`. Pre-fix `push_offset` lost the sign
    /// when `offset_hours == 0` and emitted `+00:30` for `(0, -30)`.
    #[test]
    fn timestamptz_negative_subhour_offset_keeps_sign() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::March, 15).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        );
        let neg_offset = UtcOffset::from_whole_seconds(-1800).unwrap();
        let odt = dt.assume_offset(neg_offset);
        let rendered = format_timestamptz(&odt);
        assert!(
            rendered.contains("-00:30"),
            "expected '-00:30' in {rendered}"
        );
        assert!(
            !rendered.contains("+00:30"),
            "must not lose sign in {rendered}"
        );
    }
}
use std::fmt::Write as _;
