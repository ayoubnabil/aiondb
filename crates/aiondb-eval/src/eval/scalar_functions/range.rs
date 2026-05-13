#![allow(dead_code, clippy::doc_markdown, clippy::many_single_char_names)]

// Range type implementation for PostgreSQL-compatible range constructors,
// operators, and functions.
//
// Ranges are stored as Value::Text in canonical PostgreSQL format:
//   "[lower,upper)"  / "empty"  / "(,)"  etc.
// This avoids adding a new Value variant and keeps the representation
// compatible with the pgwire text protocol.

use aiondb_core::temporal::{format_date, format_timestamp, format_timestamptz};
use aiondb_core::{DbError, DbResult, ErrorReport, NumericValue, SqlState, Value};
use std::cmp::Ordering;

use super::value_convert::{f64_to_i32, f64_to_i64};
#[path = "range_eval_support.rs"]
mod range_eval_support;
use self::range_eval_support::guess_and_parse_range;
pub(crate) use self::range_eval_support::{
    eval_multirange_constructor, eval_range_adjacent, eval_range_contained_by_range,
    eval_range_contains_elem, eval_range_contains_range, eval_range_difference, eval_range_isempty,
    eval_range_lower, eval_range_lower_inc, eval_range_lower_inf, eval_range_merge,
    eval_range_overlap, eval_range_upper, eval_range_upper_inc, eval_range_upper_inf,
    text_to_range,
};

/// Check if a text value looks like a PostgreSQL range literal.
pub(crate) fn looks_like_range(s: &str) -> bool {
    let s = s.trim();
    if s.eq_ignore_ascii_case("empty") {
        return true;
    }
    if s.len() < 3 {
        return false;
    }
    let first = s.as_bytes()[0];
    let last = s.as_bytes()[s.len() - 1];
    (first == b'[' || first == b'(') && (last == b']' || last == b')')
}

/// Check if a text value looks like a PostgreSQL multirange literal.
pub(crate) fn looks_like_multirange(s: &str) -> bool {
    let s = s.trim();
    s.starts_with('{') && s.ends_with('}')
}

fn malformed_range_literal(literal: &str) -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::InvalidTextRepresentation,
        format!("malformed range literal: \"{}\"", literal.trim()),
    ))
}

fn quote_bound_if_needed(text: &str) -> String {
    // PostgreSQL `range_bound_escape`: an empty string is rendered as `""` so
    // it is distinguishable from the unbounded form. Inside quotes, `"` and
    // `\` are doubled (PG uses doubling, not backslash escaping).
    let requires_quotes = text.is_empty()
        || text.chars().any(|ch| {
            ch == ','
                || ch == '['
                || ch == ']'
                || ch == '('
                || ch == ')'
                || ch == '"'
                || ch == '\\'
                || ch.is_ascii_whitespace()
        });
    if !requires_quotes {
        return text.to_owned();
    }
    let mut escaped = String::with_capacity(text.len() + 2);
    escaped.push('"');
    for ch in text.chars() {
        if ch == '"' || ch == '\\' {
            escaped.push(ch);
        }
        escaped.push(ch);
    }
    escaped.push('"');
    escaped
}

/// Decode a PG range bound string into its raw value.  Implements the PG
/// `range_parse_bound` semantics: outside quotes `\X` consumes the next char;
/// inside quotes `""` is a literal `"` and `\X` consumes the next char; quotes
/// can open and close repeatedly within a single bound and the segments are
/// concatenated verbatim. No whitespace trimming.
fn decode_text_bound(s: &str) -> DbResult<(String, bool)> {
    let mut out = String::with_capacity(s.len());
    let mut was_quoted = false;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                was_quoted = true;
                loop {
                    let qc = match chars.next() {
                        Some(c) => c,
                        None => {
                            return Err(DbError::from_report(ErrorReport::new(
                                SqlState::InvalidTextRepresentation,
                                "malformed range literal: unterminated quoted bound".to_string(),
                            )));
                        }
                    };
                    match qc {
                        '"' => {
                            if chars.peek() == Some(&'"') {
                                out.push('"');
                                chars.next();
                            } else {
                                break;
                            }
                        }
                        '\\' => {
                            let nc = chars.next().ok_or_else(|| {
                                DbError::from_report(ErrorReport::new(
                                    SqlState::InvalidTextRepresentation,
                                    "malformed range literal: trailing backslash".to_string(),
                                ))
                            })?;
                            out.push(nc);
                        }
                        _ => out.push(qc),
                    }
                }
            }
            '\\' => {
                was_quoted = true;
                let nc = chars.next().ok_or_else(|| {
                    DbError::from_report(ErrorReport::new(
                        SqlState::InvalidTextRepresentation,
                        "malformed range literal: trailing backslash".to_string(),
                    ))
                })?;
                out.push(nc);
            }
            _ => out.push(ch),
        }
    }
    Ok((out, was_quoted))
}

fn split_multirange_items(inner: &str, full_literal: &str) -> DbResult<Vec<String>> {
    let mut items = Vec::new();
    let mut idx = 0usize;
    let bytes = inner.as_bytes();
    let mut expect_item = false;

    while idx < inner.len() {
        while idx < inner.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= inner.len() {
            if expect_item {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::InvalidTextRepresentation,
                    format!("malformed multirange literal: \"{}\"", full_literal.trim()),
                )));
            }
            break;
        }
        expect_item = false;

        let item_start = idx;
        let ch = bytes[idx] as char;
        if ch != '[' && ch != '(' {
            // Let the caller report malformed item content with the same
            // top-level multirange error shape.
            let next_comma = inner[idx..]
                .find(',')
                .map(|off| idx + off)
                .unwrap_or(inner.len());
            items.push(inner[item_start..next_comma].to_owned());
            idx = if next_comma < inner.len() {
                expect_item = true;
                next_comma + 1
            } else {
                next_comma
            };
            continue;
        }

        idx += 1;
        let mut in_quotes = false;
        let mut escape = false;
        let mut found_end = false;

        while idx < inner.len() {
            let ch = bytes[idx] as char;
            if escape {
                escape = false;
                idx += 1;
                continue;
            }
            match ch {
                '\\' => {
                    escape = true;
                    idx += 1;
                }
                '"' => {
                    in_quotes = !in_quotes;
                    idx += 1;
                }
                ']' | ')' if !in_quotes => {
                    let range_end = idx + 1;
                    let mut lookahead = range_end;
                    while lookahead < inner.len() && bytes[lookahead].is_ascii_whitespace() {
                        lookahead += 1;
                    }
                    if lookahead == inner.len() || bytes[lookahead] == b',' {
                        items.push(inner[item_start..range_end].to_owned());
                        idx = if lookahead < inner.len() {
                            expect_item = true;
                            lookahead + 1
                        } else {
                            lookahead
                        };
                        found_end = true;
                        break;
                    }
                    idx += 1;
                }
                _ => idx += 1,
            }
        }

        if !found_end {
            return Err(DbError::from_report(ErrorReport::new(
                SqlState::InvalidTextRepresentation,
                format!("malformed multirange literal: \"{}\"", full_literal.trim()),
            )));
        }
    }
    if expect_item {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidTextRepresentation,
            format!("malformed multirange literal: \"{}\"", full_literal.trim()),
        )));
    }

    Ok(items)
}

fn canonicalize_multirange_ranges(mut ranges: Vec<Range>) -> Vec<Range> {
    ranges.retain(|range| !range.empty);
    if ranges.is_empty() {
        return ranges;
    }
    ranges.sort_by(|a, b| {
        cmp_lower_bound(&a.lower, &b.lower)
            .or_else(|| cmp_upper_bound(&a.upper, &b.upper))
            .unwrap_or(Ordering::Equal)
    });
    let mut merged: Vec<Range> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(last) = merged.last_mut() {
            if last.overlaps(&range) || last.is_adjacent(&range) {
                let merged_range = last.union(&range).unwrap_or_else(|_| last.merge(&range));
                *last = merged_range;
                continue;
            }
        }
        merged.push(range);
    }
    merged
}

fn parse_multirange_text(s: &str) -> DbResult<Vec<Range>> {
    let trimmed = s.trim();
    if !looks_like_multirange(trimmed) {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidTextRepresentation,
            format!("malformed multirange literal: \"{}\"", trimmed),
        )));
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut ranges = Vec::new();
    for item in split_multirange_items(inner, trimmed)? {
        let token = item.trim();
        if token.eq_ignore_ascii_case("empty") {
            continue;
        }
        if !looks_like_range(token) {
            return Err(DbError::from_report(ErrorReport::new(
                SqlState::InvalidTextRepresentation,
                format!("malformed multirange literal: \"{}\"", trimmed),
            )));
        }
        let parsed = guess_and_parse_range(token)?;
        if !parsed.empty {
            ranges.push(parsed);
        }
    }
    Ok(canonicalize_multirange_ranges(ranges))
}

pub(crate) fn parse_multirange_value(v: &Value) -> DbResult<Vec<Range>> {
    match v {
        Value::Text(text) => {
            let trimmed = text.trim();
            if looks_like_multirange(trimmed) {
                parse_multirange_text(trimmed)
            } else if looks_like_range(trimmed) || trimmed.eq_ignore_ascii_case("empty") {
                let parsed = guess_and_parse_range(trimmed)?;
                if parsed.empty {
                    Ok(Vec::new())
                } else {
                    Ok(vec![parsed])
                }
            } else {
                Err(DbError::from_report(ErrorReport::new(
                    SqlState::InvalidTextRepresentation,
                    format!("malformed multirange literal: \"{}\"", trimmed),
                )))
            }
        }
        _ => {
            let rendered = v.to_string();
            parse_multirange_value(&Value::Text(rendered))
        }
    }
}

pub(crate) fn eval_multirange_contains_elem(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    if let Value::Text(text) = right {
        let trimmed = text.trim();
        if looks_like_multirange(trimmed) {
            return eval_multirange_contains_multirange(left, right);
        }
        if looks_like_range(trimmed) || trimmed.eq_ignore_ascii_case("empty") {
            return eval_multirange_contains_range(left, right);
        }
    }
    let ranges = parse_multirange_value(left)?;
    if ranges.is_empty() {
        return Ok(Value::Boolean(false));
    }
    let Some(kind) = ranges.first().map(|range| range.kind) else {
        return Ok(Value::Boolean(false));
    };
    let elem = value_to_range_val(right, &kind)?;
    Ok(Value::Boolean(
        ranges.iter().any(|range| range.contains_val(&elem)),
    ))
}

pub(crate) fn eval_multirange_contains_range(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let ranges = parse_multirange_value(left)?;
    let rhs = text_to_range(right)?;
    if rhs.empty {
        return Ok(Value::Boolean(true));
    }
    Ok(Value::Boolean(
        ranges.iter().any(|range| range.contains_range(&rhs)),
    ))
}

pub(crate) fn eval_multirange_contains_multirange(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let lhs_ranges = parse_multirange_value(left)?;
    let rhs_ranges = parse_multirange_value(right)?;
    Ok(Value::Boolean(rhs_ranges.iter().all(|rhs| {
        lhs_ranges.iter().any(|lhs| lhs.contains_range(rhs))
    })))
}

pub(crate) fn eval_range_contains_multirange(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let lhs = text_to_range(left)?;
    let rhs_ranges = parse_multirange_value(right)?;
    Ok(Value::Boolean(
        rhs_ranges.iter().all(|rhs| lhs.contains_range(rhs)),
    ))
}

pub(crate) fn eval_range_contained_by_multirange(left: &Value, right: &Value) -> DbResult<Value> {
    eval_multirange_contains_range(right, left)
}

pub(crate) fn eval_elem_contained_by_multirange(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    if let Value::Text(text) = left {
        let trimmed = text.trim();
        if looks_like_multirange(trimmed) {
            return eval_multirange_contained_by_multirange(left, right);
        }
        if looks_like_range(trimmed) || trimmed.eq_ignore_ascii_case("empty") {
            return eval_range_contained_by_multirange(left, right);
        }
    }
    eval_multirange_contains_elem(right, left)
}

pub(crate) fn eval_multirange_contained_by_multirange(
    left: &Value,
    right: &Value,
) -> DbResult<Value> {
    eval_multirange_contains_multirange(right, left)
}

pub(crate) fn eval_multirange_overlaps_range(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let lhs_ranges = parse_multirange_value(left)?;
    let rhs = text_to_range(right)?;
    Ok(Value::Boolean(
        lhs_ranges.iter().any(|lhs| lhs.overlaps(&rhs)),
    ))
}

pub(crate) fn eval_range_overlaps_multirange(left: &Value, right: &Value) -> DbResult<Value> {
    eval_multirange_overlaps_range(right, left)
}

pub(crate) fn eval_multirange_overlaps_multirange(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let lhs_ranges = parse_multirange_value(left)?;
    let rhs_ranges = parse_multirange_value(right)?;
    Ok(Value::Boolean(
        lhs_ranges
            .iter()
            .any(|lhs| rhs_ranges.iter().any(|rhs| lhs.overlaps(rhs))),
    ))
}

fn operand_to_ranges(value: &Value) -> DbResult<Vec<Range>> {
    if let Value::Text(text) = value {
        let trimmed = text.trim();
        if looks_like_multirange(trimmed) {
            return parse_multirange_value(value);
        }
        let range = text_to_range(value)?;
        if range.empty {
            return Ok(Vec::new());
        }
        return Ok(vec![range]);
    }
    let rendered = Value::Text(value.to_string());
    operand_to_ranges(&rendered)
}

fn operand_extents(value: &Value) -> DbResult<Option<(RangeBound, RangeBound)>> {
    let ranges = operand_to_ranges(value)?;
    let (Some(first), Some(last)) = (ranges.first(), ranges.last()) else {
        return Ok(None);
    };
    Ok(Some((first.lower.clone(), last.upper.clone())))
}

fn upper_strictly_left_of_lower(lhs_upper: &RangeBound, rhs_lower: &RangeBound) -> bool {
    match (lhs_upper, rhs_lower) {
        (RangeBound::Unbounded, _) | (_, RangeBound::Unbounded) => false,
        (RangeBound::Inclusive(lhs), RangeBound::Inclusive(rhs)) => {
            lhs.cmp_val(rhs) == Some(Ordering::Less)
        }
        (RangeBound::Inclusive(lhs) | RangeBound::Exclusive(lhs), RangeBound::Exclusive(rhs))
        | (RangeBound::Exclusive(lhs), RangeBound::Inclusive(rhs)) => {
            matches!(lhs.cmp_val(rhs), Some(Ordering::Less | Ordering::Equal))
        }
    }
}

pub(crate) fn eval_range_strictly_left_generic(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let (Some((_, lhs_upper)), Some((rhs_lower, _))) =
        (operand_extents(left)?, operand_extents(right)?)
    else {
        return Ok(Value::Boolean(false));
    };
    Ok(Value::Boolean(upper_strictly_left_of_lower(
        &lhs_upper, &rhs_lower,
    )))
}

pub(crate) fn eval_range_strictly_right_generic(left: &Value, right: &Value) -> DbResult<Value> {
    eval_range_strictly_left_generic(right, left)
}

pub(crate) fn eval_range_not_extend_right_generic(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let (Some((_, lhs_upper)), Some((_, rhs_upper))) =
        (operand_extents(left)?, operand_extents(right)?)
    else {
        return Ok(Value::Boolean(false));
    };
    Ok(Value::Boolean(!matches!(
        cmp_upper_bound(&lhs_upper, &rhs_upper),
        Some(Ordering::Greater)
    )))
}

pub(crate) fn eval_range_not_extend_left_generic(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let (Some((lhs_lower, _)), Some((rhs_lower, _))) =
        (operand_extents(left)?, operand_extents(right)?)
    else {
        return Ok(Value::Boolean(false));
    };
    Ok(Value::Boolean(!matches!(
        cmp_lower_bound(&lhs_lower, &rhs_lower),
        Some(Ordering::Less)
    )))
}

pub(crate) fn eval_range_adjacent_generic(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let lhs = operand_to_ranges(left)?;
    let rhs = operand_to_ranges(right)?;
    if lhs.is_empty() || rhs.is_empty() {
        return Ok(Value::Boolean(false));
    }
    Ok(Value::Boolean(
        lhs.iter().any(|l| rhs.iter().any(|r| l.is_adjacent(r))),
    ))
}

pub(crate) fn eval_multirange_lower(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        return Err(DbError::internal("range function requires 1 argument"));
    }
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let ranges = parse_multirange_value(&args[0])?;
    let Some(first) = ranges.first() else {
        return Ok(Value::Null);
    };
    match &first.lower {
        RangeBound::Unbounded => Ok(Value::Null),
        RangeBound::Inclusive(v) | RangeBound::Exclusive(v) => match v {
            RangeVal::Int(n) => Ok(Value::Int(*n)),
            RangeVal::BigInt(n) => Ok(Value::BigInt(*n)),
            RangeVal::Numeric(n) => Ok(Value::Numeric(n.clone())),
            RangeVal::Text(text) => Ok(Value::Text(text.clone())),
            RangeVal::Date(d) => Ok(Value::Date(*d)),
            RangeVal::Timestamp(ts) => Ok(Value::Timestamp(*ts)),
            RangeVal::TimestampTz(ts) => Ok(Value::TimestampTz(*ts)),
        },
    }
}

pub(crate) fn eval_multirange_upper(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        return Err(DbError::internal("range function requires 1 argument"));
    }
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let ranges = parse_multirange_value(&args[0])?;
    let Some(last) = ranges.last() else {
        return Ok(Value::Null);
    };
    match &last.upper {
        RangeBound::Unbounded => Ok(Value::Null),
        RangeBound::Inclusive(v) | RangeBound::Exclusive(v) => match v {
            RangeVal::Int(n) => Ok(Value::Int(*n)),
            RangeVal::BigInt(n) => Ok(Value::BigInt(*n)),
            RangeVal::Numeric(n) => Ok(Value::Numeric(n.clone())),
            RangeVal::Text(text) => Ok(Value::Text(text.clone())),
            RangeVal::Date(d) => Ok(Value::Date(*d)),
            RangeVal::Timestamp(ts) => Ok(Value::Timestamp(*ts)),
            RangeVal::TimestampTz(ts) => Ok(Value::TimestampTz(*ts)),
        },
    }
}

pub(crate) fn canonical_multirange_text_for_kind(
    literal: &str,
    kind: RangeKind,
) -> DbResult<String> {
    let trimmed = literal.trim();
    let ranges = if looks_like_multirange(trimmed) {
        let inner = &trimmed[1..trimmed.len() - 1];
        if inner.trim().is_empty() {
            Vec::new()
        } else {
            let mut parsed = Vec::new();
            for item in split_multirange_items(inner, trimmed)? {
                let token = item.trim();
                if token.eq_ignore_ascii_case("empty") {
                    continue;
                }
                let parsed_range = parse_range_text(token, &kind)?;
                if !parsed_range.empty {
                    parsed.push(parsed_range);
                }
            }
            canonicalize_multirange_ranges(parsed)
        }
    } else if trimmed.eq_ignore_ascii_case("empty") {
        Vec::new()
    } else {
        let range = parse_range_text(trimmed, &kind)?;
        if range.empty {
            Vec::new()
        } else {
            vec![range]
        }
    };
    if ranges.is_empty() {
        return Ok("{}".to_owned());
    }
    let mut text = String::from("{");
    for (idx, range) in ranges.iter().enumerate() {
        if idx > 0 {
            text.push(',');
        }
        text.push_str(&range.to_pg_text());
    }
    text.push('}');
    Ok(text)
}

/// Best-effort canonicalisation of a Text value that *looks* like a
/// PG range or multirange literal.  Tries each subtype kind and
/// returns the canonical form for the first that parses cleanly.
/// `None` means the value is not a range/multirange-shaped literal
/// or no kind matched, in which case the caller leaves the value
/// untouched.  Used by the INSERT path so that stored values match
/// the canonical PG form regardless of how the user wrote them.
pub fn try_canonicalize_range_or_multirange_text(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let is_multirange = looks_like_multirange(trimmed);
    let is_range_literal =
        !is_multirange && (looks_like_range(trimmed) || trimmed.eq_ignore_ascii_case("empty"));
    if !is_multirange && !is_range_literal {
        return None;
    }
    let kinds = [
        RangeKind::Int4,
        RangeKind::Int8,
        RangeKind::Numeric,
        RangeKind::Date,
        RangeKind::Timestamp,
        RangeKind::TimestampTz,
        RangeKind::Text,
    ];
    if is_multirange {
        for kind in kinds {
            if let Ok(text) = canonical_multirange_text_for_kind(trimmed, kind) {
                return Some(text);
            }
        }
        return None;
    }
    for kind in kinds {
        if let Ok(range) = parse_range_text(trimmed, &kind) {
            return Some(range.to_pg_text());
        }
    }
    None
}

pub(crate) fn compare_multirange_text(left: &str, right: &str) -> DbResult<Option<Ordering>> {
    let left_ranges = parse_multirange_value(&Value::Text(left.to_owned()))?;
    let right_ranges = parse_multirange_value(&Value::Text(right.to_owned()))?;
    for (l, r) in left_ranges.iter().zip(right_ranges.iter()) {
        let Some(ord) = l.cmp_range(r) else {
            return Ok(None);
        };
        if ord != Ordering::Equal {
            return Ok(Some(ord));
        }
    }
    Ok(Some(left_ranges.len().cmp(&right_ranges.len())))
}

fn render_multirange_ranges(ranges: &[Range]) -> String {
    if ranges.is_empty() {
        return "{}".to_owned();
    }
    let mut out = String::from("{");
    for (idx, range) in ranges.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&range.to_pg_text());
    }
    out.push('}');
    out
}

pub(crate) fn eval_range_union_op(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let r1 = text_to_range(left)?;
    let r2 = text_to_range(right)?;
    let result = r1.union(&r2)?;
    Ok(Value::Text(result.to_pg_text()))
}

pub(crate) fn eval_range_intersect_op(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let r1 = text_to_range(left)?;
    let r2 = text_to_range(right)?;
    let result = r1.intersect(&r2)?;
    Ok(Value::Text(result.to_pg_text()))
}

pub(crate) fn eval_multirange_union(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let mut ranges = parse_multirange_value(left)?;
    ranges.extend(parse_multirange_value(right)?);
    let merged = canonicalize_multirange_ranges(ranges);
    Ok(Value::Text(render_multirange_ranges(&merged)))
}

pub(crate) fn eval_multirange_minus(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let lhs = parse_multirange_value(left)?;
    let rhs = parse_multirange_value(right)?;
    let mut acc: Vec<Range> = lhs;
    for sub in &rhs {
        let mut next = Vec::with_capacity(acc.len());
        for r in &acc {
            for piece in subtract_one_range(r, sub) {
                if !piece.empty {
                    next.push(piece);
                }
            }
        }
        acc = next;
    }
    let merged = canonicalize_multirange_ranges(acc);
    Ok(Value::Text(render_multirange_ranges(&merged)))
}

pub(crate) fn eval_multirange_intersect(left: &Value, right: &Value) -> DbResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let lhs = parse_multirange_value(left)?;
    let rhs = parse_multirange_value(right)?;
    let mut out: Vec<Range> = Vec::new();
    for a in &lhs {
        for b in &rhs {
            if let Ok(r) = a.intersect(b) {
                if !r.empty {
                    out.push(r);
                }
            }
        }
    }
    let merged = canonicalize_multirange_ranges(out);
    Ok(Value::Text(render_multirange_ranges(&merged)))
}

fn subtract_one_range(src: &Range, sub: &Range) -> Vec<Range> {
    if src.empty {
        return Vec::new();
    }
    if sub.empty || !src.overlaps(sub) {
        return vec![src.clone()];
    }
    if sub.contains_range(src) {
        return Vec::new();
    }
    let mut out: Vec<Range> = Vec::new();
    let left_keeps = match (src.lower_val(), sub.lower_val()) {
        (None, Some(_)) => true,
        (Some(s), Some(b)) => match s.cmp_val(b) {
            Some(Ordering::Less) => true,
            Some(Ordering::Equal) => src.lower_inclusive() && !sub.lower_inclusive(),
            _ => false,
        },
        _ => false,
    };
    if left_keeps {
        let upper = invert_bound(&sub.lower);
        let mut piece = Range {
            kind: src.kind,
            lower: src.lower.clone(),
            upper,
            empty: false,
        };
        if piece.canonicalize().is_ok() && !piece.empty {
            out.push(piece);
        }
    }
    let right_keeps = match (sub.upper_val(), src.upper_val()) {
        (Some(_), None) => true,
        (Some(b), Some(s)) => match b.cmp_val(s) {
            Some(Ordering::Less) => true,
            Some(Ordering::Equal) => !sub.upper_inclusive() && src.upper_inclusive(),
            _ => false,
        },
        _ => false,
    };
    if right_keeps {
        let lower = invert_bound(&sub.upper);
        let mut piece = Range {
            kind: src.kind,
            lower,
            upper: src.upper.clone(),
            empty: false,
        };
        if piece.canonicalize().is_ok() && !piece.empty {
            out.push(piece);
        }
    }
    out
}

// =====================================================================
// Range bound representation
// =====================================================================

/// A single bound of a range - either inclusive or exclusive.
#[derive(Clone, Debug)]
pub(crate) enum RangeBound {
    /// No bound (unbounded / infinity)
    Unbounded,
    /// Inclusive bound
    Inclusive(RangeVal),
    /// Exclusive bound
    Exclusive(RangeVal),
}

/// The scalar value stored in a range bound.
#[derive(Clone, Debug)]
pub(crate) enum RangeVal {
    Int(i32),
    BigInt(i64),
    Numeric(NumericValue),
    Text(String),
    Date(time::Date),
    Timestamp(time::PrimitiveDateTime),
    TimestampTz(time::OffsetDateTime),
}

impl RangeVal {
    fn cmp_val(&self, other: &RangeVal) -> Option<Ordering> {
        match (self, other) {
            (RangeVal::Int(a), RangeVal::Int(b)) => Some(a.cmp(b)),
            (RangeVal::BigInt(a), RangeVal::BigInt(b)) => Some(a.cmp(b)),
            (RangeVal::Int(a), RangeVal::BigInt(b)) => Some(i64::from(*a).cmp(b)),
            (RangeVal::BigInt(a), RangeVal::Int(b)) => Some(a.cmp(&i64::from(*b))),
            (RangeVal::Int(a), RangeVal::Numeric(b)) => {
                Some(NumericValue::new(i128::from(*a), 0).cmp(b))
            }
            (RangeVal::Numeric(a), RangeVal::Int(b)) => {
                Some(a.cmp(&NumericValue::new(i128::from(*b), 0)))
            }
            (RangeVal::BigInt(a), RangeVal::Numeric(b)) => {
                Some(NumericValue::new(i128::from(*a), 0).cmp(b))
            }
            (RangeVal::Numeric(a), RangeVal::BigInt(b)) => {
                Some(a.cmp(&NumericValue::new(i128::from(*b), 0)))
            }
            (RangeVal::Numeric(a), RangeVal::Numeric(b)) => Some(a.cmp(b)),
            (RangeVal::Text(a), RangeVal::Text(b)) => Some(a.cmp(b)),
            (RangeVal::Date(a), RangeVal::Date(b)) => Some(a.cmp(b)),
            (RangeVal::Timestamp(a), RangeVal::Timestamp(b)) => Some(a.cmp(b)),
            (RangeVal::TimestampTz(a), RangeVal::TimestampTz(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }

    fn display(&self, _kind: &RangeKind) -> String {
        match self {
            RangeVal::Int(v) => v.to_string(),
            RangeVal::BigInt(v) => v.to_string(),
            RangeVal::Numeric(v) => v.to_string(),
            RangeVal::Text(v) => quote_bound_if_needed(v),
            RangeVal::Date(v) => format_date(*v),
            RangeVal::Timestamp(v) => format_timestamp(v),
            RangeVal::TimestampTz(v) => format_timestamptz(v),
        }
    }

    /// For discrete types (int4, int8, date), return the successor value.
    fn successor(&self) -> Option<RangeVal> {
        match self {
            RangeVal::Int(v) => v.checked_add(1).map(RangeVal::Int),
            RangeVal::BigInt(v) => v.checked_add(1).map(RangeVal::BigInt),
            RangeVal::Date(d) => d.next_day().map(RangeVal::Date),
            _ => None,
        }
    }

    /// For discrete types, return the predecessor value.
    #[expect(dead_code, reason = "reserved for future discrete-range operations")]
    fn predecessor(&self) -> Option<RangeVal> {
        match self {
            RangeVal::Int(v) => v.checked_sub(1).map(RangeVal::Int),
            RangeVal::BigInt(v) => v.checked_sub(1).map(RangeVal::BigInt),
            RangeVal::Date(d) => d.previous_day().map(RangeVal::Date),
            _ => None,
        }
    }

    fn is_discrete(&self) -> bool {
        matches!(
            self,
            RangeVal::Int(_) | RangeVal::BigInt(_) | RangeVal::Date(_)
        )
    }
}

// =====================================================================
// Range type enum
// =====================================================================

#[derive(Clone, Copy, Debug)]
pub(crate) enum RangeKind {
    Int4,
    Int8,
    Numeric,
    Text,
    Date,
    Timestamp,
    TimestampTz,
}

// =====================================================================
// Range struct
// =====================================================================

#[derive(Clone, Debug)]
pub(crate) struct Range {
    pub kind: RangeKind,
    pub lower: RangeBound,
    pub upper: RangeBound,
    pub empty: bool,
}

impl Range {
    pub fn empty(kind: RangeKind) -> Self {
        Self {
            kind,
            lower: RangeBound::Unbounded,
            upper: RangeBound::Unbounded,
            empty: true,
        }
    }

    pub fn new(kind: RangeKind, lower: RangeBound, upper: RangeBound) -> DbResult<Self> {
        let mut r = Self {
            kind,
            lower,
            upper,
            empty: false,
        };
        r.canonicalize()?;
        Ok(r)
    }

    /// Canonicalize: for discrete types, convert to [inclusive, exclusive).
    /// Also detect empty ranges.
    fn canonicalize(&mut self) -> DbResult<()> {
        if self.empty {
            return Ok(());
        }
        // For discrete types, normalize bounds
        let is_discrete = match &self.lower {
            RangeBound::Inclusive(v) | RangeBound::Exclusive(v) => v.is_discrete(),
            RangeBound::Unbounded => match &self.upper {
                RangeBound::Inclusive(v) | RangeBound::Exclusive(v) => v.is_discrete(),
                RangeBound::Unbounded => {
                    matches!(
                        self.kind,
                        RangeKind::Int4 | RangeKind::Int8 | RangeKind::Date
                    )
                }
            },
        };

        if is_discrete {
            // Normalize lower: exclusive -> inclusive(successor)
            if let RangeBound::Exclusive(v) = &self.lower {
                if let Some(succ) = v.successor() {
                    self.lower = RangeBound::Inclusive(succ);
                } else {
                    self.empty = true;
                    return Ok(());
                }
            }
            // Normalize upper: inclusive -> exclusive(successor)
            if let RangeBound::Inclusive(v) = &self.upper {
                if let Some(succ) = v.successor() {
                    self.upper = RangeBound::Exclusive(succ);
                } else {
                    // Overflow means there is no representable successor.
                    // Keep inclusive upper bound so singleton max ranges stay non-empty.
                }
            }
        }

        // Check if range is empty (lower >= upper for non-unbounded)
        if let (Some(l), Some(u)) = (self.lower_val(), self.upper_val()) {
            match l.cmp_val(u) {
                Some(Ordering::Greater) => {
                    return Err(DbError::from_report(ErrorReport::new(
                        SqlState::InvalidTextRepresentation,
                        "range lower bound must be less than or equal to range upper bound",
                    )));
                }
                Some(Ordering::Equal) => {
                    let both_inc = matches!(self.lower, RangeBound::Inclusive(_))
                        && matches!(self.upper, RangeBound::Inclusive(_));
                    if !both_inc {
                        self.empty = true;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn lower_val(&self) -> Option<&RangeVal> {
        match &self.lower {
            RangeBound::Inclusive(v) | RangeBound::Exclusive(v) => Some(v),
            RangeBound::Unbounded => None,
        }
    }

    fn upper_val(&self) -> Option<&RangeVal> {
        match &self.upper {
            RangeBound::Inclusive(v) | RangeBound::Exclusive(v) => Some(v),
            RangeBound::Unbounded => None,
        }
    }

    fn lower_inclusive(&self) -> bool {
        matches!(self.lower, RangeBound::Inclusive(_))
    }

    fn upper_inclusive(&self) -> bool {
        matches!(self.upper, RangeBound::Inclusive(_))
    }

    fn lower_unbounded(&self) -> bool {
        matches!(self.lower, RangeBound::Unbounded)
    }

    fn upper_unbounded(&self) -> bool {
        matches!(self.upper, RangeBound::Unbounded)
    }

    /// Format as PostgreSQL canonical text.
    pub fn to_pg_text(&self) -> String {
        if self.empty {
            return "empty".to_string();
        }
        let lb = if self.lower_inclusive() { "[" } else { "(" };
        let ub = if self.upper_inclusive() { "]" } else { ")" };
        let lower_str = match &self.lower {
            RangeBound::Unbounded => String::new(),
            RangeBound::Inclusive(v) | RangeBound::Exclusive(v) => v.display(&self.kind),
        };
        let upper_str = match &self.upper {
            RangeBound::Unbounded => String::new(),
            RangeBound::Inclusive(v) | RangeBound::Exclusive(v) => v.display(&self.kind),
        };
        format!("{lb}{lower_str},{upper_str}{ub}")
    }

    /// Check if a scalar value is contained in this range.
    fn contains_val(&self, val: &RangeVal) -> bool {
        if self.empty {
            return false;
        }
        // Check lower bound
        if let Some(l) = self.lower_val() {
            match l.cmp_val(val) {
                Some(Ordering::Greater) => return false,
                Some(Ordering::Equal) => {
                    if !self.lower_inclusive() {
                        return false;
                    }
                }
                _ => {}
            }
        }
        // Check upper bound
        if let Some(u) = self.upper_val() {
            match u.cmp_val(val) {
                Some(Ordering::Less) => return false,
                Some(Ordering::Equal) => {
                    if !self.upper_inclusive() {
                        return false;
                    }
                }
                _ => {}
            }
        }
        true
    }

    /// Check if this range contains another range (@>).
    fn contains_range(&self, other: &Range) -> bool {
        if other.empty {
            return true;
        }
        if self.empty {
            return false;
        }
        // Check lower: self.lower <= other.lower
        match (self.lower_val(), other.lower_val()) {
            (None, _) => {}                  // self is unbounded below, always ok
            (Some(_), None) => return false, // self bounded, other not
            (Some(sl), Some(ol)) => match sl.cmp_val(ol) {
                Some(Ordering::Greater) => return false,
                Some(Ordering::Equal) => {
                    if !self.lower_inclusive() && other.lower_inclusive() {
                        return false;
                    }
                }
                _ => {}
            },
        }
        // Check upper: self.upper >= other.upper
        match (self.upper_val(), other.upper_val()) {
            (_, None) => {
                if self.upper_unbounded() {
                    // both unbounded above, ok
                } else {
                    return false;
                }
            }
            (None, Some(_)) => {} // self unbounded above, always ok
            (Some(su), Some(ou)) => match su.cmp_val(ou) {
                Some(Ordering::Less) => return false,
                Some(Ordering::Equal) => {
                    if !self.upper_inclusive() && other.upper_inclusive() {
                        return false;
                    }
                }
                _ => {}
            },
        }
        true
    }

    /// Check if two ranges overlap (&&).
    fn overlaps(&self, other: &Range) -> bool {
        if self.empty || other.empty {
            return false;
        }
        // Ranges overlap if self.lower < other.upper AND other.lower < self.upper
        !self.strictly_left_of(other) && !other.strictly_left_of(self)
    }

    /// Check if this range is strictly left of another (<<).
    fn strictly_left_of(&self, other: &Range) -> bool {
        if self.empty || other.empty {
            return false;
        }
        match (self.upper_val(), other.lower_val()) {
            (None, _) | (_, None) => false,
            (Some(su), Some(ol)) => match su.cmp_val(ol) {
                Some(Ordering::Less) => true,
                Some(Ordering::Equal) => !self.upper_inclusive() || !other.lower_inclusive(),
                _ => false,
            },
        }
    }

    /// Check if ranges are adjacent (-|-).
    fn is_adjacent(&self, other: &Range) -> bool {
        if self.empty || other.empty {
            return false;
        }
        // Check if self's upper matches other's lower with opposite inclusivity
        if let (Some(su), Some(ol)) = (self.upper_val(), other.lower_val()) {
            if let Some(Ordering::Equal) = su.cmp_val(ol) {
                // One must be inclusive and the other exclusive
                if self.upper_inclusive() != other.lower_inclusive() {
                    return true;
                }
            }
        }
        // Check the reverse
        if let (Some(ou), Some(sl)) = (other.upper_val(), self.lower_val()) {
            if let Some(Ordering::Equal) = ou.cmp_val(sl) {
                if other.upper_inclusive() != self.lower_inclusive() {
                    return true;
                }
            }
        }
        false
    }

    /// Union of two ranges (+). Ranges must overlap or be adjacent.
    fn union(&self, other: &Range) -> DbResult<Range> {
        if self.empty {
            return Ok(other.clone());
        }
        if other.empty {
            return Ok(self.clone());
        }
        if !self.overlaps(other) && !self.is_adjacent(other) {
            return Err(DbError::internal(
                "result of range union would not be contiguous",
            ));
        }
        let lower = range_bound_min_lower(&self.lower, &other.lower);
        let upper = range_bound_max_upper(&self.upper, &other.upper);
        Ok(Range {
            kind: self.kind,
            lower,
            upper,
            empty: false,
        })
    }

    /// Intersection of two ranges (*).
    fn intersect(&self, other: &Range) -> DbResult<Range> {
        if self.empty || other.empty || !self.overlaps(other) {
            return Ok(Range::empty(self.kind));
        }
        let lower = range_bound_max_lower(&self.lower, &other.lower);
        let upper = range_bound_min_upper(&self.upper, &other.upper);
        let mut r = Range {
            kind: self.kind,
            lower,
            upper,
            empty: false,
        };
        r.canonicalize()?;
        Ok(r)
    }

    /// Difference of two ranges (-).
    fn difference(&self, other: &Range) -> DbResult<Range> {
        if self.empty || other.empty {
            return Ok(self.clone());
        }
        if !self.overlaps(other) {
            return Ok(self.clone());
        }
        // If other completely contains self, result is empty
        if other.contains_range(self) {
            return Ok(Range::empty(self.kind));
        }
        // Check if other splits self (would produce two ranges - error)
        let other_contains_lower = match self.lower_val() {
            None => !other.lower_unbounded(),
            Some(lv) => other.contains_val(lv),
        };
        let other_contains_upper = match self.upper_val() {
            None => !other.upper_unbounded(),
            Some(uv) => other.contains_val(uv),
        };
        if !other_contains_lower && !other_contains_upper {
            // other is in the middle of self - can't subtract
            return Err(DbError::internal(
                "result of range difference would not be contiguous",
            ));
        }
        if other_contains_lower {
            // Remove from the start
            let lower = invert_bound(&other.upper);
            Ok(Range {
                kind: self.kind,
                lower,
                upper: self.upper.clone(),
                empty: false,
            })
        } else {
            // Remove from the end
            let upper = invert_bound(&other.lower);
            Ok(Range {
                kind: self.kind,
                lower: self.lower.clone(),
                upper,
                empty: false,
            })
        }
    }

    /// Compare two ranges for ordering (for < > <= >= operators).
    fn cmp_range(&self, other: &Range) -> Option<Ordering> {
        // Empty ranges sort first
        match (self.empty, other.empty) {
            (true, true) => return Some(Ordering::Equal),
            (true, false) => return Some(Ordering::Less),
            (false, true) => return Some(Ordering::Greater),
            (false, false) => {}
        }
        // Compare lower bounds
        let lower_cmp = cmp_lower_bound(&self.lower, &other.lower);
        if let Some(ord) = lower_cmp {
            if ord != Ordering::Equal {
                return Some(ord);
            }
        }
        // Lower bounds are equal, compare upper bounds
        cmp_upper_bound(&self.upper, &other.upper)
    }

    /// Merge two ranges (range_merge) - like union but doesn't require overlap.
    fn merge(&self, other: &Range) -> Range {
        if self.empty {
            return other.clone();
        }
        if other.empty {
            return self.clone();
        }
        let lower = range_bound_min_lower(&self.lower, &other.lower);
        let upper = range_bound_max_upper(&self.upper, &other.upper);
        Range {
            kind: self.kind,
            lower,
            upper,
            empty: false,
        }
    }
}

// =====================================================================
// Bound comparison helpers
// =====================================================================

fn cmp_lower_bound(a: &RangeBound, b: &RangeBound) -> Option<Ordering> {
    match (a, b) {
        (RangeBound::Unbounded, RangeBound::Unbounded) => Some(Ordering::Equal),
        (RangeBound::Unbounded, _) => Some(Ordering::Less),
        (_, RangeBound::Unbounded) => Some(Ordering::Greater),
        (RangeBound::Inclusive(av), RangeBound::Inclusive(bv)) => av.cmp_val(bv),
        (RangeBound::Inclusive(av), RangeBound::Exclusive(bv)) => match av.cmp_val(bv) {
            Some(Ordering::Equal) => Some(Ordering::Less), // inclusive < exclusive for lower
            other => other,
        },
        (RangeBound::Exclusive(av), RangeBound::Inclusive(bv)) => match av.cmp_val(bv) {
            Some(Ordering::Equal) => Some(Ordering::Greater),
            other => other,
        },
        (RangeBound::Exclusive(av), RangeBound::Exclusive(bv)) => av.cmp_val(bv),
    }
}

fn cmp_upper_bound(a: &RangeBound, b: &RangeBound) -> Option<Ordering> {
    match (a, b) {
        (RangeBound::Unbounded, RangeBound::Unbounded) => Some(Ordering::Equal),
        (RangeBound::Unbounded, _) => Some(Ordering::Greater),
        (_, RangeBound::Unbounded) => Some(Ordering::Less),
        (RangeBound::Inclusive(av), RangeBound::Inclusive(bv)) => av.cmp_val(bv),
        (RangeBound::Inclusive(av), RangeBound::Exclusive(bv)) => match av.cmp_val(bv) {
            Some(Ordering::Equal) => Some(Ordering::Greater), // inclusive > exclusive for upper
            other => other,
        },
        (RangeBound::Exclusive(av), RangeBound::Inclusive(bv)) => match av.cmp_val(bv) {
            Some(Ordering::Equal) => Some(Ordering::Less),
            other => other,
        },
        (RangeBound::Exclusive(av), RangeBound::Exclusive(bv)) => av.cmp_val(bv),
    }
}

fn range_bound_min_lower(a: &RangeBound, b: &RangeBound) -> RangeBound {
    match cmp_lower_bound(a, b) {
        Some(Ordering::Less | Ordering::Equal) => a.clone(),
        _ => b.clone(),
    }
}

fn range_bound_max_upper(a: &RangeBound, b: &RangeBound) -> RangeBound {
    match cmp_upper_bound(a, b) {
        Some(Ordering::Greater | Ordering::Equal) => a.clone(),
        _ => b.clone(),
    }
}

fn range_bound_max_lower(a: &RangeBound, b: &RangeBound) -> RangeBound {
    match cmp_lower_bound(a, b) {
        Some(Ordering::Greater | Ordering::Equal) => a.clone(),
        _ => b.clone(),
    }
}

fn range_bound_min_upper(a: &RangeBound, b: &RangeBound) -> RangeBound {
    match cmp_upper_bound(a, b) {
        Some(Ordering::Less | Ordering::Equal) => a.clone(),
        _ => b.clone(),
    }
}

fn invert_bound(bound: &RangeBound) -> RangeBound {
    match bound {
        RangeBound::Unbounded => RangeBound::Unbounded,
        RangeBound::Inclusive(v) => RangeBound::Exclusive(v.clone()),
        RangeBound::Exclusive(v) => RangeBound::Inclusive(v.clone()),
    }
}

// =====================================================================
// Parsing range text into Range struct
// =====================================================================

pub(crate) fn parse_range_text(s: &str, kind: &RangeKind) -> DbResult<Range> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("empty") {
        return Ok(Range::empty(*kind));
    }
    if s.len() < 3 {
        return Err(malformed_range_literal(s));
    }
    let first = s.as_bytes()[0];
    let last = s.as_bytes()[s.len() - 1];
    let lower_inc = match first {
        b'[' => true,
        b'(' => false,
        _ => return Err(malformed_range_literal(s)),
    };
    let upper_inc = match last {
        b']' => true,
        b')' => false,
        _ => return Err(malformed_range_literal(s)),
    };
    let inner = &s[1..s.len() - 1];
    // Split on comma, being careful of quoted strings
    let comma_pos = find_comma(inner, s)?;
    // For text bounds, PG does not strip surrounding whitespace - a literal
    // space between brackets and comma becomes part of the bound. Preserve
    // verbatim and let `parse_range_val` apply quote/escape decoding. For
    // non-text kinds, PG's type input functions tolerate leading/trailing
    // whitespace around the bound, so trimming is harmless.
    let (lower_str, upper_str) = match kind {
        RangeKind::Text => (&inner[..comma_pos], &inner[comma_pos + 1..]),
        _ => (inner[..comma_pos].trim(), inner[comma_pos + 1..].trim()),
    };

    let lower = if lower_str.is_empty() {
        RangeBound::Unbounded
    } else {
        let val = parse_range_val(lower_str, kind)?;
        if lower_inc {
            RangeBound::Inclusive(val)
        } else {
            RangeBound::Exclusive(val)
        }
    };

    let upper = if upper_str.is_empty() {
        RangeBound::Unbounded
    } else {
        let val = parse_range_val(upper_str, kind)?;
        if upper_inc {
            RangeBound::Inclusive(val)
        } else {
            RangeBound::Exclusive(val)
        }
    };

    Range::new(*kind, lower, upper)
}

fn find_comma(s: &str, full_literal: &str) -> DbResult<usize> {
    let mut in_quote = false;
    let mut escape = false;
    let mut comma_pos: Option<usize> = None;
    for (i, ch) in s.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        match ch {
            '\\' => escape = true,
            '"' => in_quote = !in_quote,
            ',' if !in_quote => {
                if comma_pos.is_some() {
                    return Err(malformed_range_literal(full_literal));
                }
                comma_pos = Some(i);
            }
            _ => {}
        }
    }
    if in_quote || escape {
        return Err(malformed_range_literal(full_literal));
    }
    comma_pos.ok_or_else(|| malformed_range_literal(full_literal))
}

fn parse_range_val(s: &str, kind: &RangeKind) -> DbResult<RangeVal> {
    // For Text kind we must apply PG range_parse_bound semantics (handle
    // `""` doubling, `\X` escapes, multiple quoted segments, no whitespace
    // trim). For other kinds, the bound was already trimmed; strip a single
    // surrounding pair of quotes if present so that callers may pass a
    // pre-quoted scalar.
    if matches!(kind, RangeKind::Text) {
        let (decoded, _was_quoted) = decode_text_bound(s)?;
        return Ok(RangeVal::Text(decoded));
    }
    let s = if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        &s[1..s.len() - 1]
    } else {
        s
    };
    match kind {
        RangeKind::Int4 => {
            let v: i32 = s.trim().parse().map_err(|_| {
                DbError::from_report(ErrorReport::new(
                    SqlState::InvalidTextRepresentation,
                    format!("invalid input syntax for type integer: \"{s}\""),
                ))
            })?;
            Ok(RangeVal::Int(v))
        }
        RangeKind::Int8 => {
            let v: i64 = s.trim().parse().map_err(|_| {
                DbError::from_report(ErrorReport::new(
                    SqlState::InvalidTextRepresentation,
                    format!("invalid input syntax for type bigint: \"{s}\""),
                ))
            })?;
            Ok(RangeVal::BigInt(v))
        }
        RangeKind::Numeric => {
            let v: NumericValue = s.trim().parse().map_err(|_| {
                DbError::from_report(ErrorReport::new(
                    SqlState::InvalidTextRepresentation,
                    format!("invalid input syntax for type numeric: \"{s}\""),
                ))
            })?;
            Ok(RangeVal::Numeric(v))
        }
        RangeKind::Text => unreachable!("Text kind handled in early return"),
        RangeKind::Date => {
            let d = parse_date_simple(s.trim())?;
            Ok(RangeVal::Date(d))
        }
        RangeKind::Timestamp => {
            let ts = parse_timestamp_simple(s.trim())?;
            Ok(RangeVal::Timestamp(ts))
        }
        RangeKind::TimestampTz => {
            let ts = parse_timestamptz_simple(s.trim())?;
            Ok(RangeVal::TimestampTz(ts))
        }
    }
}

fn parse_date_simple(s: &str) -> DbResult<time::Date> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidDatetimeFormat,
            format!("cannot parse date: \"{s}\""),
        )));
    }
    let y: i32 = parts[0].parse().map_err(|_| {
        DbError::from_report(ErrorReport::new(
            SqlState::InvalidDatetimeFormat,
            format!("cannot parse date: \"{s}\""),
        ))
    })?;
    let m: u8 = parts[1].parse().map_err(|_| {
        DbError::from_report(ErrorReport::new(
            SqlState::InvalidDatetimeFormat,
            format!("cannot parse date: \"{s}\""),
        ))
    })?;
    let d: u8 = parts[2].parse().map_err(|_| {
        DbError::from_report(ErrorReport::new(
            SqlState::InvalidDatetimeFormat,
            format!("cannot parse date: \"{s}\""),
        ))
    })?;
    let month = time::Month::try_from(m).map_err(|_| {
        DbError::from_report(ErrorReport::new(
            SqlState::InvalidDatetimeFormat,
            format!("cannot parse date: \"{s}\""),
        ))
    })?;
    time::Date::from_calendar_date(y, month, d).map_err(|_| {
        DbError::from_report(ErrorReport::new(
            SqlState::InvalidDatetimeFormat,
            format!("cannot parse date: \"{s}\""),
        ))
    })
}

fn parse_timestamp_simple(s: &str) -> DbResult<time::PrimitiveDateTime> {
    // Expect "YYYY-MM-DD HH:MM:SS" or "YYYY-MM-DD HH:MM:SS.ffffff"
    let parts: Vec<&str> = s.splitn(2, ' ').collect();
    if parts.len() != 2 {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidDatetimeFormat,
            format!("cannot parse timestamp: \"{s}\""),
        )));
    }
    let date = parse_date_simple(parts[0])?;
    let time = parse_time_simple(parts[1])?;
    Ok(time::PrimitiveDateTime::new(date, time))
}

fn parse_timestamptz_simple(s: &str) -> DbResult<time::OffsetDateTime> {
    // Try to parse with offset
    let ts = parse_timestamp_simple(s)?;
    Ok(ts.assume_utc())
}

fn parse_time_simple(s: &str) -> DbResult<time::Time> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 2 {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidDatetimeFormat,
            format!("cannot parse time: \"{s}\""),
        )));
    }
    let h: u8 = parts[0].parse().map_err(|_| {
        DbError::from_report(ErrorReport::new(
            SqlState::InvalidDatetimeFormat,
            format!("cannot parse time: \"{s}\""),
        ))
    })?;
    let m: u8 = parts[1].parse().map_err(|_| {
        DbError::from_report(ErrorReport::new(
            SqlState::InvalidDatetimeFormat,
            format!("cannot parse time: \"{s}\""),
        ))
    })?;
    let (sec, micro) = if parts.len() > 2 {
        let sec_parts: Vec<&str> = parts[2].split('.').collect();
        let s_val: u8 = sec_parts[0].parse().map_err(|_| {
            DbError::from_report(ErrorReport::new(
                SqlState::InvalidDatetimeFormat,
                format!("cannot parse time: \"{s}\""),
            ))
        })?;
        let us = if sec_parts.len() > 1 {
            let frac = sec_parts[1];
            let padded = format!("{frac:0<6}");
            padded[..6].parse::<u32>().unwrap_or(0)
        } else {
            0
        };
        (s_val, us)
    } else {
        (0, 0)
    };
    time::Time::from_hms_micro(h, m, sec, micro).map_err(|_| {
        DbError::from_report(ErrorReport::new(
            SqlState::InvalidDatetimeFormat,
            format!("cannot parse time: \"{s}\""),
        ))
    })
}

// =====================================================================
// Constructor functions: numrange, int4range, etc.
// =====================================================================

pub(crate) fn eval_range_constructor(kind: RangeKind, args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        // 0-arg: return empty range
        return Ok(Value::Text(Range::empty(kind).to_pg_text()));
    }
    if args.len() == 1 {
        // 1-arg: parse from text
        match &args[0] {
            Value::Null => return Ok(Value::Null),
            Value::Text(s) => {
                let r = parse_range_text(s, &kind)?;
                return Ok(Value::Text(r.to_pg_text()));
            }
            _ => {
                return Err(DbError::internal(
                    "range constructor: invalid argument type",
                ));
            }
        }
    }
    // Any null arg -> null
    if args[0].is_null() && args[1].is_null() {
        return Ok(Value::Text(
            Range::new(kind, RangeBound::Unbounded, RangeBound::Unbounded)?.to_pg_text(),
        ));
    }

    let bounds_text = if args.len() >= 3 {
        match &args[2] {
            Value::Text(s) => s.as_str(),
            Value::Null => "[)",
            _ => "[)",
        }
    } else {
        "[)"
    };

    let (lower_inc, upper_inc) = parse_bounds_text(bounds_text)?;

    let lower = if args[0].is_null() {
        RangeBound::Unbounded
    } else {
        let val = value_to_range_val(&args[0], &kind)?;
        if lower_inc {
            RangeBound::Inclusive(val)
        } else {
            RangeBound::Exclusive(val)
        }
    };

    let upper = if args[1].is_null() {
        RangeBound::Unbounded
    } else {
        let val = value_to_range_val(&args[1], &kind)?;
        if upper_inc {
            RangeBound::Inclusive(val)
        } else {
            RangeBound::Exclusive(val)
        }
    };

    let range = Range::new(kind, lower, upper)?;
    Ok(Value::Text(range.to_pg_text()))
}

fn parse_bounds_text(s: &str) -> DbResult<(bool, bool)> {
    match s {
        "[)" => Ok((true, false)),
        "[]" => Ok((true, true)),
        "()" => Ok((false, false)),
        "(]" => Ok((false, true)),
        _ => Err(DbError::internal(format!(
            "range bound flags must be one of \"[)\", \"[]\", \"()\", \"(]\", got \"{s}\""
        ))),
    }
}

fn value_to_range_val(v: &Value, kind: &RangeKind) -> DbResult<RangeVal> {
    match kind {
        RangeKind::Int4 => match v {
            Value::Int(n) => Ok(RangeVal::Int(*n)),
            Value::BigInt(n) => i32::try_from(*n)
                .map(RangeVal::Int)
                .map_err(|_| DbError::internal("integer out of range")),
            Value::Double(n) => {
                if n.is_nan() {
                    return Ok(RangeVal::Int(0));
                }
                f64_to_i32(*n).map(RangeVal::Int)
            }
            Value::Numeric(n) => {
                let s = n.to_string();
                let f: f64 = s.parse().unwrap_or(0.0);
                f64_to_i32(f).map(RangeVal::Int)
            }
            Value::Text(s) => {
                let parsed = s.trim();
                let v: i32 = parsed
                    .parse()
                    .map_err(|_| DbError::invalid_input_syntax("integer", parsed))?;
                Ok(RangeVal::Int(v))
            }
            _ => Err(DbError::internal("invalid input for int4range")),
        },
        RangeKind::Int8 => match v {
            Value::Int(n) => Ok(RangeVal::BigInt(i64::from(*n))),
            Value::BigInt(n) => Ok(RangeVal::BigInt(*n)),
            Value::Double(n) => f64_to_i64(*n).map(RangeVal::BigInt),
            Value::Numeric(n) => {
                let s = n.to_string();
                let f: f64 = s.parse().unwrap_or(0.0);
                f64_to_i64(f).map(RangeVal::BigInt)
            }
            Value::Text(s) => {
                let parsed = s.trim();
                let v: i64 = parsed
                    .parse()
                    .map_err(|_| DbError::invalid_input_syntax("bigint", parsed))?;
                Ok(RangeVal::BigInt(v))
            }
            _ => Err(DbError::internal("invalid input for int8range")),
        },
        RangeKind::Numeric => match v {
            Value::Numeric(n) => Ok(RangeVal::Numeric(n.clone())),
            Value::Int(n) => Ok(RangeVal::Numeric(NumericValue::from_i32(*n))),
            Value::BigInt(n) => Ok(RangeVal::Numeric(NumericValue::from_i64(*n))),
            Value::Real(n) => {
                let s = format!("{n}");
                let nv: NumericValue = s
                    .parse()
                    .map_err(|_| DbError::internal("cannot convert to numeric"))?;
                Ok(RangeVal::Numeric(nv))
            }
            Value::Double(n) => {
                let s = format!("{n}");
                let nv: NumericValue = s
                    .parse()
                    .map_err(|_| DbError::internal("cannot convert to numeric"))?;
                Ok(RangeVal::Numeric(nv))
            }
            Value::Text(s) => {
                let parsed = s.trim();
                let nv: NumericValue = parsed
                    .parse()
                    .map_err(|_| DbError::invalid_input_syntax("numeric", parsed))?;
                Ok(RangeVal::Numeric(nv))
            }
            _ => Err(DbError::internal("invalid input for numrange")),
        },
        RangeKind::Text => match v {
            Value::Text(s) => Ok(RangeVal::Text(s.trim().to_owned())),
            other => Ok(RangeVal::Text(other.to_string())),
        },
        RangeKind::Date => match v {
            Value::Date(d) => Ok(RangeVal::Date(*d)),
            Value::Text(s) => {
                let d = parse_date_simple(s.trim())?;
                Ok(RangeVal::Date(d))
            }
            _ => Err(DbError::internal("invalid input for daterange")),
        },
        RangeKind::Timestamp => match v {
            Value::Timestamp(ts) => Ok(RangeVal::Timestamp(*ts)),
            Value::Text(s) => {
                let ts = parse_timestamp_simple(s.trim())?;
                Ok(RangeVal::Timestamp(ts))
            }
            _ => Err(DbError::internal("invalid input for tsrange")),
        },
        RangeKind::TimestampTz => match v {
            Value::TimestampTz(ts) => Ok(RangeVal::TimestampTz(*ts)),
            Value::Timestamp(ts) => Ok(RangeVal::TimestampTz(ts.assume_utc())),
            Value::Text(s) => {
                let ts = parse_timestamptz_simple(s.trim())?;
                Ok(RangeVal::TimestampTz(ts))
            }
            _ => Err(DbError::internal("invalid input for tstzrange")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_multirange_merges_discrete_adjacent_ranges() {
        let canonical =
            canonical_multirange_text_for_kind("{[1,2], [3,4]}", RangeKind::Int4).unwrap();
        assert_eq!(canonical, "{[1,5)}");
    }

    #[test]
    fn canonical_multirange_drops_empty_components() {
        let canonical =
            canonical_multirange_text_for_kind("{[1,1), empty, [10,12)}", RangeKind::Int4).unwrap();
        assert_eq!(canonical, "{[10,12)}");
    }

    #[test]
    fn try_canonicalize_renders_unbounded_brackets_as_exclusive() {
        assert_eq!(
            try_canonicalize_range_or_multirange_text("{[,)}").as_deref(),
            Some("{(,)}")
        );
        assert_eq!(
            try_canonicalize_range_or_multirange_text("{[3,]}").as_deref(),
            Some("{[3,)}")
        );
        assert_eq!(
            try_canonicalize_range_or_multirange_text("{[,), [3,]}").as_deref(),
            Some("{(,)}")
        );
    }

    #[test]
    fn canonical_textmultirange_quotes_special_chars_in_bounds() {
        let canonical = canonical_multirange_text_for_kind("{[ , ]}", RangeKind::Text).unwrap();
        assert_eq!(canonical, r#"{[" "," "]}"#);
        let canonical = canonical_multirange_text_for_kind("{(!,()}", RangeKind::Text).unwrap();
        assert_eq!(canonical, r#"{(!,"(")}"#);
    }

    #[test]
    fn malformed_range_reports_invalid_text_representation() {
        let err = parse_range_text("[1,2", &RangeKind::Int4).unwrap_err();
        assert_eq!(err.report().sqlstate, SqlState::InvalidTextRepresentation);
    }

    #[test]
    fn malformed_range_with_extra_comma_is_rejected() {
        let err = parse_range_text("[1,2,3)", &RangeKind::Int4).unwrap_err();
        assert_eq!(err.report().sqlstate, SqlState::InvalidTextRepresentation);
    }

    #[test]
    fn malformed_multirange_with_trailing_comma_is_rejected() {
        let err = canonical_multirange_text_for_kind("{[1,2),}", RangeKind::Int4).unwrap_err();
        assert_eq!(err.report().sqlstate, SqlState::InvalidTextRepresentation);
    }

    #[test]
    fn discrete_range_with_reversed_bounds_reports_error() {
        let err = parse_range_text("[5,1]", &RangeKind::Int4).unwrap_err();
        assert_eq!(err.report().sqlstate, SqlState::InvalidTextRepresentation);
        assert!(err
            .report()
            .message
            .contains("range lower bound must be less than or equal to range upper bound"));
    }

    #[test]
    fn discrete_singleton_at_max_bound_is_not_marked_empty() {
        let range = parse_range_text("[2147483647,2147483647]", &RangeKind::Int4).unwrap();
        assert!(!range.empty);
    }

    #[test]
    fn multirange_contains_elem_routes_range_and_multirange_text_rhs() {
        let contains_range = eval_multirange_contains_elem(
            &Value::Text("{[1,5)}".to_owned()),
            &Value::Text("[2,3)".to_owned()),
        )
        .expect("range rhs should be treated as a range");
        assert_eq!(contains_range, Value::Boolean(true));

        let contains_multirange = eval_multirange_contains_elem(
            &Value::Text("{[1,5)}".to_owned()),
            &Value::Text("{[2,3), [4,5)}".to_owned()),
        )
        .expect("multirange rhs should be treated as a multirange");
        assert_eq!(contains_multirange, Value::Boolean(true));
    }

    #[test]
    fn elem_contained_by_multirange_routes_range_and_multirange_text_lhs() {
        let range_contained = eval_elem_contained_by_multirange(
            &Value::Text("[2,3)".to_owned()),
            &Value::Text("{[1,5)}".to_owned()),
        )
        .expect("range lhs should be treated as a range");
        assert_eq!(range_contained, Value::Boolean(true));

        let multirange_contained = eval_elem_contained_by_multirange(
            &Value::Text("{[2,3), [4,5)}".to_owned()),
            &Value::Text("{[1,6)}".to_owned()),
        )
        .expect("multirange lhs should be treated as a multirange");
        assert_eq!(multirange_contained, Value::Boolean(true));
    }
}
