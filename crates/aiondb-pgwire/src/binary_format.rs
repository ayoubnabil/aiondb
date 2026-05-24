//! Binary-format encoding and decoding for the `PostgreSQL` wire protocol.
//!
//! `PostgreSQL` binary format uses big-endian byte order for numeric types.
//! Each value is preceded by a 4-byte length (handled by the caller); NULL
//! is indicated by a length of -1 with no payload.

use aiondb_core::temporal::{
    write_date_into, write_time_into, write_timestamp_into, write_timestamptz_into,
    write_timetz_into,
};
use aiondb_core::{
    DataType, DbError, IntervalValue, MacAddr, MacAddr8, NumericValue, PgDate, TextTypeModifier,
    TidValue, Value, VectorValue,
};
use bytes::BufMut;
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

use crate::format::{
    value_to_text, write_array_into, write_bytea_into, write_f32_ascii, write_f64_ascii,
    write_interval_into, write_numeric_ascii, write_uuid_ascii,
};

/// `PostgreSQL` epoch: 2000-01-01 00:00:00 UTC.
/// All PG binary date/time types are relative to this epoch.
const PG_EPOCH_DATE: Date = match Date::from_calendar_date(2000, Month::January, 1) {
    Ok(d) => d,
    // SAFETY: 2000-01-01 is always a valid date; this is a const context
    // where the compiler verifies the match is exhaustive.
    Err(_) => unreachable!(),
};
const PG_EPOCH: PrimitiveDateTime = PrimitiveDateTime::new(PG_EPOCH_DATE, Time::MIDNIGHT);
/// Microseconds per second.
const MICROS_PER_SECOND: i64 = 1_000_000;
use aiondb_core::AIONDB_VECTOR_TYPE_OID;

/// Resolve a format code for a given column index from a format code slice.
///
/// Follows `PostgreSQL` conventions:
/// - Empty slice: text (0) for all columns.
/// - One element: that format applies to every column.
/// - N elements: per-column format codes.
pub fn resolve_format_code(format_codes: &[i16], col_index: usize) -> i16 {
    match format_codes.len() {
        0 => 0,
        1 => format_codes[0],
        _ => format_codes.get(col_index).copied().unwrap_or(0),
    }
}

/// Return whether a column type has a PG wire binary result encoding.
pub const fn supports_binary_result_format(data_type: &DataType) -> bool {
    let _ = data_type;
    true
}

/// Resolve the actual result format for a column, downgrading unsupported
/// binary requests to text so `RowDescription` matches the bytes on the wire.
pub fn resolve_result_format_code(
    data_type: &DataType,
    format_codes: &[i16],
    col_index: usize,
) -> i16 {
    let requested = resolve_format_code(format_codes, col_index);
    if requested == 1 && !supports_binary_result_format(data_type) {
        0
    } else {
        requested
    }
}

/// Encode a single column value using the appropriate format (text or binary).
pub fn encode_column_value(
    value: &Value,
    data_type: &DataType,
    text_type_modifier: Option<TextTypeModifier>,
    result_formats: &[i16],
    col_index: usize,
) -> Option<Vec<u8>> {
    if resolve_result_format_code(data_type, result_formats, col_index) == 1 {
        encode_binary_value_typed(value, data_type, text_type_modifier)
    } else if text_type_modifier.is_some_and(is_vector_text_modifier) {
        match value {
            Value::Array(elements) => {
                let mut out = Vec::new();
                write_int_vector_text_into(&mut out, elements);
                Some(out)
            }
            Value::Text(text) => explicit_bound_vector_text(text).map(String::into_bytes),
            _ => value_to_text(value).map(String::into_bytes),
        }
    } else {
        value_to_text(value).map(String::into_bytes)
    }
}

/// Encode a [`Value`] in `PostgreSQL` binary format.
///
/// Returns `None` for `Value::Null` (the caller writes -1 as the column
/// length). Returns `Some(bytes)` for all other values -- the caller
/// prepends the 4-byte length.
pub fn encode_binary_value(value: &Value) -> Option<Vec<u8>> {
    let data_type = value.data_type()?;
    encode_binary_value_typed(value, &data_type, None)
}

/// Encode a column value directly into a caller-provided buffer.
///
/// Returns `true` if a value was written, `false` for NULL.
/// This avoids the intermediate `Vec<u8>` allocation of [`encode_column_value`].
pub fn encode_column_value_into(
    buf: &mut Vec<u8>,
    value: &Value,
    data_type: &DataType,
    text_type_modifier: Option<TextTypeModifier>,
    result_formats: &[i16],
    col_index: usize,
) -> bool {
    let is_binary = resolve_result_format_code(data_type, result_formats, col_index) == 1;
    encode_column_value_into_resolved(buf, value, data_type, text_type_modifier, is_binary)
}

/// Like [`encode_column_value_into`], but the caller has already resolved
/// the format code for this column. Used by hot-path batch encoders that
/// pre-resolve formats once per batch instead of once per cell.
pub fn encode_column_value_into_resolved(
    buf: &mut Vec<u8>,
    value: &Value,
    data_type: &DataType,
    text_type_modifier: Option<TextTypeModifier>,
    is_binary: bool,
) -> bool {
    if matches!(value, Value::Null) {
        return false;
    }
    if is_binary {
        encode_binary_value_into(buf, value, data_type, text_type_modifier)
    } else {
        // Text format: zero-alloc path for the types that show up most often
        // in real workloads (TPC-H, JOB, pgbench): integers, booleans, text,
        // numerics, dates, timestamps, floats, time, uuid. Everything else
        // falls back through `value_to_text`, which allocates a `String` we
        // then memcpy into the buffer.
        match value {
            Value::Int(v) => {
                crate::format::write_i32_ascii(buf, *v);
                true
            }
            Value::BigInt(v) => {
                crate::format::write_i64_ascii(buf, *v);
                true
            }
            Value::Boolean(v) => {
                buf.push(if *v { b't' } else { b'f' });
                true
            }
            Value::Text(v) => {
                if text_type_modifier.is_some_and(is_vector_text_modifier) {
                    if let Some(vector_text) = explicit_bound_vector_text(v) {
                        buf.extend_from_slice(vector_text.as_bytes());
                    } else {
                        buf.extend_from_slice(v.as_bytes());
                    }
                } else {
                    buf.extend_from_slice(v.as_bytes());
                }
                true
            }
            Value::Numeric(nv) => {
                write_numeric_ascii(buf, nv);
                true
            }
            Value::Date(d) => {
                write_date_into(buf, *d);
                true
            }
            Value::Timestamp(dt) => {
                write_timestamp_into(buf, dt);
                true
            }
            Value::TimestampTz(odt) => {
                write_timestamptz_into(buf, odt);
                true
            }
            Value::Time(t) => {
                write_time_into(buf, t);
                true
            }
            Value::TimeTz(t, off) => {
                write_timetz_into(buf, t, off);
                true
            }
            Value::Real(v) => {
                write_f32_ascii(buf, *v);
                true
            }
            Value::Double(v) => {
                write_f64_ascii(buf, *v);
                true
            }
            Value::Uuid(bytes) => {
                write_uuid_ascii(buf, bytes);
                true
            }
            Value::Jsonb(json) => {
                use std::fmt::Write as _;
                // Stream the JSONB rendering straight into the wire
                // buffer through a `fmt::Write` adapter; the previous
                // route allocated a `String` via `pg_jsonb_to_string`,
                // copied it into the buffer, and dropped it.
                let _ = write!(
                    BufFmtAdapter(buf),
                    "{}",
                    aiondb_core::value::PgJsonbDisplay(json),
                );
                true
            }
            // Stream-into-buffer for the remaining fixed-shape types
            // whose Display impl already produces the PG wire-text
            // form. Each variant's `write!` goes directly through the
            // BufFmtAdapter; the previous fall-through route allocated
            // an intermediate String via `value_to_text`, copied it
            // into the buffer, and dropped it.
            Value::Tid(t) => {
                use std::fmt::Write as _;
                let _ = write!(BufFmtAdapter(buf), "{t}");
                true
            }
            Value::PgLsn(v) => {
                use std::fmt::Write as _;
                let _ = write!(BufFmtAdapter(buf), "{v}");
                true
            }
            Value::MacAddr(v) => {
                use std::fmt::Write as _;
                let _ = write!(BufFmtAdapter(buf), "{v}");
                true
            }
            Value::MacAddr8(v) => {
                use std::fmt::Write as _;
                let _ = write!(BufFmtAdapter(buf), "{v}");
                true
            }
            Value::Money(v) => {
                use std::fmt::Write as _;
                let _ = write!(BufFmtAdapter(buf), "{}", Value::Money(*v));
                true
            }
            Value::LargeDate(d) => {
                use std::fmt::Write as _;
                let _ = write!(BufFmtAdapter(buf), "{d}");
                true
            }
            Value::Vector(vector) => {
                write_vector_text_into(buf, &vector.values);
                true
            }
            Value::Blob(data) => {
                write_bytea_into(buf, data);
                true
            }
            Value::Interval(iv) => {
                let _ = write_interval_into(&mut BufFmtAdapter(buf), iv.months, iv.days, iv.micros);
                true
            }
            Value::Array(elements) if text_type_modifier.is_some_and(is_vector_text_modifier) => {
                write_int_vector_text_into(buf, elements);
                true
            }
            Value::Array(elements) => {
                let _ = write_array_into(&mut BufFmtAdapter(buf), elements);
                true
            }
            Value::Null => {
                if let Some(text) = crate::format::value_to_text(value) {
                    buf.extend_from_slice(text.as_bytes());
                    true
                } else {
                    false
                }
            }
        }
    }
}

fn is_vector_text_modifier(modifier: TextTypeModifier) -> bool {
    matches!(
        modifier,
        TextTypeModifier::Int2Vector | TextTypeModifier::OidVector
    )
}

fn write_int_vector_text_into(buf: &mut Vec<u8>, elements: &[Value]) {
    use std::fmt::Write as _;
    let mut adapter = BufFmtAdapter(buf);
    for (i, element) in elements.iter().enumerate() {
        if i > 0 {
            let _ = adapter.write_char(' ');
        }
        match element {
            Value::Int(value) => {
                let _ = write!(adapter, "{value}");
            }
            Value::BigInt(value) => {
                let _ = write!(adapter, "{value}");
            }
            Value::Text(value) => {
                let _ = adapter.write_str(value);
            }
            Value::Null => {}
            other => {
                if let Some(text) = crate::format::value_to_text(other) {
                    let _ = adapter.write_str(&text);
                }
            }
        }
    }
}

fn explicit_bound_vector_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with('}') {
        return None;
    }
    let closing = trimmed.find("]={")?;
    let body = &trimmed[(closing + 3)..trimmed.len() - 1];
    if body.is_empty() {
        return Some(String::new());
    }

    let mut out = String::with_capacity(body.len());
    for (index, element) in body.split(',').enumerate() {
        if index > 0 {
            out.push(' ');
        }
        out.push_str(element.trim());
    }
    Some(out)
}

/// Write a `[v1,v2,...]` vector literal straight into `buf`, mirroring
/// the formatter in `crate::format::format_vector` but emitting through
/// the `BufFmtAdapter` so we never allocate the intermediate `String`.
fn write_vector_text_into(buf: &mut Vec<u8>, values: &[f32]) {
    use std::fmt::Write as _;
    buf.push(b'[');
    {
        let mut adapter = BufFmtAdapter(buf);
        for (i, v) in values.iter().enumerate() {
            if i > 0 {
                let _ = adapter.write_str(",");
            }
            if v.is_nan() {
                let _ = adapter.write_str("NaN");
            } else if *v == f32::INFINITY {
                let _ = adapter.write_str("Infinity");
            } else if *v == f32::NEG_INFINITY {
                let _ = adapter.write_str("-Infinity");
            } else {
                let _ = write!(adapter, "{v}");
            }
        }
    }
    buf.push(b']');
}

/// `fmt::Write` adapter that appends bytes onto a `Vec<u8>`. Used by the
/// JSONB-text fast path above so we don't have to round-trip through an
/// intermediate `String`.
struct BufFmtAdapter<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for BufFmtAdapter<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

/// Encode a binary value directly into a caller-provided buffer for
/// fixed-size types. Returns `true` if data was written, `false` for NULL.
pub fn encode_binary_value_into(
    buf: &mut Vec<u8>,
    value: &Value,
    data_type: &DataType,
    text_type_modifier: Option<TextTypeModifier>,
) -> bool {
    match value {
        Value::Null => false,
        Value::Int(v) => {
            buf.put_i32(*v);
            true
        }
        Value::BigInt(v) => {
            buf.put_i64(*v);
            true
        }
        Value::Boolean(v) => {
            buf.push(u8::from(*v));
            true
        }
        Value::Real(v) => {
            buf.put_f32(*v);
            true
        }
        Value::Double(v) => {
            buf.put_f64(*v);
            true
        }
        Value::Money(v) => {
            buf.put_i64(*v);
            true
        }
        _ => {
            // Fall back to the allocating path for complex types
            if let Some(bytes) = encode_binary_value_typed(value, data_type, text_type_modifier) {
                buf.extend_from_slice(&bytes);
                true
            } else {
                false
            }
        }
    }
}

fn encode_i32(v: i32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4);
    buf.put_i32(v);
    buf
}

fn encode_i64(v: i64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8);
    buf.put_i64(v);
    buf
}

fn encode_binary_value_typed(
    value: &Value,
    data_type: &DataType,
    text_type_modifier: Option<TextTypeModifier>,
) -> Option<Vec<u8>> {
    match value {
        Value::Null => None,

        Value::Int(v) => Some(encode_i32(*v)),
        Value::BigInt(v) => Some(encode_i64(*v)),
        Value::Boolean(v) => Some(vec![u8::from(*v)]),
        Value::Real(v) => {
            let mut buf = Vec::with_capacity(4);
            buf.put_f32(*v);
            Some(buf)
        }
        Value::Double(v) => {
            let mut buf = Vec::with_capacity(8);
            buf.put_f64(*v);
            Some(buf)
        }
        Value::Money(v) => Some(encode_i64(*v)),
        Value::Text(v) => Some(v.as_bytes().to_vec()),
        Value::Blob(v) => Some(v.clone()),
        Value::Tid(value) => {
            let mut buf = Vec::with_capacity(6);
            buf.put_u32(value.block());
            buf.put_u16(value.offset());
            Some(buf)
        }
        Value::PgLsn(value) => Some(value.raw().to_be_bytes().to_vec()),
        Value::MacAddr(value) => Some(value.as_bytes().to_vec()),
        Value::MacAddr8(value) => Some(value.as_bytes().to_vec()),
        Value::Uuid(bytes) => Some(bytes.to_vec()),

        Value::Timestamp(dt) => Some(encode_i64(timestamp_to_pg_micros(*dt))),
        Value::TimestampTz(odt) => {
            let utc = odt.to_offset(UtcOffset::UTC);
            let naive = PrimitiveDateTime::new(utc.date(), utc.time());
            Some(encode_i64(timestamp_to_pg_micros(naive)))
        }
        Value::Date(d) => i32::try_from((*d - PG_EPOCH_DATE).whole_days())
            .ok()
            .map(encode_i32),
        Value::LargeDate(d) => {
            let days = i32::try_from(d.days_since(PgDate::from(PG_EPOCH_DATE))).ok()?;
            Some(encode_i32(days))
        }
        Value::Time(t) => Some(encode_i64(time_to_pg_micros(*t))),
        Value::TimeTz(t, offset) => {
            // PostgreSQL binary TIMETZ stores time as microseconds since midnight
            // plus a 32-bit zone field in seconds west of UTC.
            let micros = time_to_pg_micros(*t);
            let mut buf = Vec::with_capacity(12);
            buf.put_i64(micros);
            buf.put_i32(-offset.whole_seconds());
            Some(buf)
        }
        Value::Numeric(nv) => Some(encode_pg_numeric(nv)),
        Value::Jsonb(v) => {
            // PostgreSQL JSONB binary format: version byte 0x01 + JSON text.
            // Stream the JSON text straight into the output buffer via
            // PgJsonbDisplay instead of allocating a transient String
            // through `pg_jsonb_to_string`.
            use std::fmt::Write;
            let mut buf = Vec::with_capacity(64);
            buf.push(0x01);
            let _ = write!(
                BufFmtAdapter(&mut buf),
                "{}",
                aiondb_core::value::PgJsonbDisplay(v)
            );
            Some(buf)
        }

        Value::Interval(iv) => {
            // PG binary interval: 8 bytes (microseconds i64) + 4 bytes (days i32)
            // + 4 bytes (months i32), all big-endian = 16 bytes total.
            let mut buf = Vec::with_capacity(16);
            buf.put_i64(iv.micros);
            buf.put_i32(iv.days);
            buf.put_i32(iv.months);
            Some(buf)
        }

        Value::Vector(vector) => encode_vector_binary(vector),
        Value::Array(elements) => encode_array_binary(elements, data_type, text_type_modifier),
    }
}

/// Decode a binary-encoded bind parameter value into a [`Value`].
///
/// `data` is the raw binary payload (without the 4-byte length prefix).
/// `data_type` describes the expected target type.
pub fn decode_binary_param(
    index: usize,
    data: &[u8],
    data_type: &DataType,
) -> Result<Value, DbError> {
    match data_type {
        DataType::Int => decode_compatible_int(index, data).map(Value::Int),
        DataType::BigInt => decode_compatible_bigint(index, data).map(Value::BigInt),
        DataType::Boolean => {
            expect_binary_len(index, data, 1, "BOOLEAN")?;
            Ok(Value::Boolean(data[0] != 0))
        }
        DataType::Real => {
            let arr = read_fixed_binary::<4>(index, data, "REAL")?;
            Ok(Value::Real(f32::from_be_bytes(arr)))
        }
        DataType::Double => {
            let arr = read_fixed_binary::<8>(index, data, "DOUBLE")?;
            Ok(Value::Double(f64::from_be_bytes(arr)))
        }
        DataType::Text => {
            let s = std::str::from_utf8(data).map_err(|_| {
                DbError::protocol(format!(
                    "binary bind parameter ${index}: invalid UTF-8 for TEXT"
                ))
            })?;
            Ok(Value::Text(s.to_owned()))
        }
        DataType::Blob => Ok(Value::Blob(data.to_vec())),
        DataType::MacAddr => {
            let arr = read_fixed_binary::<6>(index, data, "MACADDR")?;
            Ok(Value::MacAddr(MacAddr::new(arr)))
        }
        DataType::MacAddr8 => {
            let arr = read_fixed_binary::<8>(index, data, "MACADDR8")?;
            Ok(Value::MacAddr8(MacAddr8::new(arr)))
        }
        DataType::Tid => {
            expect_binary_len(index, data, 6, "TID")?;
            let block_bytes = read_fixed_binary::<4>(index, &data[..4], "TID")?;
            let offset_bytes = read_fixed_binary::<2>(index, &data[4..6], "TID")?;
            Ok(Value::Tid(TidValue::new(
                u32::from_be_bytes(block_bytes),
                u16::from_be_bytes(offset_bytes),
            )))
        }
        DataType::PgLsn => {
            let arr = read_fixed_binary::<8>(index, data, "PG_LSN")?;
            Ok(Value::PgLsn(aiondb_core::PgLsnValue::new(
                u64::from_be_bytes(arr),
            )))
        }
        DataType::Uuid => {
            let arr = read_fixed_binary::<16>(index, data, "UUID")?;
            Ok(Value::Uuid(arr))
        }
        DataType::Timestamp => {
            let arr = read_fixed_binary::<8>(index, data, "TIMESTAMP")?;
            let micros = i64::from_be_bytes(arr);
            let dt = pg_micros_to_timestamp(micros).map_err(|e| {
                DbError::protocol(format!(
                    "binary bind parameter ${index}: invalid TIMESTAMP: {e}"
                ))
            })?;
            Ok(Value::Timestamp(dt))
        }
        DataType::TimestampTz => {
            let arr = read_fixed_binary::<8>(index, data, "TIMESTAMPTZ")?;
            let micros = i64::from_be_bytes(arr);
            let naive = pg_micros_to_timestamp(micros).map_err(|e| {
                DbError::protocol(format!(
                    "binary bind parameter ${index}: invalid TIMESTAMPTZ: {e}"
                ))
            })?;
            Ok(Value::TimestampTz(naive.assume_utc()))
        }
        DataType::Date => {
            let arr = read_fixed_binary::<4>(index, data, "DATE")?;
            let days = i32::from_be_bytes(arr);
            let d = pg_days_to_date(days).map_err(|e| {
                DbError::protocol(format!("binary bind parameter ${index}: invalid DATE: {e}"))
            })?;
            Ok(Value::Date(d))
        }
        DataType::Time => {
            let arr = read_fixed_binary::<8>(index, data, "TIME")?;
            let micros = i64::from_be_bytes(arr);
            let t = pg_micros_to_time(micros).map_err(|e| {
                DbError::protocol(format!("binary bind parameter ${index}: invalid TIME: {e}"))
            })?;
            Ok(Value::Time(t))
        }
        DataType::TimeTz => {
            expect_binary_len(index, data, 12, "TIMETZ")?;
            let micros_bytes = read_fixed_binary::<8>(index, &data[..8], "TIMETZ")?;
            let micros = i64::from_be_bytes(micros_bytes);
            let t = pg_micros_to_time(micros).map_err(|e| {
                DbError::protocol(format!(
                    "binary bind parameter ${index}: invalid TIMETZ: {e}"
                ))
            })?;

            let zone_bytes = read_fixed_binary::<4>(index, &data[8..12], "TIMETZ")?;
            let zone_west = i32::from_be_bytes(zone_bytes);
            // V2-10 : `-zone_west` overflows when `zone_west == i32::MIN`,
            // which panics in debug builds. Validate with checked_neg so
            // the worst case becomes a clean protocol error.
            let east_seconds = zone_west.checked_neg().ok_or_else(|| {
                DbError::protocol(format!(
                    "binary bind parameter ${index}: invalid TIMETZ: zone offset {zone_west} out of range"
                ))
            })?;
            let offset = UtcOffset::from_whole_seconds(east_seconds).map_err(|e| {
                DbError::protocol(format!(
                    "binary bind parameter ${index}: invalid TIMETZ: {e}"
                ))
            })?;
            Ok(Value::TimeTz(t, offset))
        }
        DataType::Numeric => {
            let nv = decode_pg_numeric(index, data)?;
            Ok(Value::Numeric(nv))
        }
        DataType::Money => {
            let arr = read_fixed_binary::<8>(index, data, "MONEY")?;
            Ok(Value::Money(i64::from_be_bytes(arr)))
        }
        DataType::Jsonb => {
            if data.is_empty() {
                return Err(DbError::protocol(format!(
                    "binary bind parameter ${index}: empty JSONB payload"
                )));
            }
            let version = data[0];
            if version != 0x01 {
                return Err(DbError::protocol(format!(
                    "binary bind parameter ${index}: unsupported JSONB version {version}"
                )));
            }
            let json_text = std::str::from_utf8(&data[1..]).map_err(|_| {
                DbError::protocol(format!(
                    "binary bind parameter ${index}: invalid UTF-8 in JSONB payload"
                ))
            })?;
            let v: serde_json::Value = serde_json::from_str(json_text).map_err(|e| {
                DbError::protocol(format!(
                    "binary bind parameter ${index}: invalid JSON in JSONB payload: {e}"
                ))
            })?;
            Ok(Value::Jsonb(v))
        }
        DataType::Interval => {
            expect_binary_len(index, data, 16, "INTERVAL")?;
            let micros_bytes = read_fixed_binary::<8>(index, &data[0..8], "INTERVAL")?;
            let micros = i64::from_be_bytes(micros_bytes);
            let days_bytes = read_fixed_binary::<4>(index, &data[8..12], "INTERVAL")?;
            let days = i32::from_be_bytes(days_bytes);
            let months_bytes = read_fixed_binary::<4>(index, &data[12..16], "INTERVAL")?;
            let months = i32::from_be_bytes(months_bytes);
            Ok(Value::Interval(IntervalValue::new(months, days, micros)))
        }
        DataType::Vector { dims, .. } => decode_vector_binary(index, data, *dims),
        DataType::Array(inner) => decode_array_binary(index, data, inner),

        #[allow(unreachable_patterns)]
        other => Err(DbError::protocol(format!(
            "binary format for type {other} is not supported"
        ))),
    }
}

fn encode_vector_binary(vector: &VectorValue) -> Option<Vec<u8>> {
    let dims = i16::try_from(vector.values.len()).ok()?;
    let mut buf = Vec::with_capacity(4 + vector.values.len() * 4);
    buf.put_i16(dims);
    buf.put_i16(0);
    for value in &vector.values {
        buf.put_f32(*value);
    }
    Some(buf)
}

fn decode_vector_binary(index: usize, data: &[u8], expected_dims: u32) -> Result<Value, DbError> {
    expect_binary_min_len(index, data, 4, "VECTOR")?;

    let dims = i16::from_be_bytes(data[0..2].try_into().map_err(|_| {
        DbError::protocol(format!(
            "binary bind parameter ${index}: malformed VECTOR dimensions header"
        ))
    })?);
    if dims < 0 {
        return Err(DbError::protocol(format!(
            "binary bind parameter ${index}: VECTOR dimensions cannot be negative"
        )));
    }
    let reserved = i16::from_be_bytes(data[2..4].try_into().map_err(|_| {
        DbError::protocol(format!(
            "binary bind parameter ${index}: malformed VECTOR reserved header"
        ))
    })?);
    if reserved != 0 {
        return Err(DbError::protocol(format!(
            "binary bind parameter ${index}: unsupported VECTOR header flags {reserved}"
        )));
    }

    let dims = usize::try_from(dims).map_err(|_| {
        DbError::protocol(format!(
            "binary bind parameter ${index}: VECTOR dimensions out of range"
        ))
    })?;
    let expected_len = 4 + dims * 4;
    if data.len() != expected_len {
        return Err(DbError::protocol(format!(
            "binary bind parameter ${index}: expected {expected_len} bytes for VECTOR payload, got {}",
            data.len()
        )));
    }
    let dims_u32 = u32::try_from(dims).map_err(|_| {
        DbError::protocol(format!(
            "binary bind parameter ${index}: VECTOR dimensions out of range"
        ))
    })?;
    if expected_dims != 0 && dims_u32 != expected_dims {
        return Err(DbError::protocol(format!(
            "binary bind parameter ${index}: expected VECTOR({expected_dims}), got VECTOR({dims})"
        )));
    }

    let mut values = Vec::with_capacity(dims);
    for chunk in data[4..].chunks_exact(4) {
        values.push(f32::from_be_bytes(chunk.try_into().map_err(|_| {
            DbError::protocol(format!(
                "binary bind parameter ${index}: malformed VECTOR element payload"
            ))
        })?));
    }
    Ok(Value::Vector(VectorValue::new(dims_u32, values)))
}

fn encode_array_binary(
    elements: &[Value],
    data_type: &DataType,
    text_type_modifier: Option<TextTypeModifier>,
) -> Option<Vec<u8>> {
    let base_type = match data_type {
        DataType::Array(inner) => array_base_element_type(inner.as_ref()),
        other => array_base_element_type(other),
    };
    let element_oid = match (base_type, text_type_modifier) {
        (DataType::Text, Some(modifier)) => modifier.scalar_type_oid(),
        (dt, _) => dt.pg_oid().unwrap_or(25),
    };
    let dimensions = infer_array_dimensions(elements)?;

    let mut payload = Vec::new();
    let mut has_nulls = false;
    if !dimensions.is_empty()
        && !encode_array_payload(
            elements,
            base_type,
            &dimensions,
            0,
            text_type_modifier,
            &mut payload,
            &mut has_nulls,
        )
    {
        return None;
    }

    let mut buf = Vec::with_capacity(12 + dimensions.len() * 8 + payload.len());
    buf.put_i32(i32::try_from(dimensions.len()).ok()?);
    buf.put_i32(i32::from(has_nulls));
    buf.put_u32(element_oid);
    for dim in &dimensions {
        buf.put_i32(*dim);
        buf.put_i32(1);
    }
    buf.extend_from_slice(&payload);
    Some(buf)
}

/// Defensive cap matching `MAX_ARRAY_DIMENSIONS` for the decode side. The
/// encode helpers below recurse on each nested `Value::Array`, so a
/// deeply-nested value built by internal code (or a future bug) would
/// otherwise abort the connection task with a stack overflow.
const MAX_ENCODE_ARRAY_DEPTH: usize = 32;

fn infer_array_dimensions(elements: &[Value]) -> Option<Vec<i32>> {
    infer_array_dimensions_with_depth(elements, 0)
}

fn infer_array_dimensions_with_depth(elements: &[Value], depth: usize) -> Option<Vec<i32>> {
    if depth > MAX_ENCODE_ARRAY_DEPTH {
        return None;
    }
    if elements.is_empty() {
        return Some(Vec::new());
    }

    let nested_dims = elements.iter().find_map(|value| match value {
        Value::Array(inner) => infer_array_dimensions_with_depth(inner, depth + 1),
        Value::Null => None,
        _ => Some(Vec::new()),
    });
    let nested_dims = nested_dims.unwrap_or_default();

    for value in elements {
        match value {
            Value::Array(inner) => {
                if infer_array_dimensions_with_depth(inner, depth + 1)? != nested_dims {
                    return None;
                }
            }
            Value::Null => {
                if !nested_dims.is_empty() {
                    return None;
                }
            }
            _ => {
                if !nested_dims.is_empty() {
                    return None;
                }
            }
        }
    }

    let mut dims = Vec::with_capacity(1 + nested_dims.len());
    dims.push(i32::try_from(elements.len()).ok()?);
    dims.extend(nested_dims);
    Some(dims)
}

fn encode_array_payload(
    elements: &[Value],
    base_type: &DataType,
    dimensions: &[i32],
    depth: usize,
    text_type_modifier: Option<TextTypeModifier>,
    payload: &mut Vec<u8>,
    has_nulls: &mut bool,
) -> bool {
    let expected_len = dimensions
        .get(depth)
        .copied()
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_default();
    if expected_len != elements.len() {
        return false;
    }

    if depth + 1 == dimensions.len() {
        for value in elements {
            match value {
                Value::Null => {
                    *has_nulls = true;
                    payload.put_i32(-1);
                }
                Value::Array(_) => return false,
                _ => {
                    // Reserve a 4-byte length placeholder, encode the
                    // element directly into `payload` via the buffer-aware
                    // helper, then back-patch the length. This skips the
                    // per-element `Vec<u8>` allocation that
                    // `encode_binary_value_typed` would have returned.
                    let len_offset = payload.len();
                    payload.extend_from_slice(&[0u8; 4]);
                    let data_start = payload.len();
                    if !encode_binary_value_into(payload, value, base_type, text_type_modifier) {
                        return false;
                    }
                    let data_len = payload.len() - data_start;
                    let Ok(encoded_len) = i32::try_from(data_len) else {
                        return false;
                    };
                    payload[len_offset..len_offset + 4].copy_from_slice(&encoded_len.to_be_bytes());
                }
            }
        }
        return true;
    }

    for value in elements {
        let Value::Array(inner) = value else {
            return false;
        };
        if !encode_array_payload(
            inner,
            base_type,
            dimensions,
            depth + 1,
            text_type_modifier,
            payload,
            has_nulls,
        ) {
            return false;
        }
    }

    true
}

fn decode_array_binary(index: usize, data: &[u8], inner_type: &DataType) -> Result<Value, DbError> {
    expect_binary_min_len(index, data, 12, "ARRAY")?;

    let ndim = i32::from_be_bytes(data[0..4].try_into().map_err(|_| {
        DbError::protocol(format!(
            "binary bind parameter ${index}: malformed ARRAY ndim header"
        ))
    })?);
    if ndim < 0 {
        return Err(DbError::protocol(format!(
            "binary bind parameter ${index}: ARRAY dimensions cannot be negative"
        )));
    }
    const MAX_ARRAY_DIMENSIONS: i32 = 32;
    if ndim > MAX_ARRAY_DIMENSIONS {
        return Err(DbError::protocol(format!(
            "binary bind parameter ${index}: too many array dimensions ({ndim}), maximum is {MAX_ARRAY_DIMENSIONS}"
        )));
    }
    let ndim = usize::try_from(ndim).map_err(|_| {
        DbError::protocol(format!(
            "binary bind parameter ${index}: ARRAY dimensions out of range"
        ))
    })?;
    let _flags = i32::from_be_bytes(data[4..8].try_into().map_err(|_| {
        DbError::protocol(format!(
            "binary bind parameter ${index}: malformed ARRAY flags header"
        ))
    })?);
    let element_oid = u32::from_be_bytes(data[8..12].try_into().map_err(|_| {
        DbError::protocol(format!(
            "binary bind parameter ${index}: malformed ARRAY element type header"
        ))
    })?);
    let expected_element_oid = match array_base_element_type(inner_type) {
        DataType::Vector { .. } => AIONDB_VECTOR_TYPE_OID,
        dt => dt.pg_oid().unwrap_or(25),
    };
    if !array_element_oid_is_compatible(inner_type, element_oid) {
        return Err(DbError::protocol(format!(
            "binary bind parameter ${index}: ARRAY element type oid mismatch: expected {expected_element_oid}, got {element_oid}"
        )));
    }

    let mut offset = 12usize;
    let mut dimensions = Vec::with_capacity(ndim);
    for _ in 0..ndim {
        if offset + 8 > data.len() {
            return Err(DbError::protocol(format!(
                "binary bind parameter ${index}: truncated ARRAY dimension header"
            )));
        }
        let len = i32::from_be_bytes(data[offset..offset + 4].try_into().map_err(|_| {
            DbError::protocol(format!(
                "binary bind parameter ${index}: malformed ARRAY dimension length"
            ))
        })?);
        let _lbound =
            i32::from_be_bytes(data[offset + 4..offset + 8].try_into().map_err(|_| {
                DbError::protocol(format!(
                    "binary bind parameter ${index}: malformed ARRAY lower bound"
                ))
            })?);
        if len < 0 {
            return Err(DbError::protocol(format!(
                "binary bind parameter ${index}: ARRAY dimension length cannot be negative"
            )));
        }
        dimensions.push(usize::try_from(len).map_err(|_| {
            DbError::protocol(format!(
                "binary bind parameter ${index}: ARRAY dimension length out of range"
            ))
        })?);
        offset += 8;
    }

    // Cap element count to keep per-parameter allocations bounded under hostile
    // bind payloads. This still allows very large arrays while reducing OOM risk.
    const MAX_ARRAY_ELEMENTS: usize = 250_000;

    let total_values = if dimensions.is_empty() {
        0
    } else {
        dimensions.iter().try_fold(1usize, |acc, dim| {
            acc.checked_mul(*dim).ok_or_else(|| {
                DbError::protocol(format!(
                    "binary bind parameter ${index}: ARRAY dimensions overflow the supported size"
                ))
            })
        })?
    };

    if total_values > MAX_ARRAY_ELEMENTS {
        return Err(DbError::protocol(format!(
            "binary bind parameter ${index}: ARRAY has {total_values} elements, exceeds maximum of {MAX_ARRAY_ELEMENTS}"
        )));
    }
    let remaining_bytes = data.len().saturating_sub(offset);
    let max_values_from_payload = remaining_bytes / 4;
    if total_values > max_values_from_payload {
        return Err(DbError::protocol(format!(
            "binary bind parameter ${index}: ARRAY header declares {total_values} elements, but payload can hold at most {max_values_from_payload}"
        )));
    }

    let base_type = array_base_element_type(inner_type);
    let mut flat_values = Vec::with_capacity(total_values);
    for _ in 0..total_values {
        if offset + 4 > data.len() {
            return Err(DbError::protocol(format!(
                "binary bind parameter ${index}: truncated ARRAY element header"
            )));
        }
        let len = i32::from_be_bytes(data[offset..offset + 4].try_into().map_err(|_| {
            DbError::protocol(format!(
                "binary bind parameter ${index}: malformed ARRAY element length"
            ))
        })?);
        offset += 4;
        if len == -1 {
            flat_values.push(Value::Null);
            continue;
        }
        if len < -1 {
            return Err(DbError::protocol(format!(
                "binary bind parameter ${index}: invalid ARRAY element length {len}"
            )));
        }
        let len = usize::try_from(len).map_err(|_| {
            DbError::protocol(format!(
                "binary bind parameter ${index}: ARRAY element length out of range"
            ))
        })?;
        if offset + len > data.len() {
            return Err(DbError::protocol(format!(
                "binary bind parameter ${index}: truncated ARRAY element payload"
            )));
        }
        let value = decode_binary_param(index, &data[offset..offset + len], base_type)?;
        flat_values.push(value);
        offset += len;
    }

    if offset != data.len() {
        return Err(DbError::protocol(format!(
            "binary bind parameter ${index}: trailing bytes after ARRAY payload"
        )));
    }
    if dimensions.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }

    let mut position = 0usize;
    Ok(build_array_from_flat(
        &flat_values,
        &dimensions,
        0,
        &mut position,
    ))
}

fn array_element_oid_is_compatible(inner_type: &DataType, element_oid: u32) -> bool {
    match array_base_element_type(inner_type) {
        DataType::Vector { .. } => element_oid == AIONDB_VECTOR_TYPE_OID,
        DataType::Text => matches!(element_oid, 18 | 19 | 25 | 1042 | 1043),
        dt => element_oid == dt.pg_oid().unwrap_or(25),
    }
}

fn build_array_from_flat(
    flat_values: &[Value],
    dimensions: &[usize],
    depth: usize,
    position: &mut usize,
) -> Value {
    let len = dimensions[depth];
    if depth + 1 == dimensions.len() {
        let mut values = Vec::with_capacity(len);
        for _ in 0..len {
            values.push(flat_values[*position].clone());
            *position += 1;
        }
        Value::Array(values)
    } else {
        let mut values = Vec::with_capacity(len);
        for _ in 0..len {
            values.push(build_array_from_flat(
                flat_values,
                dimensions,
                depth + 1,
                position,
            ));
        }
        Value::Array(values)
    }
}

fn array_base_element_type(data_type: &DataType) -> &DataType {
    let mut current = data_type;
    while let DataType::Array(inner) = current {
        current = inner.as_ref();
    }
    current
}

fn binary_len_error(
    index: usize,
    data_len: usize,
    expected_len: usize,
    type_name: &str,
) -> DbError {
    DbError::protocol(format!(
        "binary bind parameter ${index}: expected {expected_len} bytes for {type_name}, got {data_len}"
    ))
}

fn binary_min_len_error(
    index: usize,
    data_len: usize,
    minimum_len: usize,
    type_name: &str,
) -> DbError {
    DbError::protocol(format!(
        "binary bind parameter ${index}: {type_name} payload must be at least {minimum_len} bytes (got {data_len})"
    ))
}

fn expect_binary_len(
    index: usize,
    data: &[u8],
    expected_len: usize,
    type_name: &str,
) -> Result<(), DbError> {
    if data.len() != expected_len {
        return Err(binary_len_error(index, data.len(), expected_len, type_name));
    }
    Ok(())
}

fn expect_binary_min_len(
    index: usize,
    data: &[u8],
    minimum_len: usize,
    type_name: &str,
) -> Result<(), DbError> {
    if data.len() < minimum_len {
        return Err(binary_min_len_error(
            index,
            data.len(),
            minimum_len,
            type_name,
        ));
    }
    Ok(())
}

fn read_fixed_binary<const N: usize>(
    index: usize,
    data: &[u8],
    type_name: &str,
) -> Result<[u8; N], DbError> {
    data.try_into()
        .map_err(|_| binary_len_error(index, data.len(), N, type_name))
}

fn decode_compatible_int(index: usize, data: &[u8]) -> Result<i32, DbError> {
    match data.len() {
        2 => {
            let arr = read_fixed_binary::<2>(index, data, "SMALLINT")?;
            Ok(i16::from_be_bytes(arr).into())
        }
        4 => {
            let arr = read_fixed_binary::<4>(index, data, "INT")?;
            Ok(i32::from_be_bytes(arr))
        }
        8 => {
            let arr = read_fixed_binary::<8>(index, data, "BIGINT")?;
            i32::try_from(i64::from_be_bytes(arr)).map_err(|_| {
                DbError::protocol(format!(
                    "binary bind parameter ${index}: BIGINT value out of range for INT"
                ))
            })
        }
        _ => Err(DbError::protocol(format!(
            "binary bind parameter ${index}: expected 2, 4, or 8 bytes for INT-compatible input, got {}",
            data.len()
        ))),
    }
}

fn decode_compatible_bigint(index: usize, data: &[u8]) -> Result<i64, DbError> {
    match data.len() {
        2 => {
            let arr = read_fixed_binary::<2>(index, data, "SMALLINT")?;
            Ok(i16::from_be_bytes(arr).into())
        }
        4 => {
            let arr = read_fixed_binary::<4>(index, data, "INT")?;
            Ok(i32::from_be_bytes(arr).into())
        }
        8 => {
            let arr = read_fixed_binary::<8>(index, data, "BIGINT")?;
            Ok(i64::from_be_bytes(arr))
        }
        _ => Err(DbError::protocol(format!(
            "binary bind parameter ${index}: expected 2, 4, or 8 bytes for BIGINT-compatible input, got {}",
            data.len()
        ))),
    }
}

// ---------------------------------------------------------------------------
// Date/time helpers
// ---------------------------------------------------------------------------

/// Convert a `PrimitiveDateTime` to microseconds since PG epoch (2000-01-01).
fn timestamp_to_pg_micros(dt: PrimitiveDateTime) -> i64 {
    let duration = dt - PG_EPOCH;
    duration.whole_seconds() * MICROS_PER_SECOND + i64::from(dt.microsecond())
        - i64::from(PG_EPOCH.microsecond())
}

/// Convert microseconds since PG epoch to a `PrimitiveDateTime`.
fn pg_micros_to_timestamp(micros: i64) -> Result<PrimitiveDateTime, String> {
    let whole_seconds = micros.div_euclid(MICROS_PER_SECOND);
    let remainder_micros = u32::try_from(micros.rem_euclid(MICROS_PER_SECOND))
        .map_err(|_| format!("timestamp microseconds remainder out of range: {micros}"))?;
    let duration = time::Duration::new(whole_seconds, 0);
    let base = PG_EPOCH + duration;
    base.replace_microsecond(remainder_micros)
        .map_err(|e| format!("{e}"))
}

/// Convert days since PG epoch to a `Date`.
fn pg_days_to_date(days: i32) -> Result<Date, String> {
    let duration = time::Duration::days(i64::from(days));
    let d = PG_EPOCH_DATE
        .checked_add(duration)
        .ok_or_else(|| format!("date out of range: {days} days from PG epoch"))?;
    Ok(d)
}

/// Convert a `Time` to microseconds since midnight.
fn time_to_pg_micros(t: Time) -> i64 {
    let (h, m, s) = (
        i64::from(t.hour()),
        i64::from(t.minute()),
        i64::from(t.second()),
    );
    let us = i64::from(t.microsecond());
    h * 3_600 * MICROS_PER_SECOND + m * 60 * MICROS_PER_SECOND + s * MICROS_PER_SECOND + us
}

/// Convert microseconds since midnight to a `Time`.
fn pg_micros_to_time(micros: i64) -> Result<Time, String> {
    if !(0..24 * 3_600 * MICROS_PER_SECOND).contains(&micros) {
        return Err(format!("time out of range: {micros} microseconds"));
    }
    let total_secs = micros / MICROS_PER_SECOND;
    let remainder_us = u32::try_from(micros % MICROS_PER_SECOND)
        .map_err(|_| format!("time microseconds remainder out of range: {micros}"))?;
    let hour = u8::try_from(total_secs / 3600)
        .map_err(|_| format!("time hour component out of range: {micros}"))?;
    let minute = u8::try_from((total_secs % 3600) / 60)
        .map_err(|_| format!("time minute component out of range: {micros}"))?;
    let second = u8::try_from(total_secs % 60)
        .map_err(|_| format!("time second component out of range: {micros}"))?;
    Time::from_hms_micro(hour, minute, second, remainder_us).map_err(|e| format!("{e}"))
}

// ---------------------------------------------------------------------------
// PostgreSQL binary numeric format
// ---------------------------------------------------------------------------

/// Encode a `NumericValue` in `PostgreSQL` binary numeric format.
///
/// Format: ndigits (i16) + weight (i16) + sign (i16) + dscale (i16) + digits (i16[])
///
/// `PostgreSQL` stores NUMERIC as base-10000 digits. `weight` is the exponent
/// of the first digit group (i.e., value = sum(digit[i] * 10000^(weight - i))).
/// `dscale` is the display scale (number of fractional decimal digits).
/// `sign` is 0x0000 for positive, 0x4000 for negative, 0xC000 for NaN.
fn encode_pg_numeric(nv: &NumericValue) -> Vec<u8> {
    if nv.is_nan() || nv.is_infinite() {
        let sign: i16 = if nv.is_nan() {
            i16::from_be_bytes(0xC000u16.to_be_bytes())
        } else if nv.is_pos_infinity() {
            i16::from_be_bytes(0xD000u16.to_be_bytes())
        } else {
            i16::from_be_bytes(0xF000u16.to_be_bytes())
        };
        let mut buf = Vec::with_capacity(8);
        buf.put_i16(0); // ndigits
        buf.put_i16(0); // weight
        buf.put_i16(sign);
        buf.put_i16(0); // dscale
        return buf;
    }

    let is_negative = nv.is_negative();
    let coeff_str = nv.coefficient_abs_string();

    if nv.is_zero() {
        let mut buf = Vec::with_capacity(8);
        buf.put_i16(0); // ndigits
        buf.put_i16(0); // weight
        buf.put_i16(0x0000); // sign = positive
        buf.put_i16(i16::try_from(nv.scale).unwrap_or(i16::MAX)); // dscale
        return buf;
    }

    let scale_usize = usize::try_from(nv.scale).unwrap_or(usize::MAX);
    let total_decimal_digits = coeff_str.len();
    let integer_digits = total_decimal_digits.saturating_sub(scale_usize);

    let integer_groups = integer_digits.div_ceil(4);
    let weight = if integer_groups > 0 {
        i16::try_from(integer_groups.saturating_sub(1)).unwrap_or(i16::MAX)
    } else {
        let leading_frac_zeros = scale_usize.saturating_sub(total_decimal_digits);
        let shift = leading_frac_zeros / 4;
        i16::try_from(shift)
            .unwrap_or(i16::MAX)
            .saturating_add(1)
            .saturating_neg()
    };

    let padded_digits = encode_numeric_digits_str(&coeff_str, nv.scale, integer_groups);

    let ndigits = padded_digits.len();
    let sign: i16 = if is_negative { 0x4000 } else { 0x0000 };
    let dscale = i16::try_from(nv.scale).unwrap_or(i16::MAX);

    let mut buf = Vec::with_capacity(8 + ndigits * 2);
    buf.put_i16(i16::try_from(ndigits).unwrap_or(i16::MAX));
    buf.put_i16(weight);
    buf.put_i16(sign);
    buf.put_i16(dscale);
    for &d in &padded_digits {
        buf.put_i16(d);
    }
    buf
}

/// Encode the coefficient into properly aligned base-10000 digit groups.
/// Works with the absolute decimal string representation.
fn encode_numeric_digits_str(coeff_str: &str, scale: u32, integer_groups: usize) -> Vec<i16> {
    // Walk through the conceptually-padded decimal layout digit-by-digit,
    // accumulating base-10000 groups directly into the output vector. The
    // previous implementation built three intermediate `String`s
    // (int_padding zeros + coeff, frac padding triple, full join) before
    // re-parsing 4-byte windows back into i16 --- six heap allocations
    // per encoded NUMERIC. The layout is:
    //
    //     [int_padding  zeros][integer_digits from coeff]
    //     [frac_leading zeros][fractional digits from coeff]
    //     [frac_right_padding zeros]
    //
    // ...with `coeff_str` occupying a single contiguous run starting at
    // `coeff_offset`. Each base-10000 group is the value of 4 consecutive
    // decimal digits in that virtual string.
    let total_len = coeff_str.len();

    // The integer part has `integer_digits` decimal digits.
    let scale_usize = usize::try_from(scale).unwrap_or(usize::MAX);
    let integer_digits = total_len.saturating_sub(scale_usize);

    // Pad the integer part on the left to be a multiple of 4.
    let padded_int_width = integer_groups * 4;
    let int_padding = padded_int_width - integer_digits;

    // Leading fractional zeros only matter when the coefficient has no
    // integer part (e.g. `0.0001`). In every other case `total_len >=
    // scale_usize` so this is zero.
    let frac_leading_zeros = scale_usize.saturating_sub(total_len);

    // For fractional digits, we need the total fractional decimal count
    // to be a multiple of 4 as well (pad on the right). `padded_frac_width`
    // is implicit in `frac_groups * 4` and consulted only via
    // `padded_int_width + frac_leading_zeros` below.
    let frac_groups = scale_usize.div_ceil(4);

    let total_groups = integer_groups + frac_groups;
    let coeff_bytes = coeff_str.as_bytes();
    // `coeff_str` starts at this offset in the conceptual padded string.
    // When the coefficient has integer digits the run begins right after
    // the int-padding zeros; when it does not, it begins after the
    // (entirely-zero) integer block plus the fractional leading zeros.
    let coeff_offset = if integer_digits > 0 {
        int_padding
    } else {
        padded_int_width + frac_leading_zeros
    };
    let coeff_end = coeff_offset + total_len;

    let digit_at = |pos: usize| -> u8 {
        if pos >= coeff_offset && pos < coeff_end {
            coeff_bytes[pos - coeff_offset] - b'0'
        } else {
            0
        }
    };

    let mut digits: Vec<i16> = Vec::with_capacity(total_groups);
    for group in 0..total_groups {
        let base = group * 4;
        let v = (i16::from(digit_at(base))) * 1000
            + (i16::from(digit_at(base + 1))) * 100
            + (i16::from(digit_at(base + 2))) * 10
            + (i16::from(digit_at(base + 3)));
        digits.push(v);
    }

    // Remove trailing zero groups (PostgreSQL convention).
    while digits.last() == Some(&0) && digits.len() > 1 {
        digits.pop();
    }

    digits
}

/// Decode `PostgreSQL` binary numeric format into a `NumericValue`.
fn decode_pg_numeric(index: usize, data: &[u8]) -> Result<NumericValue, DbError> {
    expect_binary_min_len(index, data, 8, "NUMERIC")?;

    let raw_ndigits = i16::from_be_bytes([data[0], data[1]]);
    if raw_ndigits < 0 {
        return Err(DbError::protocol(format!(
            "binary bind parameter ${index}: invalid NUMERIC digit count: {raw_ndigits}"
        )));
    }
    let ndigits = usize::try_from(raw_ndigits).map_err(|_| {
        DbError::protocol(format!(
            "binary bind parameter ${index}: invalid NUMERIC digit count: {raw_ndigits}"
        ))
    })?;
    let weight = i16::from_be_bytes([data[2], data[3]]);
    let sign = i16::from_be_bytes([data[4], data[5]]);
    let raw_dscale = i16::from_be_bytes([data[6], data[7]]);
    if raw_dscale < 0 {
        return Err(DbError::protocol(format!(
            "binary bind parameter ${index}: invalid NUMERIC display scale: {raw_dscale}"
        )));
    }
    let dscale = u32::try_from(raw_dscale).map_err(|_| {
        DbError::protocol(format!(
            "binary bind parameter ${index}: invalid NUMERIC display scale: {raw_dscale}"
        ))
    })?;

    let special_value = match u16::from_be_bytes(sign.to_be_bytes()) {
        0xC000 => Some(NumericValue::NAN),
        0xD000 => Some(NumericValue::INFINITY),
        0xF000 => Some(NumericValue::NEG_INFINITY),
        _ => None,
    };

    if special_value.is_none() && sign != 0x0000 && sign != 0x4000 {
        return Err(DbError::protocol(format!(
            "binary bind parameter ${index}: invalid NUMERIC sign: {sign:#06x}"
        )));
    }

    let expected_len = 8 + ndigits * 2;
    expect_binary_min_len(index, data, expected_len, "NUMERIC")?;

    if let Some(value) = special_value {
        if ndigits != 0 {
            return Err(DbError::protocol(format!(
                "binary bind parameter ${index}: special NUMERIC values must not contain digit groups"
            )));
        }
        return Ok(value);
    }

    // Read base-10000 digits and validate they are in [0, 9999].
    let mut pg_digits = Vec::with_capacity(ndigits);
    for i in 0..ndigits {
        let offset = 8 + i * 2;
        let d = i16::from_be_bytes([data[offset], data[offset + 1]]);
        if !(0..=9999).contains(&d) {
            return Err(DbError::protocol(format!(
                "binary bind parameter ${index}: NUMERIC digit out of range: {d}"
            )));
        }
        pg_digits.push(i64::from(d));
    }

    // Reconstruct the value.
    // value = sum(digit[i] * 10000^(weight - i)) for i in 0..ndigits
    // The result has `dscale` fractional decimal digits.
    let mut coefficient: i128 = 0;
    for &d in &pg_digits {
        coefficient = coefficient
            .checked_mul(10000)
            .and_then(|c| c.checked_add(i128::from(d)))
            .ok_or_else(|| {
                DbError::protocol(format!(
                    "binary bind parameter ${index}: NUMERIC coefficient overflow"
                ))
            })?;
    }

    // The value represented is: coefficient * 10000^(weight - ndigits + 1)
    // We want to express this as coefficient * 10^(-dscale).
    // So: coefficient * 10^(4 * (weight - ndigits + 1)) = result * 10^(-dscale)
    // => result = coefficient * 10^(4 * (weight - ndigits + 1) + dscale)
    let ndigits_i64 = i64::try_from(ndigits).unwrap_or(i64::MAX);
    let exponent = 4 * (i64::from(weight) - ndigits_i64 + 1) + i64::from(dscale);
    if exponent > 0 {
        let exponent_u32 = u32::try_from(exponent).map_err(|_| {
            DbError::protocol(format!(
                "binary bind parameter ${index}: NUMERIC value overflow (exponent {exponent})"
            ))
        })?;
        let factor = 10i128.checked_pow(exponent_u32).ok_or_else(|| {
            DbError::protocol(format!(
                "binary bind parameter ${index}: NUMERIC value overflow (exponent {exponent})"
            ))
        })?;
        coefficient = coefficient.checked_mul(factor).ok_or_else(|| {
            DbError::protocol(format!(
                "binary bind parameter ${index}: NUMERIC coefficient overflow"
            ))
        })?;
    } else if exponent < 0 {
        let exponent_u32 = u32::try_from(-exponent).map_err(|_| {
            DbError::protocol(format!(
                "binary bind parameter ${index}: NUMERIC value overflow (exponent {exponent})"
            ))
        })?;
        let factor = 10i128.checked_pow(exponent_u32).ok_or_else(|| {
            DbError::protocol(format!(
                "binary bind parameter ${index}: NUMERIC value overflow (exponent {exponent})"
            ))
        })?;
        // Banker's rounding (round-half-even) so the binary numeric
        // decoder matches PG's `numeric_recv`. Truncating integer
        // which is observable round-trip data loss for clients.
        let quotient = coefficient / factor;
        let remainder = coefficient % factor;
        let half = factor / 2;
        coefficient = match remainder.abs().cmp(&half) {
            std::cmp::Ordering::Less => quotient,
            std::cmp::Ordering::Greater => {
                if (coefficient < 0) ^ (factor < 0) {
                    quotient - 1
                } else {
                    quotient + 1
                }
            }
            std::cmp::Ordering::Equal => {
                // Tie -> round to even.
                if quotient % 2 == 0 {
                    quotient
                } else if (coefficient < 0) ^ (factor < 0) {
                    quotient - 1
                } else {
                    quotient + 1
                }
            }
        };
    }

    let is_negative = sign == 0x4000;
    if is_negative {
        coefficient = -coefficient;
    }

    Ok(NumericValue::new(coefficient, dscale))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "binary_format_tests.rs"]
mod tests;
