//! Bind-parameter coercion: convert raw wire bytes to typed `Value`s.

use std::str;

use aiondb_core::{
    DataType, DbError, ErrorReport, IntervalValue, MacAddr, MacAddr8, NumericValue, PgLsnValue,
    SqlState, TidValue, Value, VectorValue,
};
use bytes::Bytes;
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

use crate::binary_format::decode_binary_param;

fn invalid_bind_parameter_count(expected: usize, actual: usize) -> DbError {
    DbError::Bind(Box::new(ErrorReport::new(
        SqlState::InvalidParameterValue,
        format!("expected {expected} bound parameter(s), received {actual}"),
    )))
}

pub(crate) fn coerce_bind_params(
    param_types: &[DataType],
    param_values: &[Option<Bytes>],
) -> Result<Vec<Value>, DbError> {
    if param_types.len() != param_values.len() {
        return Err(invalid_bind_parameter_count(
            param_types.len(),
            param_values.len(),
        ));
    }

    param_types
        .iter()
        .zip(param_values.iter())
        .enumerate()
        .map(|(index, (data_type, value))| {
            coerce_bind_value(index + 1, data_type, value.as_ref().map(Bytes::as_ref))
        })
        .collect()
}

/// Dispatch bind parameter coercion based on per-parameter format codes.
///
/// Text-format parameters go through the existing `coerce_bind_params` path.
/// Binary-format parameters are decoded via `coerce_binary_bind_params`.
pub(crate) fn coerce_bind_params_dispatched(
    param_types: &[DataType],
    param_formats: &[i16],
    param_values: &[Option<Bytes>],
) -> Result<Vec<Value>, DbError> {
    let any_binary = match param_formats.len() {
        0 => false,
        1 => param_formats[0] == 1,
        _ => param_formats.contains(&1),
    };
    if !any_binary {
        return coerce_bind_params(param_types, param_values);
    }
    coerce_binary_bind_params(param_types, param_formats, param_values)
}

/// Coerce bind parameters with mixed text/binary format codes.
///
/// `param_formats` follows `PostgreSQL` conventions:
/// - Empty or `[0]`: all text.
/// - `[1]`: all binary.
/// - N elements: per-parameter format code.
fn coerce_binary_bind_params(
    param_types: &[DataType],
    param_formats: &[i16],
    param_values: &[Option<Bytes>],
) -> Result<Vec<Value>, DbError> {
    if param_types.len() != param_values.len() {
        return Err(invalid_bind_parameter_count(
            param_types.len(),
            param_values.len(),
        ));
    }

    param_types
        .iter()
        .zip(param_values.iter())
        .enumerate()
        .map(|(index, (data_type, value))| {
            let format_code = match param_formats.len() {
                0 => 0,
                1 => param_formats[0],
                _ => param_formats.get(index).copied().unwrap_or(0),
            };
            let human_index = index + 1;
            match value {
                None => Ok(Value::Null),
                Some(data) if format_code == 1 => {
                    decode_binary_param(human_index, data.as_ref(), data_type)
                }
                Some(data) => coerce_bind_value(human_index, data_type, Some(data.as_ref())),
            }
        })
        .collect()
}

pub(crate) fn coerce_bind_value(
    index: usize,
    data_type: &DataType,
    raw_value: Option<&[u8]>,
) -> Result<Value, DbError> {
    let Some(raw_value) = raw_value else {
        return Ok(Value::Null);
    };
    match data_type {
        DataType::Int => {
            let parsed = parse_ascii_signed_i64(raw_value).and_then(|v| i32::try_from(v).ok());
            return parsed.map(Value::Int).ok_or_else(|| {
                DbError::protocol(format!(
                    "invalid INT value for bind parameter ${index}: {}",
                    String::from_utf8_lossy(raw_value)
                ))
            });
        }
        DataType::BigInt => {
            return parse_ascii_signed_i64(raw_value)
                .map(Value::BigInt)
                .ok_or_else(|| {
                    DbError::protocol(format!(
                        "invalid BIGINT value for bind parameter ${index}: {}",
                        String::from_utf8_lossy(raw_value)
                    ))
                });
        }
        _ => {}
    }
    let text = str::from_utf8(raw_value)
        .map_err(|_| DbError::protocol(format!("bind parameter ${index} is not valid UTF-8")))?;

    match data_type {
        DataType::Real => text.parse::<f32>().map(Value::Real).map_err(|_| {
            DbError::protocol(format!(
                "invalid REAL value for bind parameter ${index}: {text}"
            ))
        }),
        DataType::Double => text.parse::<f64>().map(Value::Double).map_err(|_| {
            DbError::protocol(format!(
                "invalid DOUBLE value for bind parameter ${index}: {text}"
            ))
        }),
        DataType::Numeric => text
            .parse::<NumericValue>()
            .map(Value::Numeric)
            .map_err(|error| {
                DbError::protocol(format!(
                    "invalid NUMERIC value for bind parameter ${index}: {error}"
                ))
            }),
        DataType::Money => {
            aiondb_eval::coercions::coerce_value(Value::Text(text.to_owned()), data_type).map_err(
                |error| {
                    DbError::protocol(format!(
                        "invalid MONEY value for bind parameter ${index}: {error}"
                    ))
                },
            )
        }
        DataType::Text => Ok(Value::Text(text.to_owned())),
        DataType::Boolean => parse_bool_text(text).map(Value::Boolean).ok_or_else(|| {
            DbError::protocol(format!(
                "invalid BOOLEAN value for bind parameter ${index}: {text}"
            ))
        }),
        DataType::Timestamp => parse_timestamp_text(text, index),
        DataType::Date => parse_date_text(text, index),
        DataType::Time => parse_time_text(text, index),
        DataType::TimeTz => parse_timetz_text(text, index),
        DataType::Interval => parse_interval_text(text, index),
        DataType::Tid => TidValue::parse(text).map(Value::Tid).ok_or_else(|| {
            DbError::protocol(format!(
                "invalid TID value for bind parameter ${index}: {text}"
            ))
        }),
        DataType::Uuid => Value::uuid_from_str(text).ok_or_else(|| {
            DbError::protocol(format!(
                "invalid UUID value for bind parameter ${index}: {text}"
            ))
        }),
        DataType::PgLsn => PgLsnValue::parse(text).map(Value::PgLsn).ok_or_else(|| {
            DbError::protocol(format!(
                "invalid PG_LSN value for bind parameter ${index}: {text}"
            ))
        }),
        DataType::TimestampTz => parse_timestamptz_text(text, index),
        DataType::Jsonb => {
            let v: serde_json::Value = serde_json::from_str(text).map_err(|e| {
                DbError::protocol(format!(
                    "invalid JSONB value for bind parameter ${index}: {e}"
                ))
            })?;
            Ok(Value::Jsonb(v))
        }
        DataType::Blob => {
            // PostgreSQL sends bytea as \x-prefixed hex in text mode.
            if let Some(hex) = text.strip_prefix("\\x") {
                let bytes = decode_hex_bytea(index, hex)?;
                Ok(Value::Blob(bytes))
            } else {
                // No \x prefix - treat as raw bytes.
                Ok(Value::Blob(text.as_bytes().to_vec()))
            }
        }
        DataType::MacAddr => MacAddr::parse(text).map(Value::MacAddr).ok_or_else(|| {
            DbError::protocol(format!(
                "invalid MACADDR value for bind parameter ${index}: {text}"
            ))
        }),
        DataType::MacAddr8 => MacAddr8::parse(text).map(Value::MacAddr8).ok_or_else(|| {
            DbError::protocol(format!(
                "invalid MACADDR8 value for bind parameter ${index}: {text}"
            ))
        }),
        DataType::Vector { dims, .. } => parse_vector_text(text, index, *dims),
        DataType::Array(ref inner) => parse_text_array(text, index, inner),
        // Safety net for any future DataType variants.
        #[allow(unreachable_patterns)]
        unsupported => Err(DbError::protocol(format!(
            "bind parameter ${index} type {unsupported} is not supported"
        ))),
    }
}

fn parse_ascii_signed_i64(raw: &[u8]) -> Option<i64> {
    if raw.is_empty() {
        return None;
    }
    let mut index = 0usize;
    let mut negative = false;
    match raw[0] {
        b'+' => index = 1,
        b'-' => {
            negative = true;
            index = 1;
        }
        _ => {}
    }
    if index >= raw.len() {
        return None;
    }
    let mut acc: i64 = 0;
    for &byte in &raw[index..] {
        if !byte.is_ascii_digit() {
            return None;
        }
        acc = acc.checked_mul(10)?;
        acc = acc.checked_add(i64::from(byte - b'0'))?;
    }
    if negative {
        acc.checked_neg()
    } else {
        Some(acc)
    }
}

fn parse_vector_text(text: &str, index: usize, expected_dims: u32) -> Result<Value, DbError> {
    let vector = VectorValue::parse(text).ok_or_else(|| {
        DbError::protocol(format!(
            "invalid VECTOR value for bind parameter ${index}: {text}"
        ))
    })?;
    if expected_dims != 0 && vector.dims != expected_dims {
        return Err(DbError::protocol(format!(
            "invalid VECTOR value for bind parameter ${index}: expected VECTOR({expected_dims}), got VECTOR({})",
            vector.dims
        )));
    }
    Ok(Value::Vector(vector))
}

fn parse_bool_text(text: &str) -> Option<bool> {
    if text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("t") || text == "1" {
        Some(true)
    } else if text.eq_ignore_ascii_case("false") || text.eq_ignore_ascii_case("f") || text == "0" {
        Some(false)
    } else {
        None
    }
}

fn parse_timestamp_text(text: &str, index: usize) -> Result<Value, DbError> {
    let (date_str, time_str) = split_timestamp_components(text).ok_or_else(|| {
        DbError::protocol(format!(
            "invalid TIMESTAMP value for bind parameter ${index}: {text}"
        ))
    })?;
    let date = parse_date_components(date_str).map_err(|()| {
        DbError::protocol(format!(
            "invalid TIMESTAMP value for bind parameter ${index}: {text}"
        ))
    })?;
    let time = parse_time_components(time_str).map_err(|()| {
        DbError::protocol(format!(
            "invalid TIMESTAMP value for bind parameter ${index}: {text}"
        ))
    })?;
    Ok(Value::Timestamp(PrimitiveDateTime::new(date, time)))
}

fn parse_date_text(text: &str, index: usize) -> Result<Value, DbError> {
    parse_date_components(text).map(Value::Date).map_err(|()| {
        DbError::protocol(format!(
            "invalid DATE value for bind parameter ${index}: {text}"
        ))
    })
}

fn parse_time_text(text: &str, index: usize) -> Result<Value, DbError> {
    parse_time_components(text).map(Value::Time).map_err(|()| {
        DbError::protocol(format!(
            "invalid TIME value for bind parameter ${index}: {text}"
        ))
    })
}

fn parse_timetz_text(text: &str, index: usize) -> Result<Value, DbError> {
    let invalid = || {
        DbError::protocol(format!(
            "invalid TIMETZ value for bind parameter ${index}: {text}"
        ))
    };
    let (time_str, offset_str) = split_offset_suffix(text).ok_or_else(invalid)?;
    let time = parse_time_components(time_str.trim_end()).map_err(|()| invalid())?;
    let offset = parse_utc_offset_text(offset_str).map_err(|()| invalid())?;
    Ok(Value::TimeTz(time, offset))
}

fn parse_interval_text(text: &str, index: usize) -> Result<Value, DbError> {
    let mut months = 0i32;
    let mut days = 0i32;
    let mut micros = 0i64;
    for part in text.split_whitespace() {
        if let Some(value) = part.strip_suffix('m') {
            months = value.parse().map_err(|_| {
                DbError::protocol(format!(
                    "invalid INTERVAL value for bind parameter ${index}: {text}"
                ))
            })?;
        } else if let Some(value) = part.strip_suffix('d') {
            days = value.parse().map_err(|_| {
                DbError::protocol(format!(
                    "invalid INTERVAL value for bind parameter ${index}: {text}"
                ))
            })?;
        } else if let Some(value) = part.strip_suffix("us") {
            micros = value.parse().map_err(|_| {
                DbError::protocol(format!(
                    "invalid INTERVAL value for bind parameter ${index}: {text}"
                ))
            })?;
        } else {
            return Err(DbError::protocol(format!(
                "invalid INTERVAL value for bind parameter ${index}: {text}"
            )));
        }
    }
    Ok(Value::Interval(IntervalValue::new(months, days, micros)))
}

fn parse_timestamptz_text(text: &str, index: usize) -> Result<Value, DbError> {
    let invalid = || {
        DbError::protocol(format!(
            "invalid TIMESTAMPTZ value for bind parameter ${index}: {text}"
        ))
    };
    let (datetime_str, offset_str) = split_offset_suffix(text).ok_or_else(invalid)?;
    let datetime_str = datetime_str.trim_end();
    let (date_str, time_str) = split_timestamp_components(datetime_str).ok_or_else(invalid)?;
    let date = parse_date_components(date_str).map_err(|()| invalid())?;
    let time = parse_time_components(time_str).map_err(|()| invalid())?;
    let offset = parse_utc_offset_text(offset_str).map_err(|()| invalid())?;

    Ok(Value::TimestampTz(
        PrimitiveDateTime::new(date, time).assume_offset(offset),
    ))
}

fn split_timestamp_components(text: &str) -> Option<(&str, &str)> {
    text.split_once(' ')
        .or_else(|| text.split_once('T'))
        .or_else(|| text.split_once('t'))
}

fn split_offset_suffix(text: &str) -> Option<(&str, &str)> {
    if let Some(base) = text.strip_suffix('Z').or_else(|| text.strip_suffix('z')) {
        return Some((base, "+00"));
    }
    text.rfind('+')
        .or_else(|| {
            text.get(1..)
                .and_then(|rest| rest.rfind('-').map(|pos| pos + 1))
        })
        .map(|pos| (&text[..pos], &text[pos..]))
}

fn parse_utc_offset_text(text: &str) -> Result<UtcOffset, ()> {
    let (sign, rest) = if let Some(rest) = text.strip_prefix('+') {
        (1i8, rest)
    } else if let Some(rest) = text.strip_prefix('-') {
        (-1i8, rest)
    } else {
        return Err(());
    };

    let (hours, minutes) = if let Some((hours, minutes)) = rest.split_once(':') {
        (
            hours.parse::<i8>().map_err(|_| ())?,
            minutes.parse::<i8>().map_err(|_| ())?,
        )
    } else if rest.len() == 4 {
        // The HHMM form must be ASCII; otherwise byte-index slicing at 2
        // would split a multi-byte UTF-8 char and panic.
        if !rest.is_ascii() {
            return Err(());
        }
        (
            rest[..2].parse::<i8>().map_err(|_| ())?,
            rest[2..].parse::<i8>().map_err(|_| ())?,
        )
    } else {
        (rest.parse::<i8>().map_err(|_| ())?, 0)
    };

    UtcOffset::from_hms(sign * hours, sign * minutes, 0).map_err(|_| ())
}

fn parse_date_components(text: &str) -> Result<Date, ()> {
    let mut parts = text.splitn(4, '-');
    let year_raw = parts.next().ok_or(())?;
    let month_raw = parts.next().ok_or(())?;
    let day_raw = parts.next().ok_or(())?;
    if parts.next().is_some() {
        return Err(());
    }
    let year: i32 = year_raw.parse().map_err(|_| ())?;
    let month: u8 = month_raw.parse().map_err(|_| ())?;
    let day: u8 = day_raw.parse().map_err(|_| ())?;
    let month = Month::try_from(month).map_err(|_| ())?;
    Date::from_calendar_date(year, month, day).map_err(|_| ())
}

fn parse_time_components(text: &str) -> Result<Time, ()> {
    let (main, subsec) = match text.split_once('.') {
        Some((main, frac)) => (main, Some(frac)),
        None => (text, None),
    };
    let mut parts = main.splitn(4, ':');
    let hour_raw = parts.next().ok_or(())?;
    let minute_raw = parts.next().ok_or(())?;
    let second_raw = parts.next().ok_or(())?;
    if parts.next().is_some() {
        return Err(());
    }
    let hour: u8 = hour_raw.parse().map_err(|_| ())?;
    let minute: u8 = minute_raw.parse().map_err(|_| ())?;
    let second: u8 = second_raw.parse().map_err(|_| ())?;
    let micros = if let Some(frac) = subsec {
        if !frac.is_ascii() {
            return Err(());
        }
        // Right-pad the fractional digits to six positions in a stack
        // buffer instead of paying a `format!("{frac:0<6}")` String
        // allocation per parsed TIME bind parameter. Anything past the
        // first six digits is truncated (matches the `[..6]` slice).
        let frac_bytes = frac.as_bytes();
        let take = frac_bytes.len().min(6);
        // Reject non-digit chars early so the manual parse below is
        // infallible.
        for &b in &frac_bytes[..take] {
            if !b.is_ascii_digit() {
                return Err(());
            }
        }
        let mut value: u32 = 0;
        for &b in &frac_bytes[..take] {
            value = value * 10 + u32::from(b - b'0');
        }
        for _ in take..6 {
            value *= 10;
        }
        value
    } else {
        0
    };
    Time::from_hms_micro(hour, minute, second, micros).map_err(|_| ())
}

/// Decode a hex string (without the `\x` prefix) into bytes.
fn decode_hex_bytea(index: usize, hex: &str) -> Result<Vec<u8>, DbError> {
    if !hex.is_ascii() {
        return Err(DbError::protocol(format!(
            "invalid BYTEA hex value for bind parameter ${index}: non-ASCII characters"
        )));
    }
    if !hex.len().is_multiple_of(2) {
        return Err(DbError::protocol(format!(
            "invalid BYTEA hex value for bind parameter ${index}: odd number of hex digits"
        )));
    }
    // Decode pairs directly via a small helper: `from_str_radix` re-walks
    // ASCII validation and runs the generic radix machine for every two
    // characters, which adds up on long BYTEA bind parameters.
    let bytes_in = hex.as_bytes();
    let mut out = Vec::with_capacity(bytes_in.len() / 2);
    for chunk in bytes_in.chunks_exact(2) {
        let high = decode_hex_digit(chunk[0]).ok_or_else(|| invalid_hex_digit(index))?;
        let low = decode_hex_digit(chunk[1]).ok_or_else(|| invalid_hex_digit(index))?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

#[inline]
fn decode_hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[inline]
fn invalid_hex_digit(index: usize) -> DbError {
    DbError::protocol(format!(
        "invalid BYTEA hex value for bind parameter ${index}: invalid hex digit"
    ))
}

/// Parse a `PostgreSQL` text-format array literal (for example `{a,b}` or
/// `{"quoted value",NULL}`) into a `Value::Array`.
/// Maximum number of elements allowed in a single array bind parameter.
/// Prevents OOM from compact array literals like `{1,1,1,...}` packed into
/// an 8 MiB wire message (~4 M elements).
const MAX_BIND_ARRAY_ELEMENTS: usize = 100_000;

/// Maximum nesting depth for text-format array literals.
///
/// `validate_bind_array_level` and `parse_text_array_elements` recurse on
/// each `{`. Without a cap, a payload like `{{{{...}}}}` with thousands of
/// levels exhausts the thread stack and aborts the connection task. The
/// binary array decoder caps at 32 dimensions; we mirror the same bound
/// here for text arrays so both wire formats refuse the same shapes.
const MAX_BIND_ARRAY_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrayLiteralElementKind {
    Scalar,
    Array,
}

fn parse_text_array(text: &str, index: usize, inner_type: &DataType) -> Result<Value, DbError> {
    let literal = strip_bind_array_bound_prefix(text.trim());
    if !literal.starts_with('{') || !literal.ends_with('}') {
        return Err(DbError::protocol(format!(
            "invalid array literal for bind parameter ${index}: expected '{{...}}', got: {literal}"
        )));
    }

    validate_bind_array_literal(literal, index)?;
    parse_text_array_elements(literal, index, inner_type, 0)
}

fn strip_bind_array_bound_prefix(text: &str) -> &str {
    if let Some(eq_pos) = text.find('=') {
        let prefix = &text[..eq_pos];
        if prefix.starts_with('[') {
            return &text[eq_pos + 1..];
        }
    }
    text
}

fn parse_text_array_elements(
    literal: &str,
    index: usize,
    inner_type: &DataType,
    depth: usize,
) -> Result<Value, DbError> {
    if depth > MAX_BIND_ARRAY_DEPTH {
        return Err(bind_array_literal_error(
            index,
            literal,
            "Array nesting too deep.",
        ));
    }
    if !literal.starts_with('{') || !literal.ends_with('}') {
        return Err(DbError::protocol(format!(
            "invalid array literal for bind parameter ${index}: expected '{{...}}', got: {literal}"
        )));
    }
    let inner_text = &literal[1..literal.len() - 1];
    if inner_text.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }

    let trimmed = inner_text.trim_start();
    if trimmed.starts_with('{') {
        let sub_arrays =
            split_pg_array_elements(inner_text, MAX_BIND_ARRAY_ELEMENTS, index, literal)?;
        let mut values = Vec::with_capacity(sub_arrays.len());
        for sub in sub_arrays {
            let sub = sub.trim();
            if sub.starts_with('{') && sub.ends_with('}') {
                values.push(parse_text_array_elements(
                    sub,
                    index,
                    inner_type,
                    depth + 1,
                )?);
            } else {
                return Err(bind_array_literal_error(
                    index,
                    literal,
                    "Unexpected array element.",
                ));
            }
        }
        return Ok(Value::Array(values));
    }

    let raw_elements =
        split_pg_array_elements(inner_text, MAX_BIND_ARRAY_ELEMENTS, index, literal)?;
    let mut values = Vec::with_capacity(raw_elements.len());
    for elem in raw_elements {
        let elem = elem.trim();
        if elem.eq_ignore_ascii_case("NULL") {
            values.push(Value::Null);
        } else {
            let unquoted = unquote_pg_array_element(elem);
            let elem_value = coerce_bind_value(index, inner_type, Some(unquoted.as_bytes()))?;
            values.push(elem_value);
        }
    }
    Ok(Value::Array(values))
}

fn validate_bind_array_literal(literal: &str, index: usize) -> Result<(), DbError> {
    let bytes = literal.as_bytes();
    let mut pos = 0;
    validate_bind_array_level(bytes, &mut pos, 0, literal, index)?;
    skip_bind_array_whitespace(bytes, &mut pos);
    if pos != bytes.len() {
        return Err(bind_array_literal_error(
            index,
            literal,
            "Junk after closing right brace.",
        ));
    }
    Ok(())
}

fn validate_bind_array_level(
    bytes: &[u8],
    pos: &mut usize,
    depth: usize,
    literal: &str,
    index: usize,
) -> Result<ArrayLiteralElementKind, DbError> {
    if depth > MAX_BIND_ARRAY_DEPTH {
        return Err(bind_array_literal_error(
            index,
            literal,
            "Array nesting too deep.",
        ));
    }
    debug_assert_eq!(bytes.get(*pos), Some(&b'{'));
    *pos += 1;
    skip_bind_array_whitespace(bytes, pos);

    if *pos >= bytes.len() {
        return Err(bind_array_literal_error(
            index,
            literal,
            "Unexpected end of input.",
        ));
    }

    if bytes[*pos] == b'}' {
        *pos += 1;
        if depth > 0 {
            return Err(bind_array_literal_error(
                index,
                literal,
                "Unexpected \"}\" character.",
            ));
        }
        return Ok(ArrayLiteralElementKind::Scalar);
    }

    let mut expected_kind = None;
    loop {
        skip_bind_array_whitespace(bytes, pos);
        if *pos >= bytes.len() {
            return Err(bind_array_literal_error(
                index,
                literal,
                "Unexpected end of input.",
            ));
        }

        let kind = match bytes[*pos] {
            b'{' => {
                if expected_kind == Some(ArrayLiteralElementKind::Scalar) {
                    return Err(bind_array_literal_error(
                        index,
                        literal,
                        "Unexpected \"{\" character.",
                    ));
                }
                validate_bind_array_level(bytes, pos, depth + 1, literal, index)?;
                ArrayLiteralElementKind::Array
            }
            b'}' => {
                return Err(bind_array_literal_error(
                    index,
                    literal,
                    "Unexpected \"}\" character.",
                ))
            }
            b'\\' => {
                return Err(bind_array_literal_error(
                    index,
                    literal,
                    "Unexpected \"\\\" character.",
                ))
            }
            b'"' => {
                validate_bind_array_quoted_element(bytes, pos, literal, index)?;
                ArrayLiteralElementKind::Scalar
            }
            _ => {
                if expected_kind == Some(ArrayLiteralElementKind::Array) {
                    return Err(bind_array_literal_error(
                        index,
                        literal,
                        "Unexpected array element.",
                    ));
                }
                validate_bind_array_unquoted_element(bytes, pos, literal, index)?;
                ArrayLiteralElementKind::Scalar
            }
        };

        if expected_kind.is_none() {
            expected_kind = Some(kind);
        }

        skip_bind_array_whitespace(bytes, pos);
        if *pos >= bytes.len() {
            return Err(bind_array_literal_error(
                index,
                literal,
                "Unexpected end of input.",
            ));
        }

        match bytes[*pos] {
            b',' => *pos += 1,
            b'}' => {
                *pos += 1;
                return Ok(expected_kind.unwrap_or(ArrayLiteralElementKind::Scalar));
            }
            _ => {
                return Err(bind_array_literal_error(
                    index,
                    literal,
                    "Unexpected array element.",
                ))
            }
        }
    }
}

fn validate_bind_array_quoted_element(
    bytes: &[u8],
    pos: &mut usize,
    literal: &str,
    index: usize,
) -> Result<(), DbError> {
    debug_assert_eq!(bytes.get(*pos), Some(&b'"'));
    *pos += 1;
    while *pos < bytes.len() {
        match bytes[*pos] {
            b'\\' => {
                if *pos + 1 >= bytes.len() {
                    return Err(bind_array_literal_error(
                        index,
                        literal,
                        "Unexpected \"\\\" character.",
                    ));
                }
                *pos += 2;
            }
            b'"' => {
                *pos += 1;
                return Ok(());
            }
            _ => *pos += 1,
        }
    }

    Err(bind_array_literal_error(
        index,
        literal,
        "Unexpected end of input.",
    ))
}

fn validate_bind_array_unquoted_element(
    bytes: &[u8],
    pos: &mut usize,
    literal: &str,
    index: usize,
) -> Result<(), DbError> {
    let start = *pos;
    while *pos < bytes.len() {
        match bytes[*pos] {
            b',' | b'}' => break,
            b'{' => {
                return Err(bind_array_literal_error(
                    index,
                    literal,
                    "Unexpected \"{\" character.",
                ))
            }
            b'\\' => {
                return Err(bind_array_literal_error(
                    index,
                    literal,
                    "Unexpected \"\\\" character.",
                ))
            }
            b'"' => {
                return Err(bind_array_literal_error(
                    index,
                    literal,
                    "Unexpected array element.",
                ))
            }
            _ => *pos += 1,
        }
    }

    if bytes[start..*pos]
        .iter()
        .all(|byte| byte.is_ascii_whitespace())
    {
        return Err(bind_array_literal_error(
            index,
            literal,
            "Unexpected array element.",
        ));
    }

    Ok(())
}

fn skip_bind_array_whitespace(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() && bytes[*pos].is_ascii_whitespace() {
        *pos += 1;
    }
}

fn bind_array_literal_error(index: usize, literal: &str, detail: &str) -> DbError {
    DbError::protocol(format!(
        "invalid array literal for bind parameter ${index}: malformed array literal: \"{literal}\" ({detail})"
    ))
}

/// Split a comma-separated `PostgreSQL` array body into individual top-level
/// element strings, respecting double-quoted values and nested `{...}` groups.
fn split_pg_array_elements<'a>(
    s: &'a str,
    max_elements: usize,
    index: usize,
    literal: &str,
) -> Result<Vec<&'a str>, DbError> {
    let mut elements = Vec::new();
    let mut depth = 0i32;
    let mut in_quote = false;
    let mut start = 0;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if in_quote {
            if ch == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if ch == b'"' {
                in_quote = false;
            }
        } else {
            match ch {
                b'"' => in_quote = true,
                b'{' => depth += 1,
                b'}' => depth -= 1,
                b',' if depth == 0 => {
                    elements.push(&s[start..i]);
                    if elements.len() > max_elements {
                        return Err(bind_array_literal_error(
                            index,
                            literal,
                            &format!("array has too many elements (>{max_elements})"),
                        ));
                    }
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    if start <= s.len() {
        elements.push(&s[start..]);
    }
    if elements.len() > max_elements {
        return Err(bind_array_literal_error(
            index,
            literal,
            &format!("array has too many elements (>{max_elements})"),
        ));
    }
    Ok(elements)
}

/// Remove surrounding double-quotes and unescape backslash sequences.
fn unquote_pg_array_element(elem: &str) -> String {
    if elem.starts_with('"') && elem.ends_with('"') && elem.len() >= 2 {
        let inner = &elem[1..elem.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else {
                out.push(ch);
            }
        }
        out
    } else {
        elem.to_owned()
    }
}

#[cfg(test)]
#[path = "bind_tests.rs"]
mod tests;
