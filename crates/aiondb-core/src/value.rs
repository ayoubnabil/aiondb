use std::fmt;
use std::fmt::Write as _;

use time::{Date, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

use crate::{
    DataType, IntervalValue, MacAddr, MacAddr8, NumericValue, PgDate, PgLsnValue, TidValue,
    VectorElementType,
};

#[inline]
fn sql_f32_eq(left: f32, right: f32) -> bool {
    left == right || (left.is_nan() && right.is_nan())
}

#[inline]
fn sql_f64_eq(left: f64, right: f64) -> bool {
    left == right || (left.is_nan() && right.is_nan())
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct VectorValue {
    pub dims: u32,
    pub values: Vec<f32>,
}

/// Defensive cap for `VectorValue` deserialization, matching the WAL codec's
/// `MAX_VECTOR_DIMS`. A higher request indicates a corrupted or hostile
/// payload - accepting it would allocate gigabytes of `f32`s.
const VECTOR_VALUE_DESERIALIZE_DIM_CAP: usize = 1_000_000;

impl<'de> serde::Deserialize<'de> for VectorValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Deserialize via a helper to a structurally identical shape, then
        // route through `new()` so the `dims == values.len()` invariant is
        // enforced regardless of what the on-disk / on-wire payload claims.
        #[derive(serde::Deserialize)]
        struct Helper {
            dims: u32,
            values: Vec<f32>,
        }
        let h = Helper::deserialize(deserializer)?;
        if h.values.len() > VECTOR_VALUE_DESERIALIZE_DIM_CAP {
            return Err(<D::Error as serde::de::Error>::custom(format!(
                "vector payload has {} values, exceeds {} cap",
                h.values.len(),
                VECTOR_VALUE_DESERIALIZE_DIM_CAP
            )));
        }
        Ok(Self::new(h.dims, h.values))
    }
}

impl VectorValue {
    /// Build a vector value while enforcing the internal invariant
    /// `dims == values.len()`.
    ///
    /// The `dims` input is treated as advisory; the canonical dimensions are
    /// always derived from `values.len()` to avoid mismatched metadata and
    /// accidental oversized allocations.
    #[must_use]
    pub fn new(_dims: u32, values: Vec<f32>) -> Self {
        let normalized_dims = u32::try_from(values.len()).unwrap_or(u32::MAX);
        Self {
            dims: normalized_dims,
            values,
        }
    }

    /// Parse a vector from text format `[f32, f32, ...]`.
    /// Returns `None` if the input is malformed.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let inner = s.strip_prefix('[')?.strip_suffix(']')?;
        let trimmed = inner.trim();
        if trimmed.is_empty() {
            return Some(Self {
                dims: 0,
                values: Vec::new(),
            });
        }
        let values: Option<Vec<f32>> = trimmed.split(',').map(|v| v.trim().parse().ok()).collect();
        let values = values?;
        if values.iter().any(|value| !value.is_finite()) {
            return None;
        }
        let dims = u32::try_from(values.len()).unwrap_or(u32::MAX);
        Some(Self::new(dims, values))
    }
}

impl PartialEq for VectorValue {
    fn eq(&self, other: &Self) -> bool {
        self.dims == other.dims
            && self.values.len() == other.values.len()
            && self
                .values
                .iter()
                .zip(other.values.iter())
                .all(|(left, right)| sql_f32_eq(*left, *right))
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum Value {
    Null,
    Int(i32),
    BigInt(i64),
    Real(f32),
    Double(f64),
    Numeric(NumericValue),
    Money(i64),
    Text(String),
    Boolean(bool),
    Blob(Vec<u8>),
    Timestamp(PrimitiveDateTime),
    Date(Date),
    LargeDate(PgDate),
    Time(Time),
    TimeTz(Time, UtcOffset),
    Interval(IntervalValue),
    Uuid([u8; 16]),
    TimestampTz(OffsetDateTime),
    Tid(TidValue),
    PgLsn(PgLsnValue),
    Jsonb(serde_json::Value),
    MacAddr(MacAddr),
    MacAddr8(MacAddr8),
    Vector(VectorValue),
    Array(Vec<Value>),
}

/// Maximum recursion depth honoured by `Value::Array` traversals (Display,
/// `PartialEq`, `data_type`). Mirrors `MAX_JSONB_DISPLAY_DEPTH` so an attacker who
/// can land an arbitrarily-nested `Value::Array` (e.g. via prepared-statement
/// parameters or recursive CTE output) cannot drive a stack overflow.
const MAX_VALUE_ARRAY_DEPTH: usize = 256;

// NOTE: a custom `Drop` for `Value` would let us iteratively drain
// deeply-nested `Value::Array(Vec<Value>)` chains and prevent stack
// overflow on auto-derived recursive deallocation. The Rust borrow
// checker, however, refuses pattern moves out of types that implement
// `Drop`, which would touch ~30 existing match arms across the codebase.
// Because the only surface that can produce extreme nesting is
// `validate_pg_array_level` (already capped at 256) and pgwire bind
// (capped at MAX_BIND_ARRAY_DEPTH = 32), the stack-overflow-on-drop
// path is unreachable from network input. Revisit if the workspace
// gains a reason to allow deep nesting elsewhere.

fn array_element_data_type_with_depth(elements: &[Value], depth: usize) -> DataType {
    if depth >= MAX_VALUE_ARRAY_DEPTH {
        return DataType::Text;
    }
    for v in elements {
        match v {
            Value::Null => {}
            Value::Array(inner) => {
                return DataType::Array(Box::new(array_element_data_type_with_depth(
                    inner,
                    depth + 1,
                )))
            }
            other => {
                if let Some(t) = other.data_type() {
                    return t;
                }
            }
        }
    }
    DataType::Text
}

fn value_eq_with_depth(left: &Value, right: &Value, depth: usize) -> bool {
    match (left, right) {
        (Value::Array(_), _) | (_, Value::Array(_)) if depth >= MAX_VALUE_ARRAY_DEPTH => {
            // Stop descending into adversarial nesting; treat as inequal.
            false
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|(l, r)| value_eq_with_depth(l, r, depth + 1))
        }
        // Neither side carries Array nesting; safe to fall back to the public
        // PartialEq impl for the scalar/leaf variants.
        (l, r) => l == r,
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Int(left), Self::Int(right)) => left == right,
            (Self::BigInt(left), Self::BigInt(right)) | (Self::Money(left), Self::Money(right)) => {
                left == right
            }
            (Self::Real(left), Self::Real(right)) => sql_f32_eq(*left, *right),
            (Self::Double(left), Self::Double(right)) => sql_f64_eq(*left, *right),
            (Self::Numeric(left), Self::Numeric(right)) => left == right,
            (Self::Text(left), Self::Text(right)) => left == right,
            (Self::Boolean(left), Self::Boolean(right)) => left == right,
            (Self::Blob(left), Self::Blob(right)) => left == right,
            (Self::Timestamp(left), Self::Timestamp(right)) => left == right,
            (Self::Date(left), Self::Date(right)) => left == right,
            (Self::LargeDate(left), Self::LargeDate(right)) => left == right,
            (Self::Time(left), Self::Time(right)) => left == right,
            (Self::TimeTz(left_time, left_offset), Self::TimeTz(right_time, right_offset)) => {
                left_time == right_time && left_offset == right_offset
            }
            (Self::Interval(left), Self::Interval(right)) => left == right,
            (Self::Uuid(left), Self::Uuid(right)) => left == right,
            (Self::TimestampTz(left), Self::TimestampTz(right)) => left == right,
            (Self::Tid(left), Self::Tid(right)) => left == right,
            (Self::PgLsn(left), Self::PgLsn(right)) => left == right,
            (Self::Jsonb(left), Self::Jsonb(right)) => left == right,
            (Self::MacAddr(left), Self::MacAddr(right)) => left == right,
            (Self::MacAddr8(left), Self::MacAddr8(right)) => left == right,
            (Self::Vector(left), Self::Vector(right)) => left == right,
            (Self::Array(left), Self::Array(right)) => {
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right.iter())
                        .all(|(l, r)| value_eq_with_depth(l, r, 1))
            }
            _ => false,
        }
    }
}

impl Value {
    #[must_use]
    pub fn data_type(&self) -> Option<DataType> {
        match self {
            Self::Null => None,
            Self::Int(_) => Some(DataType::Int),
            Self::BigInt(_) => Some(DataType::BigInt),
            Self::Real(_) => Some(DataType::Real),
            Self::Double(_) => Some(DataType::Double),
            Self::Numeric(_) => Some(DataType::Numeric),
            Self::Money(_) => Some(DataType::Money),
            Self::Text(_) => Some(DataType::Text),
            Self::Boolean(_) => Some(DataType::Boolean),
            Self::Blob(_) => Some(DataType::Blob),
            Self::Timestamp(_) => Some(DataType::Timestamp),
            Self::Date(_) | Self::LargeDate(_) => Some(DataType::Date),
            Self::Time(_) => Some(DataType::Time),
            Self::TimeTz(_, _) => Some(DataType::TimeTz),
            Self::Interval(_) => Some(DataType::Interval),
            Self::Uuid(_) => Some(DataType::Uuid),
            Self::TimestampTz(_) => Some(DataType::TimestampTz),
            Self::Tid(_) => Some(DataType::Tid),
            Self::PgLsn(_) => Some(DataType::PgLsn),
            Self::Jsonb(_) => Some(DataType::Jsonb),
            Self::MacAddr(_) => Some(DataType::MacAddr),
            Self::MacAddr8(_) => Some(DataType::MacAddr8),
            Self::Vector(value) => Some(DataType::Vector {
                dims: value.dims,
                element_type: VectorElementType::Float32,
            }),
            Self::Array(elements) => Some(DataType::Array(Box::new(
                array_element_data_type_with_depth(elements, 1),
            ))),
        }
    }

    #[inline]
    #[must_use]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Returns true if this value is a numeric type (`Int`, `BigInt`, `Real`, `Double`, `Numeric`, `Boolean`).
    #[inline]
    #[must_use]
    pub const fn is_numeric_coercible(&self) -> bool {
        matches!(
            self,
            Self::Int(_)
                | Self::BigInt(_)
                | Self::Real(_)
                | Self::Double(_)
                | Self::Numeric(_)
                | Self::Money(_)
                | Self::Boolean(_)
        )
    }

    /// Parse a UUID string and return `Value::Uuid`.
    ///
    /// Accepted forms are:
    /// - canonical: `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`
    /// - compact:   `xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx`
    /// - canonical with braces: `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`
    ///
    /// Any malformed delimiter placement is rejected to preserve `PostgreSQL`
    /// compatibility and avoid ambiguous parsing in security-sensitive paths.
    /// Decode a single hex ASCII byte to its nibble value.
    #[inline]
    fn hex_nibble(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }

    #[must_use]
    pub fn uuid_from_str(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        let core = if let Some(inner) = trimmed
            .strip_prefix('{')
            .and_then(|inner| inner.strip_suffix('}'))
        {
            inner
        } else {
            trimmed
        };

        let canonical_hyphen_positions = [8usize, 13, 18, 23];
        let has_hyphens = core.contains('-');
        if has_hyphens {
            if core.len() != 36 {
                return None;
            }
            for (idx, ch) in core.char_indices() {
                let should_be_hyphen = canonical_hyphen_positions.contains(&idx);
                if should_be_hyphen != (ch == '-') {
                    return None;
                }
            }
        } else if core.len() != 32 {
            return None;
        }

        // Zero-allocation hex parse: walk the core bytes, skip hyphens, and
        // decode hex nibbles directly into the output array.
        let raw = core.as_bytes();
        let mut bytes = [0u8; 16];
        let mut byte_idx = 0usize;
        let mut nibble_idx = 0u8; // 0 = high nibble, 1 = low nibble
        for &b in raw {
            if b == b'-' {
                continue;
            }
            let nibble = Self::hex_nibble(b)?;
            if nibble_idx == 0 {
                if byte_idx >= 16 {
                    return None;
                }
                bytes[byte_idx] = nibble << 4;
                nibble_idx = 1;
            } else {
                bytes[byte_idx] |= nibble;
                byte_idx += 1;
                nibble_idx = 0;
            }
        }
        if byte_idx != 16 || nibble_idx != 0 {
            return None;
        }

        Some(Self::Uuid(bytes))
    }
}

/// Format a UUID as `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
fn fmt_uuid(bytes: &[u8; 16], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    // 32 hex digits + 4 hyphens = 36 chars
    let mut buf = [0u8; 36];
    let mut pos = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if i == 4 || i == 6 || i == 8 || i == 10 {
            buf[pos] = b'-';
            pos += 1;
        }
        buf[pos] = HEX[usize::from(b >> 4)];
        buf[pos + 1] = HEX[usize::from(b & 0x0f)];
        pos += 2;
    }
    // buf is guaranteed valid ASCII
    let Ok(s) = std::str::from_utf8(&buf[..pos]) else {
        return Err(fmt::Error);
    };
    f.write_str(s)
}

/// Write ".NNNNNNNNN" (fractional seconds) trimming trailing zeros, directly to formatter.
/// Writes nothing if `nano` is 0. Up to 9 digits of precision.
fn write_fractional_seconds(f: &mut fmt::Formatter<'_>, nano: u32) -> fmt::Result {
    if nano == 0 {
        return Ok(());
    }
    let digits = [
        (nano / 100_000_000) % 10,
        (nano / 10_000_000) % 10,
        (nano / 1_000_000) % 10,
        (nano / 100_000) % 10,
        (nano / 10_000) % 10,
        (nano / 1_000) % 10,
        (nano / 100) % 10,
        (nano / 10) % 10,
        nano % 10,
    ];
    let last_nonzero = digits.iter().rposition(|&d| d != 0).unwrap_or(0);
    f.write_char('.')?;
    for &d in &digits[..=last_nonzero] {
        let digit = u8::try_from(d).map_err(|_| fmt::Error)?;
        f.write_char(char::from(b'0' + digit))?;
    }
    Ok(())
}

/// Format a `Time` in PG style: `HH:MM:SS` or `HH:MM:SS.fffffffff` (trailing zeros trimmed).
#[allow(clippy::trivially_copy_pass_by_ref)]
fn fmt_pg_time(t: &Time, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let nano = t.nanosecond();
    write!(f, "{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second())?;
    write_fractional_seconds(f, nano)
}

/// Format a `Time` with offset in PG style: `HH:MM:SS[.ffffff]+OO[:MM]`.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn fmt_pg_timetz(t: &Time, offset: &UtcOffset, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    fmt_pg_time(t, f)?;
    let (oh, om, _) = offset.as_hms();
    let negative = oh < 0 || om < 0;
    let sign = if negative { '-' } else { '+' };
    let abs_hour_offset = oh.unsigned_abs();
    if om == 0 {
        write!(f, "{sign}{abs_hour_offset:02}")
    } else {
        let abs_minute_offset = om.unsigned_abs();
        write!(f, "{sign}{abs_hour_offset:02}:{abs_minute_offset:02}")
    }
}

/// Format a `PrimitiveDateTime` in PG style: `YYYY-MM-DD HH:MM:SS[.fffffffff]`.
fn fmt_pg_timestamp(dt: &PrimitiveDateTime, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let (year, month, day) = (dt.year(), u8::from(dt.month()), dt.day());
    let (hour, minute, second) = (dt.hour(), dt.minute(), dt.second());
    let nano = dt.nanosecond();
    write!(
        f,
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
    )?;
    write_fractional_seconds(f, nano)
}

/// Format an `OffsetDateTime` in PG style: `YYYY-MM-DD HH:MM:SS[.fffffffff]+OO[:MM]`.
fn fmt_pg_timestamptz(odt: &OffsetDateTime, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let (year, month, day) = (odt.year(), u8::from(odt.month()), odt.day());
    let (hour, minute, second) = (odt.hour(), odt.minute(), odt.second());
    let nano = odt.nanosecond();
    let offset = odt.offset();
    let (oh, om, _) = offset.as_hms();
    write!(
        f,
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
    )?;
    write_fractional_seconds(f, nano)?;
    let negative = oh < 0 || om < 0;
    let sign = if negative { '-' } else { '+' };
    let abs_hour_offset = oh.unsigned_abs();
    if om == 0 {
        write!(f, "{sign}{abs_hour_offset:02}")
    } else {
        let abs_minute_offset = om.unsigned_abs();
        write!(f, "{sign}{abs_hour_offset:02}:{abs_minute_offset:02}")
    }
}

/// Format a `serde_json::Value` in `PostgreSQL` style.
///
/// PG rules:
/// - Object: `{"key": value, ...}` (space after colon, space after comma)
/// - Array: `[elem, elem, ...]` (space after comma)
/// - Strings: `"text"` with standard JSON escaping
///
/// Write a JSON-escaped string (without surrounding quotes) into any
/// `fmt::Write` sink.  Used by both the Display formatter and the pretty
/// printer to avoid duplicating the escape table.
///
/// The hot path is text with no escape triggers (the dominant shape for
/// JSON keys + most string values): the loop bulk-copies the longest
/// trigger-free run via `write_str` and only calls the per-byte escape
/// branch when an actual trigger byte is hit. UTF-8 multi-byte
/// sequences start at byte >= 0x80 which never collide with the
/// trigger set (control bytes < 0x20 plus `"` and `\\`), so slicing on
/// raw byte indices stays at valid char boundaries.
fn write_json_escaped_str(w: &mut impl fmt::Write, s: &str) -> fmt::Result {
    let bytes = s.as_bytes();
    let mut last = 0usize;
    for (idx, &b) in bytes.iter().enumerate() {
        if b >= 0x20 && b != b'"' && b != b'\\' {
            continue;
        }
        if idx > last {
            w.write_str(&s[last..idx])?;
        }
        match b {
            b'"' => w.write_str("\\\"")?,
            b'\\' => w.write_str("\\\\")?,
            b'\n' => w.write_str("\\n")?,
            b'\r' => w.write_str("\\r")?,
            b'\t' => w.write_str("\\t")?,
            0x08 => w.write_str("\\b")?,
            0x0C => w.write_str("\\f")?,
            other => write!(w, "\\u{:04x}", u32::from(other))?,
        }
        last = idx + 1;
    }
    if last < bytes.len() {
        w.write_str(&s[last..])?;
    }
    Ok(())
}

/// Write a JSON object key: `"escaped_key"` (with surrounding quotes and
/// minimal escaping for keys that are already well-formed).
///
/// Same bulk-copy strategy as `write_json_escaped_str`, restricted to
/// the two key triggers `"` and `\\`. JSON keys in real workloads are
/// almost always trigger-free identifiers, so the fast path is one
/// `write_str` of the entire key between two quote chars.
fn write_json_key(w: &mut impl fmt::Write, key: &str) -> fmt::Result {
    w.write_char('"')?;
    let bytes = key.as_bytes();
    let mut last = 0usize;
    for (idx, &b) in bytes.iter().enumerate() {
        if b != b'"' && b != b'\\' {
            continue;
        }
        if idx > last {
            w.write_str(&key[last..idx])?;
        }
        w.write_char('\\')?;
        w.write_char(b as char)?;
        last = idx + 1;
    }
    if last < bytes.len() {
        w.write_str(&key[last..])?;
    }
    w.write_char('"')
}

/// Format a `serde_json::Number` without scientific notation.
fn format_json_number_to(w: &mut impl fmt::Write, n: &serde_json::Number) -> fmt::Result {
    // Try integer paths first --- the dominant case for real JSONB
    // payloads --- so we never allocate the speculative `n.to_string()`
    // that the previous implementation always performed before
    // checking for scientific notation. The `write!(w, "{i}")` route
    // formats directly through the `fmt::Write` trait, skipping the
    // intermediate String entirely.
    if let Some(i) = n.as_i64() {
        write!(w, "{i}")
    } else if let Some(u) = n.as_u64() {
        write!(w, "{u}")
    } else if let Some(fv) = n.as_f64() {
        write_f64_no_exponent(w, fv)
    } else {
        // Arbitrary-precision numbers (with the `arbitrary_precision`
        // serde_json feature) land here. Fall back to the string repr.
        // PG renders these without scientific notation, so we only
        // re-route through f64 when the textual form would surface an
        // exponent.
        let s = n.to_string();
        if s.contains('e') || s.contains('E') {
            // Best-effort: parse as f64 and emit non-exponent form.
            if let Ok(fv) = s.parse::<f64>() {
                write_f64_no_exponent(w, fv)
            } else {
                w.write_str(&s)
            }
        } else {
            w.write_str(&s)
        }
    }
}

/// - Numbers: rendered as exact numeric (no scientific notation)
/// - Booleans: `true`/`false`
/// - Null: `null`
///
/// Maximum nesting depth permitted when text-encoding a JSONB / Value
/// composite. Adversarial inputs (`'{"a":{"a":{"a":...}}}'` thousands
/// deep) would otherwise blow the host stack via fmt recursion. A
/// hard cap keeps Display bounded while still rendering shapes far
/// deeper than any sensible workload requires.
pub const MAX_JSONB_DISPLAY_DEPTH: usize = 256;

fn fmt_pg_jsonb_value(v: &serde_json::Value, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    fmt_pg_jsonb_value_at_depth(v, f, 0)
}

fn fmt_pg_jsonb_value_at_depth(
    v: &serde_json::Value,
    f: &mut fmt::Formatter<'_>,
    depth: usize,
) -> fmt::Result {
    if depth >= MAX_JSONB_DISPLAY_DEPTH {
        return f.write_str("\"<jsonb too deep>\"");
    }
    match v {
        serde_json::Value::Null => f.write_str("null"),
        serde_json::Value::Bool(b) => {
            if *b {
                f.write_str("true")
            } else {
                f.write_str("false")
            }
        }
        serde_json::Value::Number(n) => format_json_number_to(f, n),
        serde_json::Value::String(s) => {
            f.write_char('"')?;
            write_json_escaped_str(f, s)?;
            f.write_char('"')
        }
        serde_json::Value::Array(arr) => {
            f.write_str("[")?;
            for (i, elem) in arr.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                fmt_pg_jsonb_value_at_depth(elem, f, depth + 1)?;
            }
            f.write_str("]")
        }
        serde_json::Value::Object(map) => {
            f.write_str("{")?;
            for (i, (key, val)) in map.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write_json_key(f, key)?;
                f.write_str(": ")?;
                fmt_pg_jsonb_value_at_depth(val, f, depth + 1)?;
            }
            f.write_str("}")
        }
    }
}

/// Format an f64 without scientific notation, matching PG's JSONB numeric output.
///
/// Rust's `f64` `Display` always uses fixed-point notation (never `e`/`E`),
/// so falling through to `format!("{v}")` is sufficient. The earlier
/// JSONB output for sci-notation-emitting numbers like `1.5e-300`.
/// Render a finite `f64` in non-exponent decimal form into a `fmt::Write`
/// sink. Used by JSONB encoding paths so we never round-trip through an
/// intermediate `String` for the JSONB-number variant.
#[allow(clippy::cast_precision_loss)]
fn write_f64_no_exponent<W: fmt::Write>(w: &mut W, v: f64) -> fmt::Result {
    const I64_MIN_F64: f64 = -9_223_372_036_854_775_808.0;
    const I64_MAX_F64: f64 = 9_223_372_036_854_775_807.0;

    if v == 0.0 {
        return w.write_str("0");
    }
    if v.fract() == 0.0 && (I64_MIN_F64..=I64_MAX_F64).contains(&v) {
        // Integer-valued in i64 range: render as an integer to avoid the
        // `e+12` scientific form that `{v}` produces for large magnitudes.
        return write!(w, "{v:.0}");
    }
    write!(w, "{v}")
}

/// `Display` adapter that emits a `serde_json::Value` in `PostgreSQL` JSONB
/// text style. Lets callers stream the rendering directly into any
/// `fmt::Write` target (a wire buffer, an existing `String`, a formatter)
/// without the intermediate heap-allocated `String` that
/// [`pg_jsonb_to_string`] returns.
pub struct PgJsonbDisplay<'a>(pub &'a serde_json::Value);

impl fmt::Display for PgJsonbDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_pg_jsonb_value(self.0, f)
    }
}

/// Format a `serde_json::Value` in `PostgreSQL` style, returning a `String`.
///
/// This is the public API for use in functions like `jsonb_extract_path_text`
/// and the `->>` operator, where nested JSONB values need PG formatting.
/// On the per-row pgwire encode path, prefer [`PgJsonbDisplay`] so the
/// rendering streams straight into the wire buffer.
#[must_use]
pub fn pg_jsonb_to_string(v: &serde_json::Value) -> String {
    PgJsonbDisplay(v).to_string()
}

/// Format a `serde_json::Value` with PG-compatible pretty-printing (4-space indent).
#[must_use]
pub fn pg_jsonb_pretty(v: &serde_json::Value) -> String {
    let mut out = String::new();
    pg_jsonb_pretty_impl(v, 0, &mut out);
    out
}

/// 64-space buffer used by `push_indent` to bulk-emit indentation in
/// chunks instead of pushing one char at a time. 64 covers indent
/// levels up to depth 16 (the pretty printer steps by 4 per level)
/// in a single `push_str`; deeper levels loop a couple of times.
const INDENT_SPACES: &str = "                                                                ";

fn push_indent(out: &mut String, n: usize) {
    // The old `out.extend(std::iter::repeat_n(' ', n))` ran the per
    // `String::extend` byte-by-byte fast path internally, but still
    // bounds-checked once per char. Writing a chunked slice instead
    // amortises bounds checks - for typical jsonb_pretty output (1-3
    // nesting levels, n in {4, 8, 12}), it collapses to a single
    // `push_str` with `extend_from_slice` of an existing const slice.
    let mut remaining = n;
    out.reserve(n);
    while remaining > 0 {
        let chunk = remaining.min(INDENT_SPACES.len());
        out.push_str(&INDENT_SPACES[..chunk]);
        remaining -= chunk;
    }
}

fn pg_jsonb_pretty_impl(v: &serde_json::Value, indent: usize, out: &mut String) {
    match v {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(b) => {
            if *b {
                out.push_str("true");
            } else {
                out.push_str("false");
            }
        }
        serde_json::Value::Number(n) => {
            // write! to String is infallible; unwrap is safe.
            let _ = format_json_number_to(out, n);
        }
        serde_json::Value::String(s) => {
            out.push('"');
            let _ = write_json_escaped_str(out, s);
            out.push('"');
        }
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                out.push_str("[\n");
                push_indent(out, indent);
                out.push(']');
                return;
            }
            out.push_str("[\n");
            let child_indent = indent + 4;
            for (i, elem) in arr.iter().enumerate() {
                push_indent(out, child_indent);
                pg_jsonb_pretty_impl(elem, child_indent, out);
                if i + 1 < arr.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(out, indent);
            out.push(']');
        }
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{\n");
                push_indent(out, indent);
                out.push('}');
                return;
            }
            out.push_str("{\n");
            let child_indent = indent + 4;
            let len = map.len();
            for (i, (key, val)) in map.iter().enumerate() {
                push_indent(out, child_indent);
                let _ = write_json_key(out, key);
                out.push_str(": ");
                pg_jsonb_pretty_impl(val, child_indent, out);
                if i + 1 < len {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(out, indent);
            out.push('}');
        }
    }
}

/// Check if a text element in a PG array literal needs double-quoting.
/// Elements containing commas, braces, quotes, backslashes, whitespace,
/// or that look like NULL need quoting.
fn pg_array_needs_quoting(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    if s.eq_ignore_ascii_case("NULL") {
        return true;
    }
    // Single byte-level scan replaces 5 separate `String::contains(char)`
    // passes plus a `chars().any(is_ascii_whitespace)` traversal. All
    // trigger bytes are ASCII (the original used `is_ascii_whitespace`,
    // not the unicode-table `is_whitespace`); UTF-8 leading bytes
    // (>= 0x80) never collide so the byte scan covers exactly the
    // same set the previous implementation tested.
    s.as_bytes().iter().any(|b| {
        matches!(
            *b,
            b',' | b'{' | b'}' | b'"' | b'\\' | b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C
        )
    })
}

fn format_pg_interval_verbose(iv: &IntervalValue) -> String {
    let mut out = String::with_capacity(48);
    let _ = write_pg_interval_verbose_into(&mut out, iv);
    out
}

/// Write `IntervalValue` in PG verbose style (`@ 1 year 2 mons …`)
/// directly into `out`. Replaces the `Vec<String> + join(" ") +
/// format!("@ {joined}")` cascade that the previous formatter built;
/// each "1 year" / "2 mons" / "3 hours" segment is now pushed
/// straight into the output buffer through `fmt::Write`.
#[allow(clippy::cast_sign_loss)]
fn write_pg_interval_verbose_into<W: fmt::Write>(out: &mut W, iv: &IntervalValue) -> fmt::Result {
    let has_positive = iv.months > 0 || iv.days > 0 || iv.micros > 0;
    let has_negative = iv.months < 0 || iv.days < 0 || iv.micros < 0;
    let whole_negative = has_negative && !has_positive;
    let mixed_signs = has_positive && has_negative;

    if iv.months == 0 && iv.days == 0 && iv.micros == 0 {
        return out.write_str("@ 0");
    }

    out.write_str("@")?;
    let mut wrote_part = false;
    let mut emit_part = |out: &mut W, sign: &str, n: u64, label: &str| -> fmt::Result {
        out.write_char(' ')?;
        write!(out, "{sign}{n} {label}")?;
        wrote_part = true;
        Ok(())
    };

    if iv.months != 0 {
        let total_months = i64::from(iv.months.unsigned_abs());
        let sign = if mixed_signs && iv.months < 0 && !whole_negative {
            "-"
        } else {
            ""
        };
        let years = (total_months / 12).cast_unsigned();
        let mons = (total_months % 12).cast_unsigned();
        if years != 0 {
            emit_part(out, sign, years, if years == 1 { "year" } else { "years" })?;
        }
        if mons != 0 {
            emit_part(out, sign, mons, if mons == 1 { "mon" } else { "mons" })?;
        }
    }

    if iv.days != 0 {
        let days = i64::from(iv.days.unsigned_abs()).cast_unsigned();
        let sign = if mixed_signs && iv.days < 0 && !whole_negative {
            "-"
        } else {
            ""
        };
        emit_part(out, sign, days, if days == 1 { "day" } else { "days" })?;
    }

    if iv.micros != 0 {
        let total_micros = iv.micros.unsigned_abs();
        let mut sign = if mixed_signs && iv.micros < 0 && !whole_negative {
            "-"
        } else {
            ""
        };
        let hours = total_micros / 3_600_000_000;
        let mins = (total_micros % 3_600_000_000) / 60_000_000;
        let secs = (total_micros % 60_000_000) / 1_000_000;
        let frac = total_micros % 1_000_000;

        if hours != 0 {
            emit_part(out, sign, hours, if hours == 1 { "hour" } else { "hours" })?;
            sign = "";
        }
        if mins != 0 {
            emit_part(out, sign, mins, if mins == 1 { "min" } else { "mins" })?;
            sign = "";
        }
        if secs != 0 || frac != 0 {
            if frac == 0 {
                emit_part(out, sign, secs, if secs == 1 { "sec" } else { "secs" })?;
            } else {
                // Render six fractional digits into a stack buffer
                // and trim trailing zeros without allocating an
                // intermediate String for the `{:06}` format.
                let mut digits = [0u8; 6];
                let mut n = frac as u32;
                for slot in digits.iter_mut().rev() {
                    *slot = b'0' + (n % 10) as u8;
                    n /= 10;
                }
                let mut end = digits.len();
                while end > 0 && digits[end - 1] == b'0' {
                    end -= 1;
                }
                let trimmed = std::str::from_utf8(&digits[..end]).unwrap_or("");
                out.write_char(' ')?;
                write!(out, "{sign}{secs}.{trimmed} secs")?;
                wrote_part = true;
            }
        }
    }

    let _ = wrote_part;
    if whole_negative {
        out.write_str(" ago")?;
    }
    Ok(())
}

fn write_pg_array_scalar(f: &mut fmt::Formatter<'_>, value: &Value) -> fmt::Result {
    // Borrow the existing Text payload instead of cloning it. For arrays
    // of TEXT values (the dominant shape - `text[]` columns, jsonb-keys
    // arrays, log line aggregations) this saves a full-string heap copy
    // per element on the per-row Display path.
    let rendered: std::borrow::Cow<'_, str> = match value {
        Value::Text(text) => std::borrow::Cow::Borrowed(text.as_str()),
        Value::Interval(iv) => std::borrow::Cow::Owned(format_pg_interval_verbose(iv)),
        other => std::borrow::Cow::Owned(other.to_string()),
    };
    let rendered: &str = rendered.as_ref();

    if pg_array_needs_quoting(rendered) {
        f.write_str("\"")?;
        // Bulk-copy the chunks between escape triggers via `write_str`
        // instead of dispatching per char through `write!("{ch}")`.
        // Two triggers (`"`, `\\`) are single-byte ASCII; UTF-8 leading
        // bytes (>= 0x80) cannot collide so byte slicing remains at
        // valid char boundaries. Same shape as iter143's
        // `write_quoted_array_element` in pgwire/format.rs.
        let bytes = rendered.as_bytes();
        let mut last = 0usize;
        for (idx, &b) in bytes.iter().enumerate() {
            if b != b'"' && b != b'\\' {
                continue;
            }
            if idx > last {
                f.write_str(&rendered[last..idx])?;
            }
            f.write_char('\\')?;
            f.write_char(b as char)?;
            last = idx + 1;
        }
        if last < bytes.len() {
            f.write_str(&rendered[last..])?;
        }
        f.write_str("\"")
    } else {
        f.write_str(&rendered)
    }
}

/// Write the PG-style money rendering (`-$1,234.56`) directly into
/// `out`. Lets the `Display` impl on `Value::Money` stream into the
/// formatter instead of allocating an intermediate `String` for the
/// dollar grouping plus another for the final formatted literal.
fn write_pg_money_into<W: fmt::Write>(out: &mut W, cents: i64) -> fmt::Result {
    let negative = cents < 0;
    let abs_cents = cents.unsigned_abs();
    let dollars = abs_cents / 100;
    let cents_part = abs_cents % 100;
    // Worst case rendering: `-$` (2) + 20 digits + 6 comma separators
    // + `.` + 2 cents digits = 31 bytes. Build the whole rendering in
    // one stack buffer and emit it through a single `write_str`,
    // skipping the per-digit + per-comma `write_char` dispatches that
    // each carry their own vtable indirection through the `fmt::Write`
    // trait object.
    let mut buf = [0u8; 32];
    let mut pos = 0usize;
    if negative {
        buf[pos] = b'-';
        pos += 1;
    }
    buf[pos] = b'$';
    pos += 1;

    // Render dollars into a temp scratch (u64::MAX has 20 digits)
    // then copy them into `buf` with comma separators every three
    // digits from the right.
    let mut tmp = [0u8; 20];
    let mut n = dollars;
    let mut start = tmp.len();
    if n == 0 {
        start -= 1;
        tmp[start] = b'0';
    } else {
        while n > 0 {
            start -= 1;
            tmp[start] = b'0' + (n % 10) as u8;
            n /= 10;
        }
    }
    let digit_slice = &tmp[start..];
    let total = digit_slice.len();
    for (idx, &b) in digit_slice.iter().enumerate() {
        if idx > 0 && (total - idx).is_multiple_of(3) {
            buf[pos] = b',';
            pos += 1;
        }
        buf[pos] = b;
        pos += 1;
    }
    buf[pos] = b'.';
    buf[pos + 1] = b'0' + (cents_part / 10) as u8;
    buf[pos + 2] = b'0' + (cents_part % 10) as u8;
    pos += 3;

    let rendered = std::str::from_utf8(&buf[..pos]).unwrap_or("");
    out.write_str(rendered)
}

fn fmt_array_with_depth(
    f: &mut fmt::Formatter<'_>,
    elements: &[Value],
    depth: usize,
) -> fmt::Result {
    f.write_str("{")?;
    for (i, val) in elements.iter().enumerate() {
        if i > 0 {
            f.write_str(",")?;
        }
        match val {
            Value::Null => f.write_str("NULL")?,
            Value::Array(inner) => {
                if depth >= MAX_VALUE_ARRAY_DEPTH {
                    f.write_str("{...}")?;
                } else {
                    fmt_array_with_depth(f, inner, depth + 1)?;
                }
            }
            other => write_pg_array_scalar(f, other)?,
        }
    }
    f.write_str("}")
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("NULL"),
            Self::Int(v) => write!(f, "{v}"),
            Self::BigInt(v) => write!(f, "{v}"),
            Self::Real(v) => write!(f, "{v}"),
            Self::Double(v) => write!(f, "{v}"),
            Self::Numeric(v) => write!(f, "{v}"),
            Self::Money(v) => write_pg_money_into(f, *v),
            Self::Text(v) => f.write_str(v),
            Self::Boolean(v) => write!(f, "{v}"),
            Self::Blob(v) => write!(f, "<{} bytes>", v.len()),
            Self::Timestamp(v) => fmt_pg_timestamp(v, f),
            Self::Date(v) => {
                let (year, month, day) = (v.year(), u8::from(v.month()), v.day());
                write!(f, "{year:04}-{month:02}-{day:02}")
            }
            Self::LargeDate(v) => write!(f, "{v}"),
            Self::Time(v) => fmt_pg_time(v, f),
            Self::TimeTz(time, offset) => fmt_pg_timetz(time, offset, f),
            Self::Interval(v) => write_pg_interval_verbose_into(f, v),
            Self::Uuid(bytes) => fmt_uuid(bytes, f),
            Self::TimestampTz(v) => fmt_pg_timestamptz(v, f),
            Self::Tid(v) => write!(f, "{v}"),
            Self::PgLsn(v) => write!(f, "{v}"),
            Self::Jsonb(v) => fmt_pg_jsonb_value(v, f),
            Self::MacAddr(v) => write!(f, "{v}"),
            Self::MacAddr8(v) => write!(f, "{v}"),
            Self::Vector(v) => {
                f.write_str("[")?;
                for (i, val) in v.values.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{val}")?;
                }
                f.write_str("]")
            }
            Self::Array(elements) => fmt_array_with_depth(f, elements, 1),
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "value/uuid_timestamptz_tests.rs"]
mod uuid_timestamptz_tests;
