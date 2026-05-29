//! Value-to-`PostgreSQL` text format serialization.
//!
//! Converts [`Value`] instances to their `PostgreSQL` text wire
//! representation. Used by the simple query protocol and by any
//! extended-query result columns that stay in text format. Binary
//! result encoding lives in [`crate::binary_format`].

use std::fmt::Write as _;

use aiondb_core::temporal::{
    format_date, format_time, format_timestamp, format_timestamptz, format_timetz,
};
use aiondb_core::value::pg_jsonb_to_string;
use aiondb_core::{NumericValue, Value};

// ---------------------------------------------------------------------------
// Zero-allocation integer-to-ASCII serializer
// ---------------------------------------------------------------------------

/// Write an `i32` as ASCII decimal digits directly into `buf`.
pub(crate) fn write_i32_ascii(buf: &mut Vec<u8>, v: i32) {
    if v == 0 {
        buf.push(b'0');
        return;
    }
    let mut tmp = [0u8; 11]; // max "-2147483648"
    let negative = v < 0;
    let mut n = if negative {
        i64::from(v).unsigned_abs()
    } else {
        u64::from(v.cast_unsigned())
    };
    let mut pos = tmp.len();
    while n > 0 {
        pos -= 1;
        tmp[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    if negative {
        pos -= 1;
        tmp[pos] = b'-';
    }
    buf.extend_from_slice(&tmp[pos..]);
}

/// Write an `i64` as ASCII decimal digits directly into `buf`.
pub(crate) fn write_i64_ascii(buf: &mut Vec<u8>, v: i64) {
    if v == 0 {
        buf.push(b'0');
        return;
    }
    let mut tmp = [0u8; 20]; // max "-9223372036854775808"
    let negative = v < 0;
    let mut n = v.unsigned_abs() as u128;
    let mut pos = tmp.len();
    while n > 0 {
        pos -= 1;
        tmp[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    if negative {
        pos -= 1;
        tmp[pos] = b'-';
    }
    buf.extend_from_slice(&tmp[pos..]);
}

/// Convert a [`Value`] directly to bytes, bypassing `String` allocation for
/// common fixed-size types. Returns `None` for `Value::Null`.
pub fn value_to_text_bytes(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::Null => None,
        Value::Int(v) => {
            let mut buf = Vec::with_capacity(11);
            write_i32_ascii(&mut buf, *v);
            Some(buf)
        }
        Value::BigInt(v) => {
            let mut buf = Vec::with_capacity(20);
            write_i64_ascii(&mut buf, *v);
            Some(buf)
        }
        Value::Boolean(v) => Some(if *v { b"t".to_vec() } else { b"f".to_vec() }),
        Value::Text(v) => Some(v.as_bytes().to_vec()),
        Value::Blob(v) => Some(format_bytea(v).into_bytes()),
        _ => value_to_text(value).map(String::into_bytes),
    }
}

/// Convert a [`Value`] to its `PostgreSQL` text format representation.
///
/// Returns `None` for `Value::Null` (the caller writes -1 as the column length).
pub fn value_to_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Int(v) => Some(v.to_string()),
        Value::BigInt(v) => Some(v.to_string()),
        Value::Real(v) => Some(format_float_f32(*v)),
        Value::Double(v) => Some(format_float_f64(*v)),
        Value::Numeric(v) => Some(format_numeric_value(v)),
        Value::Money(v) => Some(Value::Money(*v).to_string()),
        Value::Text(v) => Some(v.clone()),
        Value::Boolean(v) => Some(if *v { "t".to_string() } else { "f".to_string() }),
        Value::Blob(v) => Some(format_bytea(v)),
        Value::Timestamp(v) => Some(format_timestamp(v)),
        Value::Date(v) => Some(format_date(*v)),
        Value::LargeDate(v) => Some(v.to_string()),
        Value::Time(v) => Some(format_time(v)),
        Value::TimeTz(v, offset) => Some(format_timetz(v, offset)),
        Value::Interval(v) => Some(format_interval(v.months, v.days, v.micros)),
        Value::Tid(v) => Some(v.to_string()),
        Value::PgLsn(v) => Some(v.to_string()),
        Value::MacAddr(v) => Some(v.to_string()),
        Value::MacAddr8(v) => Some(v.to_string()),
        Value::Uuid(bytes) => Some(Value::Uuid(*bytes).to_string()),
        Value::TimestampTz(v) => Some(format_timestamptz(v)),
        Value::Jsonb(v) => Some(pg_jsonb_to_string(v)),
        Value::Vector(v) => Some(format_vector(&v.values)),
        Value::Array(elements) => Some(format_array(elements)),
    }
}

/// Format an f32 value, handling special cases.
fn format_float_f32(v: f32) -> String {
    if v.is_nan() {
        "NaN".to_string()
    } else if v == f32::INFINITY {
        "Infinity".to_string()
    } else if v == f32::NEG_INFINITY {
        "-Infinity".to_string()
    } else {
        v.to_string()
    }
}

/// Format an f64 value, handling special cases.
fn format_float_f64(v: f64) -> String {
    if v.is_nan() {
        "NaN".to_string()
    } else if v == f64::INFINITY {
        "Infinity".to_string()
    } else if v == f64::NEG_INFINITY {
        "-Infinity".to_string()
    } else {
        v.to_string()
    }
}

/// Write a NumericValue's PG text rendering directly into `buf`.
///
/// Intercepts NaN / Infinity / -Infinity sentinels (scale = u32::MAX) before
/// the digit loop. The historic bug fixed here was that the naive formatter
/// looped `(scale - digits_len)` times pushing zeros; on a special value
/// (scale = u32::MAX) this attempted a ~4 GiB allocation.
pub(crate) fn write_numeric_ascii(buf: &mut Vec<u8>, value: &NumericValue) {
    if value.is_nan() {
        buf.extend_from_slice(b"NaN");
        return;
    }
    if value.is_pos_infinity() {
        buf.extend_from_slice(b"Infinity");
        return;
    }
    if value.is_neg_infinity() {
        buf.extend_from_slice(b"-Infinity");
        return;
    }
    write_decimal_into(buf, value.coefficient, value.scale);
}

fn format_numeric_value(value: &NumericValue) -> String {
    let mut buf = Vec::with_capacity(16);
    write_numeric_ascii(&mut buf, value);
    String::from_utf8(buf).unwrap_or_default()
}

/// Write `coefficient` scaled by `10^-scale` as decimal digits into `buf`.
fn write_decimal_into(buf: &mut Vec<u8>, coefficient: i128, scale: u32) {
    if scale == 0 {
        let _ = write!(BufFmtWriter(buf), "{coefficient}");
        return;
    }

    let is_negative = coefficient < 0;
    if coefficient == i128::MIN {
        // i128::MIN cannot be negated; use the textual digits.
        let s = coefficient.to_string();
        let digits = &s[1..]; // strip the '-'
        write_decimal_str_into(buf, true, digits.as_bytes(), scale);
        return;
    }
    let abs_coeff = if is_negative {
        (-coefficient).cast_unsigned()
    } else {
        coefficient.cast_unsigned()
    };

    // Render the unsigned coefficient into a small stack buffer; u128::MAX has
    // 39 decimal digits.
    let mut digits = [0u8; 40];
    let mut pos = digits.len();
    if abs_coeff == 0 {
        pos -= 1;
        digits[pos] = b'0';
    } else {
        let mut n = abs_coeff;
        while n > 0 {
            pos -= 1;
            digits[pos] = b'0' + (n % 10) as u8;
            n /= 10;
        }
    }
    write_decimal_str_into(buf, is_negative, &digits[pos..], scale);
}

/// Insert a decimal point into a non-negative digit slice, writing into `buf`.
/// 64-byte ASCII '0' buffer used by `write_decimal_str_into` to emit the
/// leading-zero run of small-magnitude numerics (`coefficient < 10^scale`)
/// in chunks instead of one byte at a time. Mirrors the iter156 trick for
/// JSON pretty-printer indentation.
const DECIMAL_ZEROS: &[u8; 64] =
    b"0000000000000000000000000000000000000000000000000000000000000000";

fn write_decimal_str_into(buf: &mut Vec<u8>, is_negative: bool, digits: &[u8], scale: u32) {
    if is_negative {
        buf.push(b'-');
    }

    let digits_len_u32 = u32::try_from(digits.len()).unwrap_or(u32::MAX);
    if digits_len_u32 <= scale {
        buf.extend_from_slice(b"0.");
        // PG numerics carry full scale; emit the leading zeros explicitly.
        // `scale` is bounded by NumericValue invariants (special-value sentinels
        // are intercepted in write_numeric_ascii) so this loop is well-defined.
        let lead = (scale - digits_len_u32) as usize;
        if lead > 0 {
            buf.reserve(lead);
            let mut remaining = lead;
            while remaining > 0 {
                let chunk = remaining.min(DECIMAL_ZEROS.len());
                buf.extend_from_slice(&DECIMAL_ZEROS[..chunk]);
                remaining -= chunk;
            }
        }
        buf.extend_from_slice(digits);
    } else {
        let Ok(scale_usize) = usize::try_from(scale) else {
            buf.extend_from_slice(digits);
            return;
        };
        let split = digits.len() - scale_usize;
        buf.extend_from_slice(&digits[..split]);
        buf.push(b'.');
        buf.extend_from_slice(&digits[split..]);
    }
}

/// Adapter so we can `write!(..., "{}", x)` directly into a `Vec<u8>`.
struct BufFmtWriter<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for BufFmtWriter<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

/// Write an `f32`'s PG text rendering directly into `buf`.
pub(crate) fn write_f32_ascii(buf: &mut Vec<u8>, v: f32) {
    if v.is_nan() {
        buf.extend_from_slice(b"NaN");
    } else if v == f32::INFINITY {
        buf.extend_from_slice(b"Infinity");
    } else if v == f32::NEG_INFINITY {
        buf.extend_from_slice(b"-Infinity");
    } else {
        let _ = write!(BufFmtWriter(buf), "{v}");
    }
}

/// Write an `f64`'s PG text rendering directly into `buf`. Mirrors
/// `float8out`'s `%g` flavour: scientific notation for |v| ≥ 1e15 or
/// 0 < |v| < 1e-4, default decimal otherwise. Without this filter,
/// Rust's `{v}` Display emits `1e30` as
/// `1000000000000000000000000000000`, diverging from PG and breaking
/// libpq parsers that expect scientific form for those magnitudes.
pub(crate) fn write_f64_ascii(buf: &mut Vec<u8>, v: f64) {
    if v.is_nan() {
        buf.extend_from_slice(b"NaN");
    } else if v == f64::INFINITY {
        buf.extend_from_slice(b"Infinity");
    } else if v == f64::NEG_INFINITY {
        buf.extend_from_slice(b"-Infinity");
    } else if v == 0.0 {
        // Preserve the sign of zero (-0 → "-0") to match PG.
        if v.is_sign_negative() {
            buf.extend_from_slice(b"-0");
        } else {
            buf.push(b'0');
        }
    } else {
        let abs = v.abs();
        if (1e-4..1e15).contains(&abs) {
            let _ = write!(BufFmtWriter(buf), "{v}");
        } else {
            // Scientific notation, mirroring PG `%g` shape `1.23e+45`.
            let _ = write!(BufFmtWriter(buf), "{v:e}");
        }
    }
}

/// Write a UUID's canonical hyphenated lowercase rendering directly into `buf`.
pub(crate) fn write_uuid_ascii(buf: &mut Vec<u8>, bytes: &[u8; 16]) {
    // 8-4-4-4-12 layout
    let mut tmp = [0u8; 36];
    let groups: [(usize, usize); 5] = [(0, 4), (4, 6), (6, 8), (8, 10), (10, 16)];
    let mut out = 0;
    for (gi, (start, end)) in groups.iter().copied().enumerate() {
        if gi > 0 {
            tmp[out] = b'-';
            out += 1;
        }
        for &b in &bytes[start..end] {
            // Single byte-pair table lookup + 2-byte copy instead of
            // two separate `HEX[hi]` / `HEX[lo]` table reads.
            let pair = HEX_PAIRS[b as usize];
            tmp[out] = pair[0];
            tmp[out + 1] = pair[1];
            out += 2;
        }
    }
    buf.extend_from_slice(&tmp[..out]);
}

/// Lookup table mapping each byte 0x00..=0xFF to its 2-character lower-case
/// hex pair. Built at compile time so the per-byte hot path in
/// `write_bytea_into` is one `[u8; 2]` table read + one `extend_from_slice(2)`.
const HEX_PAIRS: [[u8; 2]; 256] = {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut table = [[0u8; 2]; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = [HEX[i >> 4], HEX[i & 0x0f]];
        i += 1;
    }
    table
};

/// Append the PG `\\x<hex>` bytea encoding of `data` directly into `buf`.
/// Skips the intermediate `String` allocation that the previous
/// `format_bytea` returned.
pub(crate) fn write_bytea_into(buf: &mut Vec<u8>, data: &[u8]) {
    buf.reserve(2 + data.len() * 2);
    buf.extend_from_slice(b"\\x");
    // Use the pre-built byte-pair table so each input byte produces a
    // single `extend_from_slice(2)` instead of two separate `push`
    // calls (each with its own bounds check). The reserve up front
    // means no Vec growth checks fire inside the loop.
    for &b in data {
        buf.extend_from_slice(&HEX_PAIRS[b as usize]);
    }
}

/// Format bytea as hex: `\x<hex>`. Thin wrapper around
/// [`write_bytea_into`] for callers that need an owned `String`.
fn format_bytea(data: &[u8]) -> String {
    let mut buf = Vec::with_capacity(2 + data.len() * 2);
    write_bytea_into(&mut buf, data);
    String::from_utf8(buf).unwrap_or_default()
}

/// Write a PostgreSQL-verbose interval rendering directly into `out`.
/// `out` accepts any `fmt::Write` target so callers on the wire encode
/// path can stream straight into a `Vec<u8>` adapter; the
/// String-returning [`format_interval`] is now a thin wrapper.
pub(crate) fn write_interval_into<W: std::fmt::Write>(
    out: &mut W,
    months: i32,
    days: i32,
    micros: i64,
) -> std::fmt::Result {
    let mut need_space = false;

    if months != 0 {
        let years = months / 12;
        let rem_months = months % 12;
        if years != 0 {
            write!(out, "{years} {}", if years == 1 { "year" } else { "years" })?;
            need_space = true;
        }
        if rem_months != 0 {
            if need_space {
                out.write_char(' ')?;
            }
            write!(
                out,
                "{rem_months} {}",
                if rem_months == 1 { "mon" } else { "mons" }
            )?;
            need_space = true;
        }
    }

    if days != 0 {
        if need_space {
            out.write_char(' ')?;
        }
        write!(out, "{days} {}", if days == 1 { "day" } else { "days" })?;
        need_space = true;
    }

    if micros != 0 || !need_space {
        if need_space {
            out.write_char(' ')?;
        }
        let has_negative_date_part = months < 0 || days < 0;
        let sign = if micros < 0 {
            "-"
        } else if need_space && has_negative_date_part {
            "+"
        } else {
            ""
        };
        let abs_micros = micros.unsigned_abs();
        let total_secs = abs_micros / 1_000_000;
        let frac_micros = abs_micros % 1_000_000;
        let hours = total_secs / 3600;
        let mins = (total_secs % 3600) / 60;
        let secs = total_secs % 60;

        if frac_micros == 0 {
            write!(out, "{sign}{hours:02}:{mins:02}:{secs:02}")?;
        } else {
            // Render the 6-digit fractional part into a stack buffer
            // and trim trailing zeros without ever allocating an
            // intermediate `String` (the previous code paid one
            // `format!("{frac_micros:06}")` + a copy through
            // `trim_end_matches`).
            let mut digits = [0u8; 6];
            let mut n = frac_micros as u32;
            for slot in digits.iter_mut().rev() {
                *slot = b'0' + (n % 10) as u8;
                n /= 10;
            }
            let mut end = digits.len();
            while end > 0 && digits[end - 1] == b'0' {
                end -= 1;
            }
            let trimmed = std::str::from_utf8(&digits[..end]).unwrap_or("");
            write!(out, "{sign}{hours:02}:{mins:02}:{secs:02}.{trimmed}")?;
        }
    }
    Ok(())
}

/// Format an interval in `PostgreSQL` verbose style. Thin wrapper over
/// [`write_interval_into`].
fn format_interval(months: i32, days: i32, micros: i64) -> String {
    let mut out = String::with_capacity(32);
    let _ = write_interval_into(&mut out, months, days, micros);
    out
}

/// Maximum recursion depth tolerated when serialising a `Value::Array`.
///
/// Mirrors the bind-side `MAX_BIND_ARRAY_DEPTH`. A deeper input would
/// almost certainly come from a programmatic builder or a corrupted
/// engine output rather than legitimate SQL, and recursing without a
/// cap aborts the connection task with a stack overflow.
const MAX_FORMAT_ARRAY_DEPTH: usize = 32;

/// Write a PostgreSQL-style array literal (`{1,2,3}`) directly into
/// `out`. `out` accepts any `fmt::Write` target so the wire encoder
/// can stream the rendering straight into a `Vec<u8>` adapter.
pub(crate) fn write_array_into<W: std::fmt::Write>(
    out: &mut W,
    elements: &[Value],
) -> std::fmt::Result {
    out.write_char('{')?;
    write_array_elements(out, elements, 0)?;
    out.write_char('}')?;
    Ok(())
}

/// Format an array as PostgreSQL-style array literal: `{1,2,3}`.
/// Thin wrapper over [`write_array_into`].
fn format_array(elements: &[Value]) -> String {
    let mut out = String::with_capacity(2 + elements.len() * 8);
    let _ = write_array_into(&mut out, elements);
    out
}

fn write_array_elements<W: std::fmt::Write>(
    out: &mut W,
    elements: &[Value],
    depth: usize,
) -> std::fmt::Result {
    if depth > MAX_FORMAT_ARRAY_DEPTH {
        return out.write_str("...");
    }
    for (i, elem) in elements.iter().enumerate() {
        if i > 0 {
            out.write_char(',')?;
        }
        match elem {
            Value::Null => out.write_str("NULL")?,
            Value::Text(t) => write_quoted_array_element(out, t)?,
            Value::Array(inner) => {
                out.write_char('{')?;
                write_array_elements(out, inner, depth + 1)?;
                out.write_char('}')?;
            }
            // Fast paths: scalar variants whose text rendering is
            // guaranteed not to contain whitespace, comma, brace, quote,
            // or backslash --- so `array_scalar_needs_quotes` would
            // always return false. Write the digits/letters straight
            // into `out` and skip the per-element `value_to_text`
            // String allocation that the fallback paid.
            Value::Int(v) => write!(out, "{v}")?,
            Value::BigInt(v) => write!(out, "{v}")?,
            Value::Boolean(v) => out.write_char(if *v { 't' } else { 'f' })?,
            Value::Real(v) => {
                if v.is_nan() {
                    out.write_str("NaN")?;
                } else if *v == f32::INFINITY {
                    out.write_str("Infinity")?;
                } else if *v == f32::NEG_INFINITY {
                    out.write_str("-Infinity")?;
                } else {
                    write!(out, "{v}")?;
                }
            }
            Value::Double(v) => {
                if v.is_nan() {
                    out.write_str("NaN")?;
                } else if *v == f64::INFINITY {
                    out.write_str("Infinity")?;
                } else if *v == f64::NEG_INFINITY {
                    out.write_str("-Infinity")?;
                } else {
                    write!(out, "{v}")?;
                }
            }
            Value::Date(_) => {
                if let Some(text) = value_to_text(elem) {
                    out.write_str(&text)?;
                }
            }
            other => {
                if let Some(text) = value_to_text(other) {
                    if array_scalar_needs_quotes(&text) {
                        write_quoted_array_element(out, &text)?;
                    } else {
                        out.write_str(&text)?;
                    }
                } else {
                    out.write_str("NULL")?;
                }
            }
        }
    }
    Ok(())
}

fn array_scalar_needs_quotes(text: &str) -> bool {
    if text.is_empty() || text.eq_ignore_ascii_case("NULL") {
        return true;
    }
    let bytes = text.as_bytes();
    // Fast ASCII path: scan raw bytes for the five PG array-mode trigger
    // characters or any ASCII whitespace byte. UTF-8 multi-byte sequences
    // start with a byte >= 0x80 and never collide with these triggers, so
    // when the string is pure ASCII (the dominant shape for array
    // elements) we avoid `chars()`'s decoded iteration plus
    // `char::is_whitespace`'s Unicode table lookup.
    if text.is_ascii() {
        return bytes.iter().any(|b| {
            matches!(
                *b,
                b',' | b'{' | b'}' | b'"' | b'\\' | b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C
            )
        });
    }
    // Non-ASCII: defer to the original char-by-char path so that locale-
    // sensitive Unicode whitespace (NBSP, etc.) is still detected as a
    // quoting trigger, matching the prior semantics exactly.
    text.chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, ',' | '{' | '}' | '"' | '\\'))
}

fn write_quoted_array_element<W: std::fmt::Write>(out: &mut W, text: &str) -> std::fmt::Result {
    out.write_char('"')?;
    // Bulk-copy chunks between escape triggers instead of dispatching
    // per `char`. Only `\\` and `"` need escaping in PG array text mode,
    // and they are both single-byte ASCII; for any prefix without
    // either byte we emit a single `write_str`. UTF-8 multi-byte
    // sequences cannot collide because their leading bytes are all
    // >= 0x80, so the byte indices `memchr` returns are always at
    // valid char boundaries.
    let bytes = text.as_bytes();
    let mut last = 0usize;
    for (idx, &b) in bytes.iter().enumerate() {
        if b == b'"' || b == b'\\' {
            if idx > last {
                out.write_str(&text[last..idx])?;
            }
            out.write_char('\\')?;
            // Safe: `b` is `"` or `\\`, both ASCII.
            out.write_char(b as char)?;
            last = idx + 1;
        }
    }
    if last < bytes.len() {
        out.write_str(&text[last..])?;
    }
    out.write_char('"')
}

/// Format a vector as PostgreSQL-style array literal: `[1.0, 2.0, 3.0]`.
fn format_vector(values: &[f32]) -> String {
    let mut s = String::with_capacity(2 + values.len() * 6);
    s.push('[');
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        if v.is_nan() {
            s.push_str("NaN");
        } else if *v == f32::INFINITY {
            s.push_str("Infinity");
        } else if *v == f32::NEG_INFINITY {
            s.push_str("-Infinity");
        } else {
            let _ = write!(s, "{v}");
        }
    }
    s.push(']');
    s
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{IntervalValue, NumericValue, VectorValue};
    use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

    #[test]
    fn null_returns_none() {
        assert_eq!(value_to_text(&Value::Null), None);
    }

    #[test]
    fn int_positive() {
        assert_eq!(value_to_text(&Value::Int(42)), Some("42".to_string()));
    }

    #[test]
    fn int_negative() {
        assert_eq!(value_to_text(&Value::Int(-1)), Some("-1".to_string()));
    }

    #[test]
    fn int_zero() {
        assert_eq!(value_to_text(&Value::Int(0)), Some("0".to_string()));
    }

    #[test]
    fn bigint_large() {
        assert_eq!(
            value_to_text(&Value::BigInt(9_999_999_999)),
            Some("9999999999".to_string())
        );
    }

    #[test]
    fn real_normal() {
        let result = value_to_text(&Value::Real(3.14));
        assert!(result.is_some());
        let s = result.unwrap();
        assert!(s.starts_with("3.14"));
    }

    #[test]
    fn real_nan() {
        assert_eq!(
            value_to_text(&Value::Real(f32::NAN)),
            Some("NaN".to_string())
        );
    }

    #[test]
    fn real_infinity() {
        assert_eq!(
            value_to_text(&Value::Real(f32::INFINITY)),
            Some("Infinity".to_string())
        );
    }

    #[test]
    fn real_neg_infinity() {
        assert_eq!(
            value_to_text(&Value::Real(f32::NEG_INFINITY)),
            Some("-Infinity".to_string())
        );
    }

    #[test]
    fn double_nan() {
        assert_eq!(
            value_to_text(&Value::Double(f64::NAN)),
            Some("NaN".to_string())
        );
    }

    #[test]
    fn boolean_true() {
        assert_eq!(value_to_text(&Value::Boolean(true)), Some("t".to_string()));
    }

    #[test]
    fn boolean_false() {
        assert_eq!(value_to_text(&Value::Boolean(false)), Some("f".to_string()));
    }

    #[test]
    fn text_value() {
        assert_eq!(
            value_to_text(&Value::Text("hello".to_string())),
            Some("hello".to_string())
        );
    }

    #[test]
    fn text_empty() {
        assert_eq!(
            value_to_text(&Value::Text(String::new())),
            Some(String::new())
        );
    }

    #[test]
    fn blob_empty() {
        assert_eq!(value_to_text(&Value::Blob(vec![])), Some("\\x".to_string()));
    }

    #[test]
    fn blob_hex() {
        assert_eq!(
            value_to_text(&Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF])),
            Some("\\xdeadbeef".to_string())
        );
    }

    #[test]
    fn numeric_integer() {
        let v = Value::Numeric(NumericValue::new(42, 0));
        assert_eq!(value_to_text(&v), Some("42".to_string()));
    }

    #[test]
    fn numeric_with_scale() {
        let v = Value::Numeric(NumericValue::new(12345, 2));
        assert_eq!(value_to_text(&v), Some("123.45".to_string()));
    }

    #[test]
    fn numeric_leading_zeros() {
        let v = Value::Numeric(NumericValue::new(5, 3));
        assert_eq!(value_to_text(&v), Some("0.005".to_string()));
    }

    #[test]
    fn numeric_negative() {
        let v = Value::Numeric(NumericValue::new(-12345, 2));
        assert_eq!(value_to_text(&v), Some("-123.45".to_string()));
    }

    #[test]
    fn numeric_zero() {
        let v = Value::Numeric(NumericValue::new(0, 0));
        assert_eq!(value_to_text(&v), Some("0".to_string()));
    }

    #[test]
    fn numeric_special_values_use_pg_text_labels() {
        assert_eq!(
            value_to_text(&Value::Numeric(NumericValue::NAN)),
            Some("NaN".to_string())
        );
        assert_eq!(
            value_to_text(&Value::Numeric(NumericValue::INFINITY)),
            Some("Infinity".to_string())
        );
        assert_eq!(
            value_to_text(&Value::Numeric(NumericValue::NEG_INFINITY)),
            Some("-Infinity".to_string())
        );
    }

    #[test]
    fn timestamp_no_fractional() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::March, 15).unwrap(),
            Time::from_hms(10, 30, 45).unwrap(),
        );
        assert_eq!(
            value_to_text(&Value::Timestamp(dt)),
            Some("2024-03-15 10:30:45".to_string())
        );
    }

    #[test]
    fn timestamp_with_microseconds() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::January, 1).unwrap(),
            Time::from_hms_micro(0, 0, 0, 123_456).unwrap(),
        );
        assert_eq!(
            value_to_text(&Value::Timestamp(dt)),
            Some("2024-01-01 00:00:00.123456".to_string())
        );
    }

    #[test]
    fn date_basic() {
        let d = Date::from_calendar_date(2024, Month::June, 15).unwrap();
        assert_eq!(
            value_to_text(&Value::Date(d)),
            Some("2024-06-15".to_string())
        );
    }

    #[test]
    fn interval_zero() {
        let v = Value::Interval(IntervalValue::new(0, 0, 0));
        assert_eq!(value_to_text(&v), Some("00:00:00".to_string()));
    }

    #[test]
    fn interval_with_months_and_days() {
        let v = Value::Interval(IntervalValue::new(14, 3, 0));
        let s = value_to_text(&v).unwrap();
        assert!(s.contains("1 year"));
        assert!(s.contains("2 mons"));
        assert!(s.contains("3 days"));
    }

    #[test]
    fn interval_with_time() {
        let v = Value::Interval(IntervalValue::new(0, 0, 3_661_000_000));
        let s = value_to_text(&v).unwrap();
        assert!(s.contains("01:01:01"));
    }

    #[test]
    fn vector_basic() {
        let v = Value::Vector(VectorValue::new(3, vec![1.0, 2.0, 3.0]));
        assert_eq!(value_to_text(&v), Some("[1,2,3]".to_string()));
    }

    #[test]
    fn vector_empty() {
        let v = Value::Vector(VectorValue::new(0, vec![]));
        assert_eq!(value_to_text(&v), Some("[]".to_string()));
    }

    #[test]
    fn format_bytea_all_zeros() {
        assert_eq!(format_bytea(&[0, 0, 0]), "\\x000000");
    }

    #[test]
    fn format_bytea_all_ff() {
        assert_eq!(format_bytea(&[0xFF, 0xFF]), "\\xffff");
    }

    // -----------------------------------------------------------------------
    // Additional value_to_text coverage
    // -----------------------------------------------------------------------

    #[test]
    fn double_normal() {
        let result = value_to_text(&Value::Double(2.718281828));
        assert!(result.is_some());
        assert!(result.unwrap().starts_with("2.71828"));
    }

    #[test]
    fn double_infinity() {
        assert_eq!(
            value_to_text(&Value::Double(f64::INFINITY)),
            Some("Infinity".to_string())
        );
    }

    #[test]
    fn double_neg_infinity() {
        assert_eq!(
            value_to_text(&Value::Double(f64::NEG_INFINITY)),
            Some("-Infinity".to_string())
        );
    }

    #[test]
    fn real_zero() {
        assert_eq!(value_to_text(&Value::Real(0.0)), Some("0".to_string()));
    }

    #[test]
    fn double_zero() {
        assert_eq!(value_to_text(&Value::Double(0.0)), Some("0".to_string()));
    }

    #[test]
    fn bigint_negative() {
        assert_eq!(
            value_to_text(&Value::BigInt(-9_999_999_999)),
            Some("-9999999999".to_string())
        );
    }

    #[test]
    fn bigint_zero() {
        assert_eq!(value_to_text(&Value::BigInt(0)), Some("0".to_string()));
    }

    #[test]
    fn text_with_unicode() {
        assert_eq!(
            value_to_text(&Value::Text("cafe\u{0301}".to_string())),
            Some("cafe\u{0301}".to_string())
        );
    }

    #[test]
    fn blob_single_byte() {
        assert_eq!(
            value_to_text(&Value::Blob(vec![0x42])),
            Some("\\x42".to_string())
        );
    }

    #[test]
    fn numeric_large_scale() {
        // coefficient=1, scale=10 -> "0.0000000001"
        let v = Value::Numeric(NumericValue::new(1, 10));
        let s = value_to_text(&v).unwrap();
        assert_eq!(s, "0.0000000001");
    }

    #[test]
    fn numeric_negative_zero_scale() {
        let v = Value::Numeric(NumericValue::new(-7, 0));
        assert_eq!(value_to_text(&v), Some("-7".to_string()));
    }

    #[test]
    fn timestamp_midnight() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2000, Month::January, 1).unwrap(),
            Time::from_hms(0, 0, 0).unwrap(),
        );
        assert_eq!(
            value_to_text(&Value::Timestamp(dt)),
            Some("2000-01-01 00:00:00".to_string())
        );
    }

    #[test]
    fn date_leap_year() {
        let d = Date::from_calendar_date(2000, Month::February, 29).unwrap();
        assert_eq!(
            value_to_text(&Value::Date(d)),
            Some("2000-02-29".to_string())
        );
    }

    #[test]
    fn interval_negative_time() {
        let v = Value::Interval(IntervalValue::new(0, 0, -3_600_000_000));
        let s = value_to_text(&v).unwrap();
        assert!(s.contains("-01:00:00"));
    }

    #[test]
    fn interval_days_only() {
        let v = Value::Interval(IntervalValue::new(0, 5, 0));
        let s = value_to_text(&v).unwrap();
        assert!(s.contains("5 days"));
    }

    #[test]
    fn interval_one_month() {
        let v = Value::Interval(IntervalValue::new(1, 0, 0));
        let s = value_to_text(&v).unwrap();
        assert!(s.contains("1 mon"));
    }

    #[test]
    fn interval_one_year() {
        let v = Value::Interval(IntervalValue::new(12, 0, 0));
        let s = value_to_text(&v).unwrap();
        assert!(s.contains("1 year"));
    }

    #[test]
    fn interval_one_day() {
        let v = Value::Interval(IntervalValue::new(0, 1, 0));
        let s = value_to_text(&v).unwrap();
        assert!(s.contains("1 day"));
    }

    #[test]
    fn vector_single_element() {
        let v = Value::Vector(VectorValue::new(1, vec![42.0]));
        assert_eq!(value_to_text(&v), Some("[42]".to_string()));
    }

    #[test]
    fn vector_with_nan() {
        let v = Value::Vector(VectorValue::new(2, vec![1.0, f32::NAN]));
        assert_eq!(value_to_text(&v), Some("[1,NaN]".to_string()));
    }

    #[test]
    fn format_float_f32_negative_zero() {
        // -0.0 should produce "-0" or "0"; either is acceptable.
        let s = format_float_f32(-0.0_f32);
        assert!(s == "-0" || s == "0");
    }

    #[test]
    fn format_float_f64_negative_zero() {
        let s = format_float_f64(-0.0_f64);
        assert!(s == "-0" || s == "0");
    }

    #[test]
    fn interval_with_fractional_seconds() {
        // 1.5 seconds = 1_500_000 micros
        let v = Value::Interval(IntervalValue::new(0, 0, 1_500_000));
        let s = value_to_text(&v).unwrap();
        assert!(s.contains("00:00:01.5"));
    }

    // -----------------------------------------------------------------------
    // UUID formatting
    // -----------------------------------------------------------------------

    #[test]
    fn uuid_standard_format() {
        let bytes = [
            0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44,
            0x00, 0x00,
        ];
        assert_eq!(
            value_to_text(&Value::Uuid(bytes)),
            Some("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
    }

    #[test]
    fn uuid_all_zeros() {
        assert_eq!(
            value_to_text(&Value::Uuid([0u8; 16])),
            Some("00000000-0000-0000-0000-000000000000".to_string())
        );
    }

    #[test]
    fn uuid_all_ff() {
        assert_eq!(
            value_to_text(&Value::Uuid([0xFF; 16])),
            Some("ffffffff-ffff-ffff-ffff-ffffffffffff".to_string())
        );
    }

    // -----------------------------------------------------------------------
    // TimestampTz formatting
    // -----------------------------------------------------------------------

    #[test]
    fn timestamptz_utc() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::March, 15).unwrap(),
            Time::from_hms(10, 30, 45).unwrap(),
        );
        let odt = dt.assume_utc();
        let s = value_to_text(&Value::TimestampTz(odt)).unwrap();
        assert_eq!(s, "2024-03-15 10:30:45+00");
    }

    #[test]
    fn timestamptz_positive_offset() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::June, 1).unwrap(),
            Time::from_hms(15, 0, 0).unwrap(),
        );
        let odt = dt.assume_offset(time::UtcOffset::from_hms(5, 0, 0).unwrap());
        let s = value_to_text(&Value::TimestampTz(odt)).unwrap();
        assert_eq!(s, "2024-06-01 15:00:00+05");
    }

    #[test]
    fn timestamptz_negative_offset() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::January, 1).unwrap(),
            Time::from_hms(0, 0, 0).unwrap(),
        );
        let odt = dt.assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap());
        let s = value_to_text(&Value::TimestampTz(odt)).unwrap();
        assert_eq!(s, "2024-01-01 00:00:00-08");
    }

    #[test]
    fn timestamptz_with_microseconds() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::January, 1).unwrap(),
            Time::from_hms_micro(0, 0, 0, 123456).unwrap(),
        );
        let odt = dt.assume_utc();
        let s = value_to_text(&Value::TimestampTz(odt)).unwrap();
        assert_eq!(s, "2024-01-01 00:00:00.123456+00");
    }

    #[test]
    fn timestamptz_with_half_hour_offset() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::March, 15).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        );
        let odt = dt.assume_offset(time::UtcOffset::from_hms(5, 30, 0).unwrap());
        let s = value_to_text(&Value::TimestampTz(odt)).unwrap();
        assert_eq!(s, "2024-03-15 12:00:00+05:30");
    }

    #[test]
    fn timetz_positive_offset() {
        let s = value_to_text(&Value::TimeTz(
            Time::from_hms_micro(12, 34, 56, 789_000).unwrap(),
            UtcOffset::from_hms(5, 30, 0).unwrap(),
        ))
        .unwrap();
        assert_eq!(s, "12:34:56.789+05:30");
    }

    #[test]
    fn timetz_negative_offset() {
        let s = value_to_text(&Value::TimeTz(
            Time::from_hms(1, 2, 3).unwrap(),
            UtcOffset::from_hms(-8, 0, 0).unwrap(),
        ))
        .unwrap();
        assert_eq!(s, "01:02:03-08");
    }

    #[test]
    fn timestamp_trailing_zero_trimmed() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::June, 1).unwrap(),
            Time::from_hms_micro(12, 0, 0, 100_000).unwrap(),
        );
        let s = value_to_text(&Value::Timestamp(dt)).unwrap();
        // 100_000 micros = ".1" after trimming trailing zeros
        assert_eq!(s, "2024-06-01 12:00:00.1");
    }

    #[test]
    fn array_text_quotes_timestamp_elements() {
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::March, 15).unwrap(),
            Time::from_hms(10, 30, 45).unwrap(),
        );
        let s = value_to_text(&Value::Array(vec![Value::Timestamp(dt)])).unwrap();
        assert_eq!(s, "{\"2024-03-15 10:30:45\"}");
    }

    #[test]
    fn array_text_quotes_interval_elements() {
        let interval = IntervalValue::new(0, 1, 3_600_000_000);
        let s = value_to_text(&Value::Array(vec![Value::Interval(interval)])).unwrap();
        assert_eq!(s, "{\"1 day 01:00:00\"}");
    }

    #[test]
    fn array_text_quotes_jsonb_elements_with_comma() {
        let s = value_to_text(&Value::Array(vec![Value::Jsonb(serde_json::json!({
            "a": 1,
            "b": 2
        }))]))
        .unwrap();
        assert_eq!(s, "{\"{\\\"a\\\": 1, \\\"b\\\": 2}\"}");
    }

    #[test]
    fn array_text_preserves_literal_null_string() {
        let s = value_to_text(&Value::Array(vec![Value::Text("NULL".to_owned())])).unwrap();
        assert_eq!(s, "{\"NULL\"}");
    }
}
