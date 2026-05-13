use super::*;
use crate::eval::scalar_functions::{pg_compat, range};
use std::borrow::Cow;

/// Parse a `PostgreSQL` array literal like `{1,2,3}` or `{foo,bar}` into a `Vec<Value>`.
/// Elements are trimmed; NULL (case-insensitive) becomes `Value::Null`.
/// If the string is not a valid `{...}` literal, returns None.
fn parse_pg_array_literal(s: &str) -> Option<Vec<Value>> {
    let s = s.trim();
    let (_, s) = split_array_bound_prefix(s)?;
    parse_pg_array_literal_body(s)
}

fn parse_pg_array_literal_body(s: &str) -> Option<Vec<Value>> {
    if !s.starts_with('{') || !s.ends_with('}') {
        return None;
    }
    let inner = &s[1..s.len() - 1];
    if inner.is_empty() {
        return Some(Vec::new());
    }
    let trimmed = inner.trim_start();
    if trimmed.starts_with('{') {
        let parts = split_top_level_array_elements(inner);
        return Some(
            parts
                .into_iter()
                .map(|part| {
                    let part = part.trim();
                    if part.starts_with('{') {
                        Value::Array(parse_pg_array_literal_body(part).unwrap_or_default())
                    } else {
                        parse_pg_array_literal_element(part)
                    }
                })
                .collect(),
        );
    }
    let parts = split_top_level_array_elements(inner);
    Some(
        parts
            .into_iter()
            .map(|elem| parse_pg_array_literal_element(elem.trim()))
            .collect(),
    )
}

fn parse_pg_array_literal_element(elem: &str) -> Value {
    if elem.eq_ignore_ascii_case("null") {
        Value::Null
    } else if let Ok(i) = elem.parse::<i32>() {
        Value::Int(i)
    } else if let Ok(i) = elem.parse::<i64>() {
        Value::BigInt(i)
    } else {
        Value::Text(elem.trim_matches('"').to_owned())
    }
}

fn split_array_bound_prefix(s: &str) -> Option<(Vec<(i64, i64)>, &str)> {
    let s = s.trim();
    if !s.starts_with('[') {
        return Some((Vec::new(), s));
    }

    let mut remaining = s;
    let mut bounds = Vec::new();
    while let Some(rest) = remaining.strip_prefix('[') {
        let end = rest.find(']')?;
        let range = &rest[..end];
        let (lower, upper) = range.split_once(':')?;
        bounds.push((
            lower.trim().parse::<i64>().ok()?,
            upper.trim().parse::<i64>().ok()?,
        ));
        remaining = &rest[end + 1..];
    }

    let remaining = remaining.trim_start();
    let remaining = remaining.strip_prefix('=')?.trim_start();
    Some((bounds, remaining))
}

fn split_top_level_array_elements(s: &str) -> Vec<&str> {
    // Pre-size by counting `,` bytes - upper bound on element count.
    let comma_count = s.bytes().filter(|&b| b == b',').count();
    let mut elements = Vec::with_capacity(comma_count.saturating_add(1));
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
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    elements.push(&s[start..]);
    elements
}

pub(crate) fn coerce_to_array(val: &Value) -> Option<Cow<'_, [Value]>> {
    match val {
        Value::Array(a) => Some(Cow::Borrowed(a.as_slice())),
        Value::Text(s) => parse_pg_array_literal(s).map(Cow::Owned),
        _ => None,
    }
}

pub(crate) fn coerce_to_array_with_bounds(
    val: &Value,
) -> Option<(Vec<(i64, i64)>, Cow<'_, [Value]>)> {
    match val {
        Value::Array(a) => Some((Vec::new(), Cow::Borrowed(a.as_slice()))),
        Value::Text(s) => {
            let (bounds, body) = split_array_bound_prefix(s)?;
            Some((bounds, Cow::Owned(parse_pg_array_literal_body(body)?)))
        }
        _ => None,
    }
}

pub(super) fn reject_multidimensional_array_search(
    bounds: &[(i64, i64)],
    elements: &[Value],
) -> DbResult<()> {
    if bounds.len() > 1
        || elements
            .iter()
            .any(|value| matches!(value, Value::Array(_)))
    {
        return Err(DbError::feature_not_supported(
            "searching for elements in multidimensional arrays is not supported",
        ));
    }
    Ok(())
}

pub(crate) fn array_result_with_bounds(bounds: &[(i64, i64)], elements: Vec<Value>) -> Value {
    if bounds.is_empty() || elements.is_empty() {
        Value::Array(elements)
    } else {
        use std::fmt::Write;
        let mut prefix = String::new();
        for (lower, upper) in bounds {
            let _ = write!(prefix, "[{lower}:{upper}]");
        }
        Value::Text(format!("{prefix}={}", Value::Array(elements)))
    }
}

pub(crate) fn array_nesting_depth(elements: &[Value]) -> usize {
    fn nest(elements: &[Value], depth: usize) -> usize {
        if depth >= 256 {
            return depth;
        }
        let Some(first) = elements.first() else {
            return depth + 1;
        };
        match first {
            Value::Array(inner) => nest(inner, depth + 1),
            _ => depth + 1,
        }
    }
    nest(elements, 0)
}

pub(crate) fn array_search_matches(element: &Value, needle: &Value) -> DbResult<bool> {
    if element.is_null() && needle.is_null() {
        return Ok(true);
    }
    Ok(values_equal(element, needle)? == Some(true))
}

pub(crate) fn concatenate_array_values(left: Vec<Value>, right: Vec<Value>) -> DbResult<Value> {
    let left_dims = array_nesting_depth(&left);
    let right_dims = array_nesting_depth(&right);
    match left_dims.cmp(&right_dims) {
        std::cmp::Ordering::Equal => {
            // Reserve up front so the `extend(right)` doesn't grow
            // through doublings when left was at capacity.
            let mut result = left;
            result.reserve(right.len());
            result.extend(right);
            Ok(Value::Array(result))
        }
        std::cmp::Ordering::Less if left_dims + 1 == right_dims => {
            let mut result: Vec<Value> = Vec::with_capacity(right.len() + 1);
            result.push(Value::Array(left));
            result.extend(right);
            Ok(Value::Array(result))
        }
        std::cmp::Ordering::Greater if right_dims + 1 == left_dims => {
            // `result.push(...)` after a fresh `left` can also trigger
            // a reallocation when capacity == len.
            let mut result = left;
            result.reserve(1);
            result.push(Value::Array(right));
            Ok(Value::Array(result))
        }
        _ => Err(DbError::internal(
            "cannot concatenate incompatible array dimensions",
        )),
    }
}

// =====================================================================
// Set-returning function: unnest
// =====================================================================
/// Evaluate `unnest(array)`.
///
/// Expands an array into a set of rows.  Returns `Value::Array(...)` whose
/// elements the executor will unpack into separate rows (like
/// `generate_series`).  If the argument is already an array, the elements
/// are returned directly.  If it is NULL, an empty set is returned.
pub(crate) fn eval_unnest(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "unnest")?;
    match &args[0] {
        Value::Null => Ok(Value::Array(Vec::new())),
        Value::Text(text) if range::looks_like_multirange(text) => {
            let ranges = pg_compat::parse_and_normalize_multirange_literal(text)?;
            Ok(Value::Array(ranges.into_iter().map(Value::Text).collect()))
        }
        other => {
            if let Some((_bounds, elements)) = coerce_to_array_with_bounds(other) {
                Ok(Value::Array(elements.into_owned()))
            } else {
                Ok(Value::Array(vec![other.clone()]))
            }
        }
    }
}
