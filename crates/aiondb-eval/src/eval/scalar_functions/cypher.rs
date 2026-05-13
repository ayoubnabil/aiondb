//! Cypher type conversion functions: toBoolean, toInteger, toFloat, toString,
//! and their OrNull variants.

use aiondb_core::{DbError, DbResult, Value};

use super::expect_args;
use super::value_convert::{f64_to_i64, i64_to_f64};

// ── Conversion target enum ───────────────────────────────────────────

/// The target type for a Cypher type-conversion function.
enum ConvertTarget {
    Boolean,
    Integer,
    Float,
    String,
}

impl ConvertTarget {
    /// The base function name (without "OrNull" suffix).
    fn func_name(&self) -> &'static str {
        match self {
            ConvertTarget::Boolean => "toBoolean",
            ConvertTarget::Integer => "toInteger",
            ConvertTarget::Float => "toFloat",
            ConvertTarget::String => "toString",
        }
    }
}

// ── Cypher-specific temporal formatters ──────────────────────────────
// Cypher renders temporals with `T` separator, full nanosecond precision,
// and `+HH:MM` offsets (never the bare `+HH` shorthand).

use std::fmt::Write as _;

fn write_fractional_seconds(out: &mut String, nano: u32) {
    if nano == 0 {
        return;
    }
    let mut digits = [0u8; 9];
    let mut n = nano;
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

fn write_offset_hhmm(out: &mut String, off: &time::UtcOffset) {
    if off.whole_seconds() == 0 {
        out.push('Z');
        return;
    }
    let (oh, om, _) = off.as_hms();
    let abs_om = om.unsigned_abs();
    let _ = write!(out, "{oh:+03}:{abs_om:02}");
}

fn write_cypher_time(out: &mut String, t: &time::Time) {
    let _ = write!(out, "{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second());
    write_fractional_seconds(out, t.nanosecond());
}

fn format_cypher_time(t: &time::Time) -> String {
    let mut out = String::with_capacity(18);
    write_cypher_time(&mut out, t);
    out
}

fn format_cypher_timetz(t: &time::Time, off: &time::UtcOffset) -> String {
    let mut out = String::with_capacity(24);
    write_cypher_time(&mut out, t);
    write_offset_hhmm(&mut out, off);
    out
}

fn format_cypher_timestamp(ts: &time::PrimitiveDateTime) -> String {
    // `format_date` already returns an owned String; reuse it as the
    // accumulator so we save one allocation versus the previous
    // `format!("{date}T{time}")` final concatenation.
    let mut out = aiondb_core::temporal::format::format_date(ts.date());
    out.reserve(20);
    out.push('T');
    write_cypher_time(&mut out, &ts.time());
    out
}

fn format_cypher_timestamptz(ts: &time::OffsetDateTime) -> String {
    let mut out = aiondb_core::temporal::format::format_date(ts.date());
    out.reserve(28);
    out.push('T');
    write_cypher_time(&mut out, &ts.time());
    write_offset_hhmm(&mut out, &ts.offset());
    out
}

// ── Shared conversion logic ──────────────────────────────────────────

/// Core conversion: converts `value` to the requested `target` type.
///
/// Returns `Ok(Value::Null)` for null input.
/// Returns `Err` for unsupported source types (the "strict" variant).
fn convert_value(value: &Value, target: &ConvertTarget) -> DbResult<Value> {
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    match target {
        ConvertTarget::Boolean => match value {
            Value::Boolean(b) => Ok(Value::Boolean(*b)),
            Value::Int(n) => Ok(Value::Boolean(*n != 0)),
            Value::BigInt(n) => Ok(Value::Boolean(*n != 0)),
            Value::Text(s) => Ok(string_to_boolean(s)),
            Value::Jsonb(j) => match j {
                serde_json::Value::Null => Ok(Value::Null),
                serde_json::Value::Bool(b) => Ok(Value::Boolean(*b)),
                serde_json::Value::Number(n) => Ok(n
                    .as_i64()
                    .map(|i| Value::Boolean(i != 0))
                    .unwrap_or(Value::Null)),
                serde_json::Value::String(s) => Ok(string_to_boolean(s)),
                _ => Err(type_error(target, value)),
            },
            _ => Err(type_error(target, value)),
        },
        ConvertTarget::Integer => match value {
            Value::Int(n) => Ok(Value::BigInt(i64::from(*n))),
            Value::BigInt(n) => Ok(Value::BigInt(*n)),
            Value::Real(f) => Ok(Value::BigInt(super::value_convert::f64_to_i64(f64::from(
                *f,
            ))?)),
            Value::Double(f) => Ok(Value::BigInt(super::value_convert::f64_to_i64(*f)?)),
            Value::Numeric(n) => Ok(super::value_convert::f64_to_i64(n.to_f64())
                .map(Value::BigInt)
                .unwrap_or(Value::Null)),
            Value::Text(s) => Ok(string_to_integer(s)),
            Value::Boolean(b) => Ok(Value::BigInt(i64::from(*b))),
            Value::Jsonb(j) => match j {
                serde_json::Value::Null => Ok(Value::Null),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        Ok(Value::BigInt(i))
                    } else if let Some(f) = n.as_f64() {
                        Ok(super::value_convert::f64_to_i64(f)
                            .map(Value::BigInt)
                            .unwrap_or(Value::Null))
                    } else {
                        Ok(Value::Null)
                    }
                }
                serde_json::Value::Bool(b) => Ok(Value::BigInt(i64::from(*b))),
                serde_json::Value::String(s) => Ok(string_to_integer(s)),
                _ => Err(type_error(target, value)),
            },
            _ => Err(type_error(target, value)),
        },
        ConvertTarget::Float => match value {
            Value::Int(n) => Ok(Value::Double(f64::from(*n))),
            Value::BigInt(n) => Ok(Value::Double(i64_to_f64(*n))),
            Value::Real(f) => Ok(Value::Double(f64::from(*f))),
            Value::Double(f) => Ok(Value::Double(*f)),
            Value::Numeric(n) => Ok(Value::Double(n.to_f64())),
            Value::Boolean(b) => Ok(Value::Double(if *b { 1.0 } else { 0.0 })),
            Value::Text(s) => Ok(string_to_float(s)),
            Value::Jsonb(j) => match j {
                serde_json::Value::Null => Ok(Value::Null),
                serde_json::Value::Number(n) => {
                    Ok(n.as_f64().map(Value::Double).unwrap_or(Value::Null))
                }
                serde_json::Value::Bool(b) => Ok(Value::Double(if *b { 1.0 } else { 0.0 })),
                serde_json::Value::String(s) => Ok(string_to_float(s)),
                _ => Err(type_error(target, value)),
            },
            _ => Err(type_error(target, value)),
        },
        ConvertTarget::String => match value {
            Value::Boolean(b) => Ok(Value::Text(b.to_string())),
            Value::Int(n) => Ok(Value::Text(n.to_string())),
            Value::BigInt(n) => Ok(Value::Text(n.to_string())),
            Value::Real(f) => Ok(Value::Text(format_float(f64::from(*f)))),
            Value::Double(f) => Ok(Value::Text(format_float(*f))),
            Value::Numeric(n) => Ok(Value::Text(n.to_string())),
            Value::Text(s) => Ok(Value::Text(s.clone())),
            Value::Date(d) => Ok(Value::Text(aiondb_core::temporal::format::format_date(*d))),
            Value::Time(t) => Ok(Value::Text(format_cypher_time(t))),
            Value::TimeTz(t, off) => Ok(Value::Text(format_cypher_timetz(t, off))),
            Value::Timestamp(ts) => Ok(Value::Text(format_cypher_timestamp(ts))),
            Value::TimestampTz(ts) => Ok(Value::Text(format_cypher_timestamptz(ts))),
            Value::Interval(iv) => Ok(Value::Text(iv.to_string())),
            Value::Jsonb(j) => match j {
                serde_json::Value::Null => Ok(Value::Null),
                serde_json::Value::Bool(b) => Ok(Value::Text(b.to_string())),
                serde_json::Value::Number(n) => Ok(Value::Text(n.to_string())),
                serde_json::Value::String(s) => Ok(Value::Text(s.clone())),
                _ => Ok(Value::Text(j.to_string())),
            },
            _ => Err(type_error(target, value)),
        },
    }
}

/// Build the type-error for strict conversion functions.
fn type_error(target: &ConvertTarget, value: &Value) -> DbError {
    DbError::internal(format!(
        "{}() does not accept {} values",
        target.func_name(),
        value_type_name(value)
    ))
}

// ── Public API (strict variants) ─────────────────────────────────────

/// `toBoolean(expr)` - convert booleans or strings to boolean.
pub(crate) fn eval_to_boolean(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "toBoolean")?;
    convert_value(&args[0], &ConvertTarget::Boolean)
}

/// `toInteger(expr)` - convert integers, floats, strings, or booleans to integer.
pub(crate) fn eval_to_integer(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "toInteger")?;
    convert_value(&args[0], &ConvertTarget::Integer)
}

/// `toFloat(expr)` - convert numbers or strings to float.
pub(crate) fn eval_to_float(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "toFloat")?;
    convert_value(&args[0], &ConvertTarget::Float)
}

/// `toString(expr)` - convert any supported value to string.
pub(crate) fn eval_to_string(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "toString")?;
    convert_value(&args[0], &ConvertTarget::String)
}

// ── Public API (OrNull variants) ─────────────────────────────────────

/// `toBooleanOrNull(expr)` - like toBoolean but returns null instead of error.
pub(crate) fn eval_to_boolean_or_null(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "toBooleanOrNull")?;
    Ok(convert_value(&args[0], &ConvertTarget::Boolean).unwrap_or(Value::Null))
}

/// `toIntegerOrNull(expr)` - like toInteger but returns null instead of error.
pub(crate) fn eval_to_integer_or_null(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "toIntegerOrNull")?;
    Ok(convert_value(&args[0], &ConvertTarget::Integer).unwrap_or(Value::Null))
}

/// `toFloatOrNull(expr)` - like toFloat but returns null instead of error.
pub(crate) fn eval_to_float_or_null(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "toFloatOrNull")?;
    Ok(convert_value(&args[0], &ConvertTarget::Float).unwrap_or(Value::Null))
}

/// `toStringOrNull(expr)` - like toString but returns null instead of error.
pub(crate) fn eval_to_string_or_null(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "toStringOrNull")?;
    Ok(convert_value(&args[0], &ConvertTarget::String).unwrap_or(Value::Null))
}

// ── Helpers ──────────────────────────────────────────────────────────

fn string_to_boolean(s: &str) -> Value {
    // Avoid the `to_ascii_lowercase()` String alloc - Cypher's
    // `toBoolean()` accepts only the two literals "true"/"false"
    // (case-insensitive); compare via `eq_ignore_ascii_case`.
    let trimmed = s.trim();
    if trimmed.eq_ignore_ascii_case("true") {
        Value::Boolean(true)
    } else if trimmed.eq_ignore_ascii_case("false") {
        Value::Boolean(false)
    } else {
        Value::Null
    }
}

fn string_to_integer(s: &str) -> Value {
    let trimmed = s.trim();
    // Try parsing as integer first
    if let Ok(n) = trimmed.parse::<i64>() {
        return Value::BigInt(n);
    }
    // Try parsing as float and truncate
    if let Ok(f) = trimmed.parse::<f64>() {
        if f.is_finite() && f >= i64_to_f64(i64::MIN) && f < i64_to_f64(i64::MAX) {
            return f64_to_i64(f).map(Value::BigInt).unwrap_or(Value::Null);
        }
    }
    Value::Null
}

fn string_to_float(s: &str) -> Value {
    let trimmed = s.trim();
    if let Ok(f) = trimmed.parse::<f64>() {
        if f.is_finite() {
            return Value::Double(f);
        }
    }
    Value::Null
}

fn format_float(f: f64) -> String {
    if f == f.trunc() && f.abs() < 1e15 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NULL",
        Value::Boolean(_) => "Boolean",
        Value::Int(_) | Value::BigInt(_) => "Integer",
        Value::Real(_) | Value::Double(_) => "Float",
        Value::Text(_) => "String",
        Value::Array(_) => "List",
        _ => "unsupported",
    }
}
