//! Value comparison and ordering logic.

use std::cmp::Ordering;

use crate::eval::scalar_functions::geometric::parse_circle_text;
use crate::eval::scalar_functions::range::{compare_multirange_text, looks_like_multirange};
use crate::eval::scalar_functions::value_convert::i64_to_f64;
use aiondb_core::{
    DbError, DbResult, MacAddr, MacAddr8, NumericValue, PgLsnValue, TidValue, Value,
};

use super::*;

pub(crate) fn as_nullable_bool(value: &Value) -> DbResult<Option<bool>> {
    match value {
        Value::Null => Ok(None),
        Value::Boolean(value) => Ok(Some(*value)),
        _ => Err(DbError::internal(
            "logical expression did not evaluate to BOOLEAN",
        )),
    }
}

const MAX_VALUE_COMPARE_DEPTH: usize = 256;

pub(crate) fn compare_values(left: &Value, right: &Value) -> DbResult<Option<Ordering>> {
    compare_values_at_depth(left, right, 0)
}

fn compare_values_at_depth(
    left: &Value,
    right: &Value,
    depth: usize,
) -> DbResult<Option<Ordering>> {
    if depth >= MAX_VALUE_COMPARE_DEPTH {
        return Err(DbError::program_limit(format!(
            "value comparison nesting depth exceeds limit {MAX_VALUE_COMPARE_DEPTH}"
        )));
    }
    fn comparison_type_rank(value: &Value) -> u8 {
        match value {
            Value::Null => 0,
            Value::Boolean(_) => 1,
            Value::Int(_) => 2,
            Value::BigInt(_) => 3,
            Value::Real(_) => 4,
            Value::Double(_) => 5,
            Value::Numeric(_) | Value::Money(_) => 6,
            Value::Text(_) => 7,
            Value::Blob(_) => 8,
            Value::Timestamp(_) => 9,
            Value::Date(_) | Value::LargeDate(_) => 10,
            Value::Time(_) => 11,
            Value::TimeTz(_, _) => 12,
            Value::Interval(_) => 13,
            Value::Uuid(_) => 14,
            Value::TimestampTz(_) => 15,
            Value::Tid(_) => 16,
            Value::PgLsn(_) => 17,
            Value::Jsonb(_) => 18,
            Value::MacAddr(_) => 19,
            Value::MacAddr8(_) => 20,
            Value::Vector(_) => 21,
            Value::Array(_) => 22,
        }
    }

    fn compare_array_elements(left: &Value, right: &Value, depth: usize) -> DbResult<Ordering> {
        match (left, right) {
            (Value::Null, Value::Null) => Ok(Ordering::Equal),
            (Value::Null, _) => Ok(Ordering::Greater),
            (_, Value::Null) => Ok(Ordering::Less),
            _ => compare_values_at_depth(left, right, depth)?.ok_or_else(|| {
                DbError::internal("array element comparison unexpectedly returned NULL")
            }),
        }
    }

    match (left, right) {
        (Value::Null, _) | (_, Value::Null) => Ok(None),
        (left, right) if is_numeric_value(left) && is_numeric_value(right) => {
            Ok(Some(compare_numeric_values(left, right)?))
        }
        (Value::Money(left), Value::Money(right)) => Ok(Some(left.cmp(right))),
        (Value::Money(left), Value::Text(text)) => Ok(Some(left.cmp(&parse_money_text(text)?))),
        (Value::Text(text), Value::Money(right)) => Ok(Some(parse_money_text(text)?.cmp(right))),
        (Value::Int(left), Value::Int(right)) => Ok(Some(left.cmp(right))),
        (Value::BigInt(left), Value::BigInt(right)) => Ok(Some(left.cmp(right))),
        // PostgreSQL sorts NaN as greater than all non-NaN values and
        // treats NaN = NaN.  f32/f64::total_cmp provides exactly this
        // ordering (NaN > Infinity > finite > -Infinity > -NaN).
        (Value::Real(left), Value::Real(right)) => Ok(Some(left.total_cmp(right))),
        (Value::Double(left), Value::Double(right)) => Ok(Some(left.total_cmp(right))),
        (Value::Numeric(left), Value::Numeric(right)) => Ok(Some(compare_numeric(left, right))),
        (Value::Text(left), Value::Text(right)) => {
            // Fast-reject for plain text: multirange/circle/point all
            // require structural punctuation (`{`, `<`, `(`) at the
            // start. If neither side starts with one, skip the three
            // expensive parse attempts that allocate Strings inside
            // their parsers (circle, fixed-point normalize).
            //
            // This runs per-row in ORDER BY/JOIN/WHERE on Text columns,
            // so eliminating the parser work for the dominant
            // plain-text case is a real per-row win.
            let needs_geom_parse = matches!(left.as_bytes().first(), Some(b'{' | b'<' | b'('))
                || matches!(right.as_bytes().first(), Some(b'{' | b'<' | b'('));
            if needs_geom_parse {
                if looks_like_multirange(left) && looks_like_multirange(right) {
                    if let Ok(Some(ord)) = compare_multirange_text(left, right) {
                        return Ok(Some(ord));
                    }
                }
                if let (Ok(left_circle), Ok(right_circle)) =
                    (parse_circle_text(left), parse_circle_text(right))
                {
                    let radius_cmp = left_circle.radius.total_cmp(&right_circle.radius);
                    if radius_cmp != Ordering::Equal {
                        return Ok(Some(radius_cmp));
                    }
                    let x_cmp = left_circle.center.x.total_cmp(&right_circle.center.x);
                    if x_cmp != Ordering::Equal {
                        return Ok(Some(x_cmp));
                    }
                    return Ok(Some(left_circle.center.y.total_cmp(&right_circle.center.y)));
                }
                if let (Some(left), Some(right)) = (
                    normalize_fixed_point_text(left),
                    normalize_fixed_point_text(right),
                ) {
                    return Ok(Some(left.cmp(&right)));
                }
            }
            Ok(Some(left.cmp(right)))
        }
        (Value::Boolean(left), Value::Boolean(right)) => Ok(Some(left.cmp(right))),
        (Value::Blob(left), Value::Blob(right)) => Ok(Some(left.cmp(right))),
        (Value::Timestamp(left), Value::Timestamp(right)) => Ok(Some(left.cmp(right))),
        (Value::Date(left), Value::Date(right)) => Ok(Some(left.cmp(right))),
        (Value::LargeDate(left), Value::LargeDate(right)) => Ok(Some(left.cmp(right))),
        (Value::Date(left), Value::LargeDate(right)) => {
            Ok(Some(aiondb_core::PgDate::from(*left).cmp(right)))
        }
        (Value::LargeDate(left), Value::Date(right)) => {
            Ok(Some(left.cmp(&aiondb_core::PgDate::from(*right))))
        }
        (Value::Time(left), Value::Time(right)) => Ok(Some(left.cmp(right))),
        (Value::TimeTz(left_time, left_offset), Value::TimeTz(right_time, right_offset)) => {
            Ok(Some(
                timetz_order_key(left_time, left_offset)
                    .cmp(&timetz_order_key(right_time, right_offset)),
            ))
        }
        (Value::Interval(left), Value::Interval(right)) => Ok(Some(
            interval_comparison_key(left).cmp(&interval_comparison_key(right)),
        )),
        (Value::Tid(left), Value::Tid(right)) => Ok(Some(left.cmp(right))),
        (Value::PgLsn(left), Value::PgLsn(right)) => Ok(Some(left.cmp(right))),
        (Value::MacAddr(left), Value::MacAddr(right)) => {
            Ok(Some(left.as_bytes().cmp(right.as_bytes())))
        }
        (Value::MacAddr8(left), Value::MacAddr8(right)) => {
            Ok(Some(left.as_bytes().cmp(right.as_bytes())))
        }
        (Value::Uuid(left), Value::Uuid(right)) => Ok(Some(left.cmp(right))),
        (Value::TimestampTz(left), Value::TimestampTz(right)) => Ok(Some(left.cmp(right))),
        (Value::Jsonb(left), Value::Jsonb(right)) => Ok(Some(compare_jsonb_values(left, right)?)),
        // TimestampTz vs Timestamp cross-comparison (promote Timestamp using
        // the current session timezone, matching PostgreSQL semantics).
        (Value::TimestampTz(left), Value::Timestamp(right)) => {
            Ok(Some(left.cmp(&promote_timestamp_to_timestamptz(*right))))
        }
        (Value::Timestamp(left), Value::TimestampTz(right)) => {
            Ok(Some(promote_timestamp_to_timestamptz(*left).cmp(right)))
        }
        // Date vs Timestamp/TimestampTz cross-comparison
        (Value::Date(d), Value::Timestamp(ts)) => {
            let left_ts = time::PrimitiveDateTime::new(*d, time::Time::MIDNIGHT);
            Ok(Some(left_ts.cmp(ts)))
        }
        (Value::Timestamp(ts), Value::Date(d)) => {
            let right_ts = time::PrimitiveDateTime::new(*d, time::Time::MIDNIGHT);
            Ok(Some(ts.cmp(&right_ts)))
        }
        (Value::Date(d), Value::TimestampTz(odt)) => {
            let left_odt = promote_timestamp_to_timestamptz(time::PrimitiveDateTime::new(
                *d,
                time::Time::MIDNIGHT,
            ));
            Ok(Some(left_odt.cmp(odt)))
        }
        (Value::TimestampTz(odt), Value::Date(d)) => {
            let right_odt = promote_timestamp_to_timestamptz(time::PrimitiveDateTime::new(
                *d,
                time::Time::MIDNIGHT,
            ));
            Ok(Some(odt.cmp(&right_odt)))
        }
        (Value::LargeDate(_), Value::Timestamp(_) | Value::TimestampTz(_)) => {
            Ok(Some(Ordering::Greater))
        }
        (Value::Timestamp(_) | Value::TimestampTz(_), Value::LargeDate(_)) => {
            Ok(Some(Ordering::Less))
        }
        // Boolean vs numeric (PG: TRUE=1, FALSE=0)
        (Value::Boolean(b), right_val) if is_numeric_value(right_val) => {
            let left_num = Value::Int(i32::from(*b));
            compare_values_at_depth(&left_num, right_val, depth + 1)
        }
        (left_val, Value::Boolean(b)) if is_numeric_value(left_val) => {
            let right_num = Value::Int(i32::from(*b));
            compare_values_at_depth(left_val, &right_num, depth + 1)
        }
        // Text vs numeric: coerce text to numeric for comparison
        (Value::Text(s), right_val) if is_numeric_value(right_val) => {
            if let Ok(coerced) = coerce_text_to_numeric(s) {
                compare_values_at_depth(&coerced, right_val, depth + 1)
            } else {
                Ok(Some(s.cmp(&right_val.to_string())))
            }
        }
        (Value::Text(text), Value::PgLsn(right)) => PgLsnValue::parse(text)
            .map(|left| Some(left.cmp(right)))
            .ok_or_else(|| DbError::invalid_input_syntax("pg_lsn", text)),
        (Value::PgLsn(left), Value::Text(text)) => PgLsnValue::parse(text)
            .map(|right| Some(left.cmp(&right)))
            .ok_or_else(|| DbError::invalid_input_syntax("pg_lsn", text)),
        (Value::Text(text), Value::Tid(right)) => TidValue::parse(text)
            .map(|left| Some(left.cmp(right)))
            .ok_or_else(|| DbError::invalid_input_syntax("tid", text)),
        (Value::Tid(left), Value::Text(text)) => TidValue::parse(text)
            .map(|right| Some(left.cmp(&right)))
            .ok_or_else(|| DbError::invalid_input_syntax("tid", text)),
        (Value::Text(text), Value::MacAddr(right)) => MacAddr::parse(text)
            .map(|left| Some(left.as_bytes().cmp(right.as_bytes())))
            .ok_or_else(|| DbError::invalid_input_syntax("macaddr", text)),
        (Value::MacAddr(left), Value::Text(text)) => MacAddr::parse(text)
            .map(|right| Some(left.as_bytes().cmp(right.as_bytes())))
            .ok_or_else(|| DbError::invalid_input_syntax("macaddr", text)),
        (Value::Text(text), Value::MacAddr8(right)) => MacAddr8::parse(text)
            .map(|left| Some(left.as_bytes().cmp(right.as_bytes())))
            .ok_or_else(|| DbError::invalid_input_syntax("macaddr8", text)),
        (Value::MacAddr8(left), Value::Text(text)) => MacAddr8::parse(text)
            .map(|right| Some(left.as_bytes().cmp(right.as_bytes())))
            .ok_or_else(|| DbError::invalid_input_syntax("macaddr8", text)),
        (left_val, Value::Text(s)) if is_numeric_value(left_val) => {
            if let Ok(coerced) = coerce_text_to_numeric(s) {
                compare_values_at_depth(left_val, &coerced, depth + 1)
            } else {
                Ok(Some(left_val.to_string().cmp(s)))
            }
        }
        // Avoid string-render-based ordering for complex values. Rendering can
        // differ independently from semantic ordering and break total-order
        // invariants expected by sorting/index code paths.
        (Value::Array(_) | Value::Jsonb(_), other)
            if !matches!(other, Value::Array(_) | Value::Jsonb(_)) =>
        {
            Ok(Some(
                comparison_type_rank(left).cmp(&comparison_type_rank(right)),
            ))
        }
        (other, Value::Array(_) | Value::Jsonb(_))
            if !matches!(other, Value::Array(_) | Value::Jsonb(_)) =>
        {
            Ok(Some(
                comparison_type_rank(left).cmp(&comparison_type_rank(right)),
            ))
        }
        // Text vs non-numeric: compare as string representations
        (Value::Text(a), other) => Ok(Some(a.cmp(&other.to_string()))),
        (other, Value::Text(b)) => Ok(Some(other.to_string().cmp(b))),
        // Array comparison: element-wise lexicographic ordering
        (Value::Array(a), Value::Array(b)) => {
            for (ea, eb) in a.iter().zip(b.iter()) {
                match compare_array_elements(ea, eb, depth + 1)? {
                    Ordering::Equal => {}
                    ordering => return Ok(Some(ordering)),
                }
            }
            Ok(Some(a.len().cmp(&b.len())))
        }
        // Mixed Array/Jsonb ordering falls back to a stable type-rank order.
        (Value::Array(_) | Value::Jsonb(_), _) | (_, Value::Array(_) | Value::Jsonb(_)) => Ok(
            Some(comparison_type_rank(left).cmp(&comparison_type_rank(right))),
        ),
        _ => Err(DbError::internal(format!(
            "cannot compare {:?} and {:?}",
            left.data_type(),
            right.data_type()
        ))),
    }
}

pub fn compare_runtime_values(left: &Value, right: &Value) -> DbResult<Option<Ordering>> {
    compare_values(left, right)
}

fn normalize_fixed_point_text(text: &str) -> Option<String> {
    // Fast rejection: most text values are not point-like.
    if !text.contains(',') {
        return None;
    }
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .unwrap_or(trimmed);
    let (x, y) = inner.split_once(',')?;
    Some(format!(
        "({},{})",
        format_fixed_point_coordinate(parse_fixed_point_coordinate(x)?),
        format_fixed_point_coordinate(parse_fixed_point_coordinate(y)?)
    ))
}

fn parse_fixed_point_coordinate(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    // Avoid the `to_ascii_lowercase()` String alloc by matching via
    // `eq_ignore_ascii_case` against the small set of recognised
    // sentinel tokens. Falls through to numeric parse otherwise.
    if trimmed.eq_ignore_ascii_case("inf")
        || trimmed.eq_ignore_ascii_case("+inf")
        || trimmed.eq_ignore_ascii_case("infinity")
        || trimmed.eq_ignore_ascii_case("+infinity")
    {
        return Some(f64::INFINITY);
    }
    if trimmed.eq_ignore_ascii_case("-inf") || trimmed.eq_ignore_ascii_case("-infinity") {
        return Some(f64::NEG_INFINITY);
    }
    if trimmed.eq_ignore_ascii_case("nan") {
        return Some(f64::NAN);
    }
    trimmed.parse::<f64>().ok()
}

fn format_fixed_point_coordinate(value: f64) -> String {
    let value = if value == 0.0 { 0.0 } else { value };
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == f64::INFINITY {
        return "Infinity".to_owned();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".to_owned();
    }

    let mut text = value.to_string();
    if !text.contains('e') && !text.contains('E') {
        // Use trim_end_matches instead of repeated pop() loop
        let trimmed = text.trim_end_matches('0').trim_end_matches('.');
        if trimmed.len() < text.len() {
            text.truncate(trimmed.len());
        }
    }
    text
}

pub(crate) fn compare_numeric(left: &NumericValue, right: &NumericValue) -> Ordering {
    // Delegate to NumericValue's Ord impl, which handles NaN and Infinity
    left.cmp(right)
}

fn is_numeric_value(value: &Value) -> bool {
    matches!(
        value,
        Value::Int(_) | Value::BigInt(_) | Value::Real(_) | Value::Double(_) | Value::Numeric(_)
    )
}

fn compare_numeric_values(left: &Value, right: &Value) -> DbResult<Ordering> {
    if matches!(left, Value::Real(_) | Value::Double(_))
        || matches!(right, Value::Real(_) | Value::Double(_))
    {
        let left = numeric_value_to_f64(left)?;
        let right = numeric_value_to_f64(right)?;
        // For NaN: PG treats NaN as equal to NaN and greater than everything
        let ord = match (left.is_nan(), right.is_nan()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
        };
        return Ok(ord);
    }

    let left = numeric_value_to_exact(left)?;
    let right = numeric_value_to_exact(right)?;
    Ok(compare_numeric(&left, &right))
}

fn numeric_value_to_exact(value: &Value) -> DbResult<NumericValue> {
    match value {
        Value::Int(value) => Ok(NumericValue::new(i128::from(*value), 0)),
        Value::BigInt(value) => Ok(NumericValue::new(i128::from(*value), 0)),
        Value::Numeric(value) => Ok(value.clone()),
        _ => Err(DbError::internal(format!(
            "cannot compare {:?} as an exact numeric value",
            value.data_type()
        ))),
    }
}

fn numeric_value_to_f64(value: &Value) -> DbResult<f64> {
    match value {
        Value::Int(value) => Ok(f64::from(*value)),
        Value::BigInt(value) => Ok(i64_to_f64(*value)),
        Value::Real(value) => Ok(f64::from(*value)),
        Value::Double(value) => Ok(*value),
        Value::Numeric(value) => Ok(crate::eval::cast::numeric::numeric_to_f64(value)),
        _ => Err(DbError::internal(format!(
            "cannot convert {:?} to f64",
            value.data_type()
        ))),
    }
}

pub(crate) fn numeric_to_f64(value: &NumericValue) -> f64 {
    crate::eval::cast::numeric::numeric_to_f64(value)
}

pub(crate) fn interval_factor(value: &Value, operation: &str) -> DbResult<f64> {
    match value {
        Value::Int(value) => Ok(f64::from(*value)),
        Value::BigInt(value) => Ok(i64_to_f64(*value)),
        Value::Double(value) => Ok(*value),
        Value::Real(value) => Ok(f64::from(*value)),
        Value::Numeric(value) => Ok(crate::eval::cast::numeric::numeric_to_f64(value)),
        _ => Err(DbError::internal(format!(
            "cannot {operation} INTERVAL by {:?}",
            value.data_type()
        ))),
    }
}
