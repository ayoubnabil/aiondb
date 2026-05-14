use super::*;

// =====================================================================
// Range functions: lower, upper, isempty, etc.
// =====================================================================

fn arg_looks_like_multirange(args: &[Value]) -> bool {
    matches!(args.first(), Some(Value::Text(s)) if looks_like_multirange(s))
}

pub(crate) fn eval_range_lower(args: &[Value]) -> DbResult<Value> {
    if arg_looks_like_multirange(args) {
        return super::eval_multirange_lower(args);
    }
    let range = extract_range(args)?;
    if range.empty {
        return Ok(Value::Null);
    }
    match &range.lower {
        RangeBound::Unbounded => Ok(Value::Null),
        RangeBound::Inclusive(v) | RangeBound::Exclusive(v) => Ok(range_val_to_value(v)),
    }
}

pub(crate) fn eval_range_upper(args: &[Value]) -> DbResult<Value> {
    if arg_looks_like_multirange(args) {
        return super::eval_multirange_upper(args);
    }
    let range = extract_range(args)?;
    if range.empty {
        return Ok(Value::Null);
    }
    match &range.upper {
        RangeBound::Unbounded => Ok(Value::Null),
        RangeBound::Inclusive(v) | RangeBound::Exclusive(v) => Ok(range_val_to_value(v)),
    }
}

pub(crate) fn eval_range_isempty(args: &[Value]) -> DbResult<Value> {
    if arg_looks_like_multirange(args) {
        if args[0].is_null() {
            return Ok(Value::Null);
        }
        let ranges = super::parse_multirange_value(&args[0])?;
        return Ok(Value::Boolean(ranges.is_empty()));
    }
    let range = extract_range(args)?;
    Ok(Value::Boolean(range.empty))
}

pub(crate) fn eval_range_lower_inc(args: &[Value]) -> DbResult<Value> {
    if arg_looks_like_multirange(args) {
        if args[0].is_null() {
            return Ok(Value::Null);
        }
        let ranges = super::parse_multirange_value(&args[0])?;
        return Ok(Value::Boolean(
            ranges.first().is_some_and(Range::lower_inclusive),
        ));
    }
    let range = extract_range(args)?;
    if range.empty {
        return Ok(Value::Boolean(false));
    }
    Ok(Value::Boolean(range.lower_inclusive()))
}

pub(crate) fn eval_range_upper_inc(args: &[Value]) -> DbResult<Value> {
    if arg_looks_like_multirange(args) {
        if args[0].is_null() {
            return Ok(Value::Null);
        }
        let ranges = super::parse_multirange_value(&args[0])?;
        return Ok(Value::Boolean(
            ranges.last().is_some_and(Range::upper_inclusive),
        ));
    }
    let range = extract_range(args)?;
    if range.empty {
        return Ok(Value::Boolean(false));
    }
    Ok(Value::Boolean(range.upper_inclusive()))
}

pub(crate) fn eval_range_lower_inf(args: &[Value]) -> DbResult<Value> {
    if arg_looks_like_multirange(args) {
        if args[0].is_null() {
            return Ok(Value::Null);
        }
        let ranges = super::parse_multirange_value(&args[0])?;
        return Ok(Value::Boolean(
            ranges.first().is_some_and(Range::lower_unbounded),
        ));
    }
    let range = extract_range(args)?;
    if range.empty {
        return Ok(Value::Boolean(false));
    }
    Ok(Value::Boolean(range.lower_unbounded()))
}

pub(crate) fn eval_range_upper_inf(args: &[Value]) -> DbResult<Value> {
    if arg_looks_like_multirange(args) {
        if args[0].is_null() {
            return Ok(Value::Null);
        }
        let ranges = super::parse_multirange_value(&args[0])?;
        return Ok(Value::Boolean(
            ranges.last().is_some_and(Range::upper_unbounded),
        ));
    }
    let range = extract_range(args)?;
    if range.empty {
        return Ok(Value::Boolean(false));
    }
    Ok(Value::Boolean(range.upper_unbounded()))
}

pub(crate) fn eval_range_merge(args: &[Value]) -> DbResult<Value> {
    if args.len() < 2 {
        return Err(DbError::internal("range_merge requires 2 arguments"));
    }
    if args[0].is_null() || args[1].is_null() {
        return Ok(Value::Null);
    }
    let r1 = text_to_range(&args[0])?;
    let r2 = text_to_range(&args[1])?;
    let merged = r1.merge(&r2);
    Ok(Value::Text(merged.to_pg_text()))
}

// =====================================================================
// Range operators
// =====================================================================

pub(crate) fn eval_range_contains_range(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let r1 = text_to_range(left)?;
    let r2 = text_to_range(right)?;
    Ok(Value::Boolean(r1.contains_range(&r2)))
}

pub(crate) fn eval_range_contains_elem(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let r = text_to_range(left)?;
    let val = value_to_range_val(right, &r.kind)?;
    Ok(Value::Boolean(r.contains_val(&val)))
}

pub(crate) fn eval_range_contained_by_range(left: &Value, right: &Value) -> DbResult<Value> {
    eval_range_contains_range(right, left)
}

#[expect(dead_code, reason = "reserved for future planner/operator wiring")]
pub(crate) fn eval_elem_contained_by_range(left: &Value, right: &Value) -> DbResult<Value> {
    eval_range_contains_elem(right, left)
}

pub(crate) fn eval_range_overlap(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let r1 = text_to_range(left)?;
    let r2 = text_to_range(right)?;
    Ok(Value::Boolean(r1.overlaps(&r2)))
}

#[allow(dead_code)]
pub(crate) fn eval_range_union(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let r1 = text_to_range(left)?;
    let r2 = text_to_range(right)?;
    let result = r1.union(&r2)?;
    Ok(Value::Text(result.to_pg_text()))
}

#[allow(dead_code)]
pub(crate) fn eval_range_intersect(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let r1 = text_to_range(left)?;
    let r2 = text_to_range(right)?;
    let result = r1.intersect(&r2)?;
    Ok(Value::Text(result.to_pg_text()))
}

pub(crate) fn eval_range_difference(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let r1 = text_to_range(left)?;
    let r2 = text_to_range(right)?;
    let result = r1.difference(&r2)?;
    Ok(Value::Text(result.to_pg_text()))
}

#[expect(dead_code, reason = "reserved for future planner/operator wiring")]
pub(crate) fn eval_range_eq(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let r1 = text_to_range(left)?;
    let r2 = text_to_range(right)?;
    Ok(Value::Boolean(r1.cmp_range(&r2) == Some(Ordering::Equal)))
}

#[expect(dead_code, reason = "reserved for future planner/operator wiring")]
pub(crate) fn eval_range_lt(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let r1 = text_to_range(left)?;
    let r2 = text_to_range(right)?;
    Ok(Value::Boolean(r1.cmp_range(&r2) == Some(Ordering::Less)))
}

pub(crate) fn eval_range_adjacent(left: &Value, right: &Value) -> DbResult<Value> {
    super::eval_range_adjacent_generic(left, right)
}

pub(crate) fn eval_range_strictly_left(left: &Value, right: &Value) -> DbResult<Value> {
    super::eval_range_strictly_left_generic(left, right)
}

#[expect(dead_code, reason = "reserved for future planner/operator wiring")]
pub(crate) fn eval_range_strictly_right(left: &Value, right: &Value) -> DbResult<Value> {
    super::eval_range_strictly_right_generic(left, right)
}

#[expect(dead_code, reason = "reserved for future planner/operator wiring")]
pub(crate) fn eval_range_not_extend_right(left: &Value, right: &Value) -> DbResult<Value> {
    super::eval_range_not_extend_right_generic(left, right)
}

// =====================================================================
// Multirange constructors (stub - returns text representation)
// =====================================================================

pub(crate) fn eval_multirange_constructor(kind: RangeKind, args: &[Value]) -> DbResult<Value> {
    fn collect_multirange_arg(
        kind: RangeKind,
        value: &Value,
        out: &mut Vec<Range>,
    ) -> DbResult<()> {
        match value {
            Value::Null => Ok(()),
            Value::Array(items) => {
                for item in items {
                    collect_multirange_arg(kind, item, out)?;
                }
                Ok(())
            }
            Value::Text(text) => {
                let trimmed = text.trim();
                if trimmed.eq_ignore_ascii_case("empty") {
                    return Ok(());
                }
                if looks_like_multirange(trimmed) {
                    for part in split_multirange_items(&trimmed[1..trimmed.len() - 1], trimmed)? {
                        let token = part.trim();
                        if token.eq_ignore_ascii_case("empty") {
                            continue;
                        }
                        let range = parse_range_text(token, &kind)?;
                        if !range.empty {
                            out.push(range);
                        }
                    }
                    return Ok(());
                }
                let range = parse_range_text(trimmed, &kind)?;
                if !range.empty {
                    out.push(range);
                }
                Ok(())
            }
            _ => Err(DbError::internal(
                "multirange constructor: expected range text arguments",
            )),
        }
    }

    if args.is_empty() {
        return Ok(Value::Text("{}".to_string()));
    }
    let mut ranges: Vec<Range> = Vec::new();
    for arg in args {
        collect_multirange_arg(kind, arg, &mut ranges)?;
    }
    if ranges.is_empty() {
        return Ok(Value::Text("{}".to_string()));
    }
    ranges.sort_by(|a, b| cmp_lower_bound(&a.lower, &b.lower).unwrap_or(Ordering::Equal));
    let mut merged: Vec<Range> = vec![ranges[0].clone()];
    for r in &ranges[1..] {
        let Some(last) = merged.last() else {
            return Err(DbError::internal(
                "multirange constructor: merged range state is empty",
            ));
        };
        if last.overlaps(r) || last.is_adjacent(r) {
            let u = last.union(r).unwrap_or_else(|_| last.merge(r));
            let Some(last_mut) = merged.last_mut() else {
                return Err(DbError::internal(
                    "multirange constructor: merged range state is empty",
                ));
            };
            *last_mut = u;
        } else {
            merged.push(r.clone());
        }
    }
    let mut result = String::from("{");
    for (i, r) in merged.iter().enumerate() {
        if i > 0 {
            result.push(',');
        }
        result.push_str(&r.to_pg_text());
    }
    result.push('}');
    Ok(Value::Text(result))
}

// =====================================================================
// Helper: extract Range from a Value (must be Text)
// =====================================================================

fn extract_range(args: &[Value]) -> DbResult<Range> {
    if args.is_empty() {
        return Err(DbError::internal("range function requires 1 argument"));
    }
    if args[0].is_null() {
        return Ok(Range::empty(RangeKind::Numeric));
    }
    text_to_range(&args[0])
}

/// Determine the range kind from the text representation and parse.
pub(crate) fn text_to_range(v: &Value) -> DbResult<Range> {
    match v {
        Value::Text(s) => guess_and_parse_range(s),
        _ => Err(DbError::internal("expected range text value")),
    }
}

/// Guess the range kind from text and parse it.
pub(crate) fn guess_and_parse_range(s: &str) -> DbResult<Range> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("empty") {
        return Ok(Range::empty(RangeKind::Numeric));
    }
    if s.len() < 3 {
        return Err(DbError::from_report(aiondb_core::ErrorReport::new(
            aiondb_core::SqlState::InvalidTextRepresentation,
            format!("malformed range literal: \"{s}\""),
        )));
    }
    let inner = &s[1..s.len() - 1];
    let comma_pos = find_comma(inner, s)?;
    let lower_str = inner[..comma_pos].trim();
    let upper_str = inner[comma_pos + 1..].trim();
    let sample = if !lower_str.is_empty() {
        lower_str
    } else if !upper_str.is_empty() {
        upper_str
    } else {
        return parse_range_text(s, &RangeKind::Numeric);
    };
    let kind = if sample.contains('-') && sample.len() >= 8 {
        if sample.contains(':') {
            RangeKind::Timestamp
        } else {
            RangeKind::Date
        }
    } else if (sample.starts_with('(') && sample.ends_with(')'))
        || sample
            .chars()
            .any(|ch| ch.is_ascii_alphabetic() || ch == '"' || ch == '\\')
    {
        RangeKind::Text
    } else if sample.contains('.') || sample.parse::<i64>().is_err() {
        RangeKind::Numeric
    } else {
        let v: i64 = sample.parse().unwrap_or(0);
        if i32::try_from(v).is_ok() {
            RangeKind::Int4
        } else {
            RangeKind::Int8
        }
    };
    parse_range_text(s, &kind)
}

fn range_val_to_value(v: &RangeVal) -> Value {
    match v {
        RangeVal::Int(n) => Value::Int(*n),
        RangeVal::BigInt(n) => Value::BigInt(*n),
        RangeVal::Numeric(n) => Value::Numeric(n.clone()),
        RangeVal::Text(text) => Value::Text(text.clone()),
        RangeVal::Date(d) => Value::Date(*d),
        RangeVal::Timestamp(ts) => Value::Timestamp(*ts),
        RangeVal::TimestampTz(ts) => Value::TimestampTz(*ts),
    }
}
