use super::value_convert::{to_i32_saturating, value_to_i32};
use aiondb_core::{DbError, DbResult, Value, VectorValue};
use aiondb_vector::distance as vdist;
use std::borrow::Cow;

#[path = "ext_array_literal_support.rs"]
mod array_literal_support;

use self::array_literal_support::reject_multidimensional_array_search;
pub(crate) use self::array_literal_support::{
    array_result_with_bounds, array_search_matches, coerce_to_array, coerce_to_array_with_bounds,
    concatenate_array_values, eval_unnest,
};
use super::geometric::{
    parse_circle_text, parse_geometry_bounds, parse_line_coefficients, parse_lseg_text,
    parse_point_text,
};
use super::{expect_arg_range, expect_args};
use crate::eval::operators::{
    eval_equality_comparison, eval_like, eval_ordering_comparison, values_equal,
};

const MAX_ARRAY_VALUE_RECURSION_DEPTH: usize = 256;
const MAX_ARRAY_FILL_DIMENSIONS: usize = 32;

// =====================================================================
// Vector distance function helpers
// =====================================================================

fn extract_vector_pair<'a>(
    args: &'a [Value],
    name: &str,
) -> DbResult<(&'a aiondb_core::VectorValue, &'a aiondb_core::VectorValue)> {
    expect_args(args, 2, name)?;
    let Value::Vector(a) = &args[0] else {
        return Err(DbError::internal(format!(
            "{name}() requires vector arguments"
        )));
    };
    let Value::Vector(b) = &args[1] else {
        return Err(DbError::internal(format!(
            "{name}() requires vector arguments"
        )));
    };
    if a.dims != b.dims {
        return Err(DbError::internal(format!(
            "{name}(): dimension mismatch ({} vs {})",
            a.dims, b.dims
        )));
    }
    Ok((a, b))
}

fn eval_vector_distance(
    args: &[Value],
    name: &str,
    dist_fn: fn(&[f32], &[f32]) -> f64,
) -> DbResult<Value> {
    expect_args(args, 2, name)?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let (a, b) = extract_vector_pair(args, name)?;
    Ok(Value::Double(dist_fn(&a.values, &b.values)))
}

pub(crate) fn eval_vector_dims(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "vector_dims")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let Value::Vector(v) = &args[0] else {
        return Err(DbError::internal(
            "vector_dims() requires a vector argument",
        ));
    };
    Ok(Value::Int(i32::try_from(v.dims).unwrap_or(i32::MAX)))
}

fn vector_l2_norm_f64(v: &VectorValue) -> f64 {
    vdist::inner_product_f64(&v.values, &v.values).sqrt()
}

pub(crate) fn eval_l2_norm(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "l2_norm")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let Value::Vector(v) = &args[0] else {
        return Err(DbError::internal("l2_norm() requires a vector argument"));
    };
    Ok(Value::Double(vector_l2_norm_f64(v)))
}

pub(crate) fn eval_l2_normalize(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "l2_normalize")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let Value::Vector(v) = &args[0] else {
        return Err(DbError::internal(
            "l2_normalize() requires a vector argument",
        ));
    };
    let norm = vector_l2_norm_f64(v);
    if norm == 0.0 {
        return Ok(Value::Vector(VectorValue::new(v.dims, v.values.clone())));
    }
    let values = v
        .values
        .iter()
        .map(|value| (f64::from(*value) / norm) as f32)
        .collect();
    Ok(Value::Vector(VectorValue::new(v.dims, values)))
}

pub(crate) fn eval_subvector(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "subvector")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let Value::Vector(v) = &args[0] else {
        return Err(DbError::internal("subvector() requires a vector argument"));
    };
    let start = value_to_i32(&args[1])?;
    let count = value_to_i32(&args[2])?;
    if start < 1 {
        return Err(DbError::internal(
            "subvector() start index must be greater than zero",
        ));
    }
    if count < 0 {
        return Err(DbError::internal("subvector() count must be non-negative"));
    }

    let start_idx = usize::try_from(start - 1).unwrap_or(usize::MAX);
    let count = usize::try_from(count).unwrap_or(usize::MAX);
    if start_idx >= v.values.len() || count == 0 {
        return Ok(Value::Vector(VectorValue::new(0, Vec::new())));
    }
    let end = start_idx.saturating_add(count).min(v.values.len());
    Ok(Value::Vector(VectorValue::new(
        u32::try_from(end - start_idx).unwrap_or(u32::MAX),
        v.values[start_idx..end].to_vec(),
    )))
}

pub(crate) fn eval_binary_quantize(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "binary_quantize")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let Value::Vector(v) = &args[0] else {
        return Err(DbError::internal(
            "binary_quantize() requires a vector argument",
        ));
    };
    let mut bits = String::with_capacity(v.values.len());
    for value in &v.values {
        bits.push(if *value > 0.0 { '1' } else { '0' });
    }
    Ok(Value::Text(bits))
}

fn extract_bitstring_pair<'a>(
    args: &'a [Value],
    function_name: &str,
) -> DbResult<(&'a str, &'a str)> {
    expect_args(args, 2, function_name)?;
    if args.iter().any(Value::is_null) {
        return Ok(("", ""));
    }
    let (Value::Text(left), Value::Text(right)) = (&args[0], &args[1]) else {
        return Err(DbError::internal(format!(
            "{function_name}() requires bitstring text arguments"
        )));
    };
    if left.len() != right.len() {
        return Err(DbError::internal(format!(
            "{function_name}(): bit strings must have the same length"
        )));
    }
    if !left.bytes().all(|byte| matches!(byte, b'0' | b'1'))
        || !right.bytes().all(|byte| matches!(byte, b'0' | b'1'))
    {
        return Err(DbError::invalid_input_syntax(
            "bit",
            &format!("{left}, {right}"),
        ));
    }
    Ok((left.as_str(), right.as_str()))
}

pub(crate) fn eval_hamming_distance(args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let (left, right) = extract_bitstring_pair(args, "hamming_distance")?;
    let distance = left
        .bytes()
        .zip(right.bytes())
        .filter(|(left, right)| left != right)
        .count();
    Ok(Value::Double(distance as f64))
}

pub(crate) fn eval_jaccard_distance(args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let (left, right) = extract_bitstring_pair(args, "jaccard_distance")?;
    let mut intersection = 0usize;
    let mut union = 0usize;
    for (left, right) in left.bytes().zip(right.bytes()) {
        if left == b'1' || right == b'1' {
            union += 1;
            if left == b'1' && right == b'1' {
                intersection += 1;
            }
        }
    }
    if union == 0 {
        return Ok(Value::Double(0.0));
    }
    Ok(Value::Double(1.0 - (intersection as f64 / union as f64)))
}

pub(crate) fn eval_l2_distance(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "l2_distance")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    if matches!((&args[0], &args[1]), (Value::Vector(_), Value::Vector(_))) {
        return eval_vector_distance(args, "l2_distance", vdist::l2_distance_f64);
    }
    let left_text = super::value_to_text(&args[0]);
    let right_text = super::value_to_text(&args[1]);
    if let (Ok(a), Ok(b)) = (parse_point_text(&left_text), parse_point_text(&right_text)) {
        let dx = a.x - b.x;
        let dy = a.y - b.y;
        return Ok(Value::Double((dx * dx + dy * dy).sqrt()));
    }
    if let (Ok(a), Ok(b)) = (
        parse_circle_text(&left_text),
        parse_circle_text(&right_text),
    ) {
        let dx = a.center.x - b.center.x;
        let dy = a.center.y - b.center.y;
        let center_distance = (dx * dx + dy * dy).sqrt();
        return Ok(Value::Double(
            (center_distance - (a.radius + b.radius)).max(0.0),
        ));
    }
    if let (Ok(point), Ok((a, b, c))) = (
        parse_point_text(&left_text),
        parse_line_coefficients(&right_text),
    ) {
        let denom = (a * a + b * b).sqrt();
        return Ok(Value::Double(
            ((a * point.x + b * point.y + c).abs()) / denom,
        ));
    }
    if let (Ok((a, b, c)), Ok(point)) = (
        parse_line_coefficients(&left_text),
        parse_point_text(&right_text),
    ) {
        let denom = (a * a + b * b).sqrt();
        return Ok(Value::Double(
            ((a * point.x + b * point.y + c).abs()) / denom,
        ));
    }
    if let (Ok((a1, b1, c1)), Ok((a2, b2, c2))) = (
        parse_line_coefficients(&left_text),
        parse_line_coefficients(&right_text),
    ) {
        let det = a1 * b2 - a2 * b1;
        if det.abs() > 1e-12 {
            return Ok(Value::Double(0.0));
        }
        let norm1 = (a1 * a1 + b1 * b1).sqrt();
        let norm2 = (a2 * a2 + b2 * b2).sqrt();
        let mut na1 = a1 / norm1;
        let mut nb1 = b1 / norm1;
        let mut nc1 = c1 / norm1;
        let mut na2 = a2 / norm2;
        let mut nb2 = b2 / norm2;
        let mut nc2 = c2 / norm2;
        if na1 < 0.0 || (na1 == 0.0 && nb1 < 0.0) {
            na1 = -na1;
            nb1 = -nb1;
            nc1 = -nc1;
        }
        if na2 < 0.0 || (na2 == 0.0 && nb2 < 0.0) {
            na2 = -na2;
            nb2 = -nb2;
            nc2 = -nc2;
        }
        if (na1 - na2).abs() < 1e-10 && (nb1 - nb2).abs() < 1e-10 {
            return Ok(Value::Double((nc1 - nc2).abs()));
        }
        return Ok(Value::Double(0.0));
    }
    if let (Ok((p1, p2)), Ok((a, b, c))) = (
        parse_lseg_text(&left_text),
        parse_line_coefficients(&right_text),
    ) {
        let denom = (a * a + b * b).sqrt();
        let d1_raw = a * p1.x + b * p1.y + c;
        let d2_raw = a * p2.x + b * p2.y + c;
        if d1_raw == 0.0 || d2_raw == 0.0 || d1_raw.signum() != d2_raw.signum() {
            return Ok(Value::Double(0.0));
        }
        return Ok(Value::Double((d1_raw.abs().min(d2_raw.abs())) / denom));
    }
    if let (Ok((a, b, c)), Ok((p1, p2))) = (
        parse_line_coefficients(&left_text),
        parse_lseg_text(&right_text),
    ) {
        let denom = (a * a + b * b).sqrt();
        let d1_raw = a * p1.x + b * p1.y + c;
        let d2_raw = a * p2.x + b * p2.y + c;
        if d1_raw == 0.0 || d2_raw == 0.0 || d1_raw.signum() != d2_raw.signum() {
            return Ok(Value::Double(0.0));
        }
        return Ok(Value::Double((d1_raw.abs().min(d2_raw.abs())) / denom));
    }
    if let (Ok(bounds), Ok(point)) = (
        parse_geometry_bounds(&left_text),
        parse_point_text(&right_text),
    ) {
        let dx = if point.x < bounds.xmin {
            bounds.xmin - point.x
        } else if point.x > bounds.xmax {
            point.x - bounds.xmax
        } else {
            0.0
        };
        let dy = if point.y < bounds.ymin {
            bounds.ymin - point.y
        } else if point.y > bounds.ymax {
            point.y - bounds.ymax
        } else {
            0.0
        };
        return Ok(Value::Double((dx * dx + dy * dy).sqrt()));
    }
    if let (Ok(point), Ok(bounds)) = (
        parse_point_text(&left_text),
        parse_geometry_bounds(&right_text),
    ) {
        let dx = if point.x < bounds.xmin {
            bounds.xmin - point.x
        } else if point.x > bounds.xmax {
            point.x - bounds.xmax
        } else {
            0.0
        };
        let dy = if point.y < bounds.ymin {
            bounds.ymin - point.y
        } else if point.y > bounds.ymax {
            point.y - bounds.ymax
        } else {
            0.0
        };
        return Ok(Value::Double((dx * dx + dy * dy).sqrt()));
    }
    if let (Ok(left_bounds), Ok(right_bounds)) = (
        parse_geometry_bounds(&left_text),
        parse_geometry_bounds(&right_text),
    ) {
        let dx = if left_bounds.xmax < right_bounds.xmin {
            right_bounds.xmin - left_bounds.xmax
        } else if right_bounds.xmax < left_bounds.xmin {
            left_bounds.xmin - right_bounds.xmax
        } else {
            0.0
        };
        let dy = if left_bounds.ymax < right_bounds.ymin {
            right_bounds.ymin - left_bounds.ymax
        } else if right_bounds.ymax < left_bounds.ymin {
            left_bounds.ymin - right_bounds.ymax
        } else {
            0.0
        };
        return Ok(Value::Double((dx * dx + dy * dy).sqrt()));
    }
    eval_vector_distance(args, "l2_distance", vdist::l2_distance_f64)
}

pub(crate) fn eval_cosine_distance(args: &[Value]) -> DbResult<Value> {
    eval_vector_distance(args, "cosine_distance", vdist::cosine_distance_f64)
}

pub(crate) fn eval_inner_product(args: &[Value]) -> DbResult<Value> {
    eval_vector_distance(args, "inner_product", vdist::inner_product_f64)
}

pub(crate) fn eval_manhattan_distance(args: &[Value]) -> DbResult<Value> {
    eval_vector_distance(args, "manhattan_distance", vdist::manhattan_distance_f64)
}

pub(crate) fn eval_negative_inner_product(args: &[Value]) -> DbResult<Value> {
    eval_vector_distance(args, "negative_inner_product", |a, b| {
        -vdist::inner_product_f64(a, b)
    })
}

// =====================================================================
// Array function helpers
// =====================================================================

pub(crate) fn eval_array_length(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 1, 2, "array_length() requires 1 or 2 arguments")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let dim = if args.len() == 2 {
        match &args[1] {
            Value::Int(d) => *d,
            Value::Null => return Ok(Value::Null),
            _ => {
                return Err(DbError::internal(
                    "array_length() dimension must be an integer",
                ));
            }
        }
    } else {
        1
    };
    let elements = match &args[0] {
        Value::Array(elements) => Cow::Borrowed(elements.as_slice()),
        Value::Text(_) => coerce_to_array(&args[0]).unwrap_or_else(|| Cow::Owned(Vec::new())),
        _ => {
            return Err(DbError::internal(
                "array_length() requires an array argument",
            ));
        }
    };
    if elements.is_empty() {
        return Ok(Value::Null);
    }
    match dim.cmp(&1) {
        std::cmp::Ordering::Equal => Ok(Value::Int(to_i32_saturating(elements.len()))),
        std::cmp::Ordering::Greater => {
            // Traverse into nested arrays for the requested dimension
            let mut cur = &elements[..];
            for _ in 1..dim {
                match cur.first() {
                    Some(Value::Array(inner)) => cur = inner,
                    _ => return Ok(Value::Null),
                }
            }
            Ok(Value::Int(to_i32_saturating(cur.len())))
        }
        std::cmp::Ordering::Less => Ok(Value::Null),
    }
}

pub(crate) fn eval_array_upper(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "array_upper")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let dim = match &args[1] {
        Value::Int(d) => *d,
        _ => {
            return Err(DbError::internal(
                "array_upper() dimension must be an integer",
            ));
        }
    };
    let Some((bounds, elements)) = coerce_to_array_with_bounds(&args[0]) else {
        return Err(DbError::internal(
            "array_upper() requires an array argument",
        ));
    };
    if dim != 1 || elements.is_empty() {
        Ok(Value::Null)
    } else {
        let upper = bounds
            .first()
            .map_or(i64::try_from(elements.len()).unwrap_or(0), |(_, upper)| {
                *upper
            });
        Ok(Value::Int(to_i32_saturating(upper)))
    }
}

pub(crate) fn eval_array_lower(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "array_lower")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let dim = match &args[1] {
        Value::Int(d) => *d,
        _ => {
            return Err(DbError::internal(
                "array_lower() dimension must be an integer",
            ));
        }
    };
    let Some((bounds, elements)) = coerce_to_array_with_bounds(&args[0]) else {
        return Err(DbError::internal(
            "array_lower() requires an array argument",
        ));
    };
    if dim != 1 || elements.is_empty() {
        Ok(Value::Null)
    } else {
        let lower = bounds.first().map_or(1, |(lower, _)| *lower);
        Ok(Value::Int(i32::try_from(lower).unwrap_or(i32::MIN)))
    }
}

pub(crate) fn eval_array_position(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 2, 3, "array_position() requires 2 or 3 arguments")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let (bounds, elements) = coerce_to_array_with_bounds(&args[0])
        .ok_or_else(|| DbError::internal("array_position() requires an array argument"))?;
    reject_multidimensional_array_search(&bounds, &elements)?;
    let base_index = bounds.first().map_or(1, |(lower, _)| *lower);
    let needle = &args[1];
    let start_index = if let Some(start) = args.get(2) {
        match start {
            Value::Null => return Ok(Value::Null),
            Value::Int(value) => i64::from(*value),
            Value::BigInt(value) => *value,
            _ => {
                return Err(DbError::internal(
                    "array_position() start position must be an integer",
                ));
            }
        }
    } else {
        base_index
    };
    let start_offset =
        usize::try_from(start_index.max(base_index) - base_index).unwrap_or(usize::MAX);
    for (i, elem) in elements.iter().enumerate().skip(start_offset) {
        if array_search_matches(elem, needle)? {
            let offset = i64::try_from(i).unwrap_or(i64::MAX);
            return Ok(Value::Int(to_i32_saturating(
                base_index.saturating_add(offset),
            )));
        }
    }
    Ok(Value::Null)
}

pub(crate) fn eval_array_remove(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "array_remove")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let (bounds, elements) = coerce_to_array_with_bounds(&args[0])
        .ok_or_else(|| DbError::internal("array_remove() requires an array argument"))?;
    if bounds.len() > 1
        || elements
            .iter()
            .any(|element| matches!(element, Value::Array(_)))
    {
        return Err(DbError::feature_not_supported(
            "removing elements from multidimensional arrays is not supported",
        ));
    }
    let target = &args[1];
    // Pre-size the keep-buffer to the input length: at most we keep
    // every element, so this costs one alloc instead of growth-doubling.
    let mut result: Vec<Value> = Vec::with_capacity(elements.len());
    for elem in elements.iter() {
        let should_remove = if target.is_null() {
            elem.is_null()
        } else {
            values_equal(elem, target)? == Some(true)
        };
        if !should_remove {
            result.push(elem.clone());
        }
    }
    Ok(array_result_with_bounds(&bounds, result))
}

pub(crate) fn eval_array_cat(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "array_cat")?;
    if args[0].is_null() && args[1].is_null() {
        return Ok(Value::Null);
    }
    let coerce = |value: &Value| -> DbResult<Vec<Value>> {
        match value {
            Value::Null => Ok(Vec::new()),
            Value::Array(v) => Ok(v.clone()),
            _ => Err(DbError::internal("array_cat() requires array arguments")),
        }
    };
    concatenate_array_values(coerce(&args[0])?, coerce(&args[1])?)
}

pub(crate) fn eval_array_append(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "array_append")?;
    if args[0].is_null() {
        return Ok(Value::Array(vec![args[1].clone()]));
    }
    let Value::Array(elements) = &args[0] else {
        return Err(DbError::internal(
            "array_append() first argument must be an array",
        ));
    };
    // Pre-size to elements.len()+1 so the trailing push doesn't
    // trigger a doubling reallocation; the legacy `elements.clone()`
    // gave a Vec with capacity == elements.len() exactly.
    let mut result: Vec<Value> = Vec::with_capacity(elements.len() + 1);
    result.extend(elements.iter().cloned());
    result.push(args[1].clone());
    Ok(Value::Array(result))
}

pub(crate) fn eval_array_prepend(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "array_prepend")?;
    if args[1].is_null() {
        return Ok(Value::Array(vec![args[0].clone()]));
    }
    let Value::Array(elements) = &args[1] else {
        return Err(DbError::internal(
            "array_prepend() second argument must be an array",
        ));
    };
    // Pre-size: 1 leading element + extend of existing array.
    let mut result: Vec<Value> = Vec::with_capacity(elements.len() + 1);
    result.push(args[0].clone());
    result.extend(elements.iter().cloned());
    Ok(Value::Array(result))
}

pub(crate) fn eval_array_to_string(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 2, 3, "array_to_string() requires 2 or 3 arguments")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let elements = coerce_to_array(&args[0])
        .ok_or_else(|| DbError::internal("array_to_string() first argument must be an array"))?;
    let delimiter = match &args[1] {
        Value::Text(s) => s.as_str(),
        Value::Null => return Ok(Value::Null),
        _ => {
            return Err(DbError::internal(
                "array_to_string() delimiter must be text",
            ));
        }
    };
    let null_string = if args.len() == 3 {
        match &args[2] {
            Value::Text(s) => Some(s.as_str()),
            Value::Null => None,
            _ => {
                return Err(DbError::internal(
                    "array_to_string() null_string must be text",
                ));
            }
        }
    } else {
        None
    };
    // Build the joined output in a single buffer instead of paying
    // N+1 heap allocations (Vec<String> of formatted parts plus the
    // join() output). Each non-null element still allocates one
    // String through `value_to_display_string`, but the surface
    // overhead collapses to a single output.
    let mut out = String::new();
    let mut first = true;
    for v in elements.iter() {
        let part: Option<std::borrow::Cow<'_, str>> = match v {
            Value::Null => null_string.map(std::borrow::Cow::Borrowed),
            // Text values borrow into the output buffer - no clone.
            Value::Text(s) => Some(std::borrow::Cow::Borrowed(s.as_str())),
            other => Some(std::borrow::Cow::Owned(value_to_display_string(other))),
        };
        let Some(part) = part else { continue };
        if !first {
            out.push_str(delimiter);
        }
        first = false;
        out.push_str(part.as_ref());
    }
    Ok(Value::Text(out))
}

pub(crate) fn eval_cardinality(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "cardinality")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    match &args[0] {
        Value::Array(elements) => {
            // For multi-dimensional, cardinality counts total elements
            fn count_elements(arr: &[Value], depth: usize) -> i32 {
                if depth >= MAX_ARRAY_VALUE_RECURSION_DEPTH {
                    return 0;
                }
                let mut total: i32 = 0;
                for v in arr {
                    match v {
                        Value::Array(inner) => {
                            total = total.saturating_add(count_elements(inner, depth + 1))
                        }
                        _ => total = total.saturating_add(1),
                    }
                }
                total
            }
            Ok(Value::Int(count_elements(elements, 0)))
        }
        Value::Text(_) => {
            if let Some(arr) = coerce_to_array(&args[0]) {
                Ok(Value::Int(to_i32_saturating(arr.len())))
            } else {
                Err(DbError::internal(
                    "cardinality() requires an array argument",
                ))
            }
        }
        _ => Err(DbError::internal(
            "cardinality() requires an array argument",
        )),
    }
}

pub(crate) fn eval_string_to_array(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 2, 3, "string_to_array() requires 2 or 3 arguments")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let s = match &args[0] {
        Value::Text(s) => s.as_str(),
        _ => {
            return Err(DbError::internal(
                "string_to_array() first argument must be text",
            ));
        }
    };
    // Second arg: delimiter (NULL means split each character)
    let delimiter = match &args[1] {
        Value::Null => None,
        Value::Text(d) => Some(d.as_str()),
        _ => {
            return Err(DbError::internal(
                "string_to_array() delimiter must be text",
            ));
        }
    };
    // Third arg: null_string (optional, elements equal to this become NULL)
    let null_string = if args.len() == 3 {
        match &args[2] {
            Value::Null => None,
            Value::Text(n) => Some(n.as_str()),
            _ => {
                return Err(DbError::internal(
                    "string_to_array() null_string must be text",
                ));
            }
        }
    } else {
        None
    };

    let parts: Vec<Value> = match delimiter {
        Some(delim) => {
            if delim.is_empty() {
                // Empty delimiter: PG returns the whole string as a single element
                // (unless string is empty, in which case PG returns empty array)
                if s.is_empty() {
                    Vec::new()
                } else {
                    vec![match null_string {
                        Some(ns) if s == ns => Value::Null,
                        _ => Value::Text(s.to_string()),
                    }]
                }
            } else {
                if s.is_empty() {
                    Vec::new()
                } else {
                    s.split(delim)
                        .map(|part| match null_string {
                            Some(ns) if part == ns => Value::Null,
                            _ => Value::Text(part.to_string()),
                        })
                        .collect()
                }
            }
        }
        None => {
            // NULL delimiter: each character becomes an element
            s.chars().map(|ch| Value::Text(ch.to_string())).collect()
        }
    };
    Ok(Value::Array(parts))
}

pub(crate) fn eval_string_to_table(args: &[Value]) -> DbResult<Value> {
    if args.first().is_some_and(Value::is_null) {
        return Ok(Value::Array(Vec::new()));
    }
    eval_string_to_array(args)
}

pub(crate) fn eval_array_dims(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "array_dims")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let Some((bounds, elements)) = coerce_to_array_with_bounds(&args[0]) else {
        return Err(DbError::internal("array_dims() requires an array argument"));
    };
    if elements.is_empty() {
        return Ok(Value::Null);
    }
    // Build dimension string by traversing nested arrays. Each
    // bracketed dimension was previously assembled via
    // `push_str(&format!(...))` which allocated a transient String.
    // Stream directly into `dims` via `write!`.
    use std::fmt::Write;
    let mut dims = String::new();
    if bounds.is_empty() {
        let _ = write!(dims, "[1:{}]", elements.len());
    } else {
        for (lower, upper) in &bounds {
            let _ = write!(dims, "[{lower}:{upper}]");
        }
    }
    let mut cur = &elements[0];
    while let Value::Array(inner) = cur {
        if inner.is_empty() {
            break;
        }
        if bounds.is_empty() {
            let _ = write!(dims, "[1:{}]", inner.len());
        }
        cur = &inner[0];
    }
    Ok(Value::Text(dims))
}

pub(crate) fn eval_array_ndims(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "array_ndims")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let (bounds, elements) = coerce_to_array_with_bounds(&args[0])
        .ok_or_else(|| DbError::internal("array_ndims() requires an array argument"))?;
    if elements.is_empty() {
        return Ok(Value::Null);
    }
    if !bounds.is_empty() {
        return Ok(Value::Int(to_i32_saturating(bounds.len())));
    }
    let mut ndims = 1;
    let mut cur = &elements[0];
    while let Value::Array(inner) = cur {
        if inner.is_empty() {
            break;
        }
        ndims += 1;
        cur = &inner[0];
    }
    Ok(Value::Int(ndims))
}

pub(crate) fn eval_array_positions(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "array_positions")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let (bounds, elements) = coerce_to_array_with_bounds(&args[0])
        .ok_or_else(|| DbError::internal("array_positions() requires an array argument"))?;
    reject_multidimensional_array_search(&bounds, &elements)?;
    let base_index = bounds.first().map_or(1, |(lower, _)| *lower);
    let needle = &args[1];
    let mut positions = Vec::new();
    for (i, elem) in elements.iter().enumerate() {
        if array_search_matches(elem, needle)? {
            let offset = i64::try_from(i).unwrap_or(i64::MAX);
            positions.push(Value::Int(to_i32_saturating(
                base_index.saturating_add(offset),
            )));
        }
    }
    Ok(Value::Array(positions))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QuantifiedArrayQuantifier {
    Any,
    All,
}

#[derive(Clone, Copy)]
enum QuantifiedArrayOp {
    Eq,
    Ne,
    Ge,
    Gt,
    Le,
    Lt,
}

pub(crate) fn eval_quantified_comparison(name: &str, args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, name)?;
    let Some((quantifier, op)) = parse_quantified_comparison_name(name) else {
        return Err(DbError::internal(format!(
            "unknown quantified comparison function: {name}"
        )));
    };
    if args[1].is_null() {
        return Ok(Value::Null);
    }
    let elements = coerce_to_array(&args[1]).ok_or_else(|| {
        DbError::internal(format!(
            "{name}() second argument must evaluate to an array"
        ))
    })?;
    if elements.is_empty() {
        return Ok(Value::Boolean(matches!(
            quantifier,
            QuantifiedArrayQuantifier::All
        )));
    }

    let mut saw_null = false;
    for element in elements.iter() {
        match eval_quantified_element(op, &args[0], element)? {
            Value::Boolean(true) if quantifier == QuantifiedArrayQuantifier::Any => {
                return Ok(Value::Boolean(true));
            }
            Value::Boolean(false) if quantifier == QuantifiedArrayQuantifier::All => {
                return Ok(Value::Boolean(false));
            }
            Value::Boolean(_) => {}
            Value::Null => saw_null = true,
            other => {
                return Err(DbError::internal(format!(
                    "{name}() comparison produced non-boolean value: {other:?}"
                )));
            }
        }
    }

    if saw_null {
        Ok(Value::Null)
    } else {
        Ok(Value::Boolean(matches!(
            quantifier,
            QuantifiedArrayQuantifier::All
        )))
    }
}

pub(crate) fn eval_quantified_like(name: &str, args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, name)?;
    let Some((quantifier, negated, case_insensitive)) = parse_quantified_like_name(name) else {
        return Err(DbError::internal(format!(
            "unknown quantified LIKE function: {name}"
        )));
    };
    if args[1].is_null() {
        return Ok(Value::Null);
    }
    let elements = coerce_to_array(&args[1]).ok_or_else(|| {
        DbError::internal(format!(
            "{name}() second argument must evaluate to an array"
        ))
    })?;
    if elements.is_empty() {
        return Ok(Value::Boolean(matches!(
            quantifier,
            QuantifiedArrayQuantifier::All
        )));
    }

    let mut saw_null = false;
    for element in elements.iter() {
        match eval_like(&args[0], element, negated, case_insensitive)? {
            Value::Boolean(true) if quantifier == QuantifiedArrayQuantifier::Any => {
                return Ok(Value::Boolean(true));
            }
            Value::Boolean(false) if quantifier == QuantifiedArrayQuantifier::All => {
                return Ok(Value::Boolean(false));
            }
            Value::Boolean(_) => {}
            Value::Null => saw_null = true,
            other => {
                return Err(DbError::internal(format!(
                    "{name}() comparison produced non-boolean value: {other:?}"
                )));
            }
        }
    }

    if saw_null {
        Ok(Value::Null)
    } else {
        Ok(Value::Boolean(matches!(
            quantifier,
            QuantifiedArrayQuantifier::All
        )))
    }
}

fn parse_quantified_comparison_name(
    name: &str,
) -> Option<(QuantifiedArrayQuantifier, QuantifiedArrayOp)> {
    match name {
        "__aiondb_quantified_any_eq" => {
            Some((QuantifiedArrayQuantifier::Any, QuantifiedArrayOp::Eq))
        }
        "__aiondb_quantified_any_ne" => {
            Some((QuantifiedArrayQuantifier::Any, QuantifiedArrayOp::Ne))
        }
        "__aiondb_quantified_any_ge" => {
            Some((QuantifiedArrayQuantifier::Any, QuantifiedArrayOp::Ge))
        }
        "__aiondb_quantified_any_gt" => {
            Some((QuantifiedArrayQuantifier::Any, QuantifiedArrayOp::Gt))
        }
        "__aiondb_quantified_any_le" => {
            Some((QuantifiedArrayQuantifier::Any, QuantifiedArrayOp::Le))
        }
        "__aiondb_quantified_any_lt" => {
            Some((QuantifiedArrayQuantifier::Any, QuantifiedArrayOp::Lt))
        }
        "__aiondb_quantified_all_eq" => {
            Some((QuantifiedArrayQuantifier::All, QuantifiedArrayOp::Eq))
        }
        "__aiondb_quantified_all_ne" => {
            Some((QuantifiedArrayQuantifier::All, QuantifiedArrayOp::Ne))
        }
        "__aiondb_quantified_all_ge" => {
            Some((QuantifiedArrayQuantifier::All, QuantifiedArrayOp::Ge))
        }
        "__aiondb_quantified_all_gt" => {
            Some((QuantifiedArrayQuantifier::All, QuantifiedArrayOp::Gt))
        }
        "__aiondb_quantified_all_le" => {
            Some((QuantifiedArrayQuantifier::All, QuantifiedArrayOp::Le))
        }
        "__aiondb_quantified_all_lt" => {
            Some((QuantifiedArrayQuantifier::All, QuantifiedArrayOp::Lt))
        }
        _ => None,
    }
}

fn parse_quantified_like_name(name: &str) -> Option<(QuantifiedArrayQuantifier, bool, bool)> {
    match name {
        "__aiondb_quantified_any_like" => Some((QuantifiedArrayQuantifier::Any, false, false)),
        "__aiondb_quantified_all_like" => Some((QuantifiedArrayQuantifier::All, false, false)),
        "__aiondb_quantified_any_not_like" => Some((QuantifiedArrayQuantifier::Any, true, false)),
        "__aiondb_quantified_all_not_like" => Some((QuantifiedArrayQuantifier::All, true, false)),
        "__aiondb_quantified_any_ilike" => Some((QuantifiedArrayQuantifier::Any, false, true)),
        "__aiondb_quantified_all_ilike" => Some((QuantifiedArrayQuantifier::All, false, true)),
        "__aiondb_quantified_any_not_ilike" => Some((QuantifiedArrayQuantifier::Any, true, true)),
        "__aiondb_quantified_all_not_ilike" => Some((QuantifiedArrayQuantifier::All, true, true)),
        _ => None,
    }
}

fn eval_quantified_element(op: QuantifiedArrayOp, left: &Value, right: &Value) -> DbResult<Value> {
    match op {
        QuantifiedArrayOp::Eq => eval_equality_comparison(left, right, false),
        QuantifiedArrayOp::Ne => eval_equality_comparison(left, right, true),
        QuantifiedArrayOp::Ge => eval_ordering_comparison(left, right, |ordering| ordering.is_ge()),
        QuantifiedArrayOp::Gt => eval_ordering_comparison(left, right, |ordering| ordering.is_gt()),
        QuantifiedArrayOp::Le => eval_ordering_comparison(left, right, |ordering| ordering.is_le()),
        QuantifiedArrayOp::Lt => eval_ordering_comparison(left, right, |ordering| ordering.is_lt()),
    }
}

pub(crate) fn eval_quantified_regex(name: &str, args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, name)?;
    let Some((quantifier, case_insensitive, negate)) = parse_quantified_regex_name(name) else {
        return Err(DbError::internal(format!(
            "unknown quantified regex function: {name}"
        )));
    };
    if args[1].is_null() {
        return Ok(Value::Null);
    }
    let elements = coerce_to_array(&args[1]).ok_or_else(|| {
        DbError::internal(format!(
            "{name}() second argument must evaluate to an array"
        ))
    })?;
    if elements.is_empty() {
        return Ok(Value::Boolean(matches!(
            quantifier,
            QuantifiedArrayQuantifier::All
        )));
    }

    let mut saw_null = false;
    for element in elements.iter() {
        match eval_regex_element(&args[0], element, case_insensitive, negate) {
            Value::Boolean(true) if quantifier == QuantifiedArrayQuantifier::Any => {
                return Ok(Value::Boolean(true));
            }
            Value::Boolean(false) if quantifier == QuantifiedArrayQuantifier::All => {
                return Ok(Value::Boolean(false));
            }
            Value::Boolean(_) => {}
            Value::Null => saw_null = true,
            other => {
                return Err(DbError::internal(format!(
                    "{name}() comparison produced non-boolean value: {other:?}"
                )));
            }
        }
    }

    if saw_null {
        Ok(Value::Null)
    } else {
        Ok(Value::Boolean(matches!(
            quantifier,
            QuantifiedArrayQuantifier::All
        )))
    }
}

fn parse_quantified_regex_name(name: &str) -> Option<(QuantifiedArrayQuantifier, bool, bool)> {
    // Returns (quantifier, case_insensitive, negate)
    match name {
        "__aiondb_quantified_any_regex_match" => {
            Some((QuantifiedArrayQuantifier::Any, false, false))
        }
        "__aiondb_quantified_all_regex_match" => {
            Some((QuantifiedArrayQuantifier::All, false, false))
        }
        "__aiondb_quantified_any_regex_match_ci" => {
            Some((QuantifiedArrayQuantifier::Any, true, false))
        }
        "__aiondb_quantified_all_regex_match_ci" => {
            Some((QuantifiedArrayQuantifier::All, true, false))
        }
        "__aiondb_quantified_any_not_regex_match" => {
            Some((QuantifiedArrayQuantifier::Any, false, true))
        }
        "__aiondb_quantified_all_not_regex_match" => {
            Some((QuantifiedArrayQuantifier::All, false, true))
        }
        "__aiondb_quantified_any_not_regex_match_ci" => {
            Some((QuantifiedArrayQuantifier::Any, true, true))
        }
        "__aiondb_quantified_all_not_regex_match_ci" => {
            Some((QuantifiedArrayQuantifier::All, true, true))
        }
        _ => None,
    }
}

fn eval_regex_element(
    text: &Value,
    pattern: &Value,
    case_insensitive: bool,
    negate: bool,
) -> Value {
    match (text, pattern) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::Text(text), Value::Text(pattern)) => {
            // Routed through the per-thread regex cache: `WHERE arr_col ~
            // 'pat'` over a quantified array no longer recompiles the
            // pattern on every element.
            match crate::regex_cache::get_ci(pattern, case_insensitive) {
                Ok(re) => Value::Boolean(re.is_match(text) ^ negate),
                Err(_) => Value::Boolean(false ^ negate),
            }
        }
        _ => {
            let text_str = text.to_string();
            let pattern_str = pattern.to_string();
            match crate::regex_cache::get_ci(&pattern_str, case_insensitive) {
                Ok(re) => Value::Boolean(re.is_match(&text_str) ^ negate),
                Err(_) => Value::Boolean(false ^ negate),
            }
        }
    }
}

pub(crate) fn eval_array_replace(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "array_replace")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let Value::Array(elements) = &args[0] else {
        return Err(DbError::internal(
            "array_replace() requires an array argument",
        ));
    };
    let search = &args[1];
    let replacement = &args[2];
    // array_replace produces the same number of elements as the input,
    // so pre-size to avoid growth allocations.
    let mut result: Vec<Value> = Vec::with_capacity(elements.len());
    for elem in elements {
        if search.is_null() {
            if elem.is_null() {
                result.push(replacement.clone());
            } else {
                result.push(elem.clone());
            }
        } else if values_equal(elem, search)? == Some(true) {
            result.push(replacement.clone());
        } else {
            result.push(elem.clone());
        }
    }
    Ok(Value::Array(result))
}

pub(crate) fn eval_array_fill(args: &[Value]) -> DbResult<Value> {
    expect_arg_range(args, 2, 3, "array_fill() requires 2 or 3 arguments")?;
    if args[1].is_null() || args.get(2).is_some_and(Value::is_null) {
        return Err(DbError::internal(
            "dimension array or low bound array cannot be null",
        ));
    }
    let fill_val = &args[0];

    let dim_arr = parse_array_fill_vector(
        &args[1],
        "array_fill() second argument must be an array of dimensions",
        "Dimension array must be one dimensional.",
    )?;
    let dims: Vec<usize> = dim_arr
        .iter()
        .map(array_fill_dimension_value)
        .collect::<DbResult<Vec<usize>>>()?;
    if dims.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    if dims.len() > MAX_ARRAY_FILL_DIMENSIONS {
        return Err(DbError::program_limit(format!(
            "array_fill() dimensions exceed limit {MAX_ARRAY_FILL_DIMENSIONS}"
        )));
    }

    let lower_bounds = if let Some(low_bounds) = args.get(2) {
        let lower_bound_arr = parse_array_fill_vector(
            low_bounds,
            "array_fill() third argument must be an array of lower bounds",
            "Low bound array must be one dimensional.",
        )?;
        if lower_bound_arr.len() != dims.len() {
            return Err(DbError::internal("wrong number of array subscripts")
                .with_client_detail("Low bound array has different size than dimensions array."));
        }
        Some(
            lower_bound_arr
                .iter()
                .map(array_fill_lower_bound_value)
                .collect::<DbResult<Vec<i64>>>()?,
        )
    } else {
        None
    };

    // Build the multi-dimensional array
    fn build_fill(fill_val: &Value, dims: &[usize], depth: usize) -> DbResult<Value> {
        if depth >= MAX_ARRAY_FILL_DIMENSIONS {
            return Err(DbError::program_limit(format!(
                "array_fill() dimensions exceed limit {MAX_ARRAY_FILL_DIMENSIONS}"
            )));
        }
        if dims.len() == 1 {
            Ok(Value::Array(vec![fill_val.clone(); dims[0]]))
        } else {
            let inner = build_fill(fill_val, &dims[1..], depth + 1)?;
            Ok(Value::Array(vec![inner; dims[0]]))
        }
    }

    let elements = match build_fill(fill_val, &dims, 0)? {
        Value::Array(elements) => elements,
        other => return Ok(other),
    };

    if let Some(lower_bounds) = lower_bounds {
        let bounds = lower_bounds
            .into_iter()
            .zip(dims)
            .map(|(lower, len)| {
                let upper = lower + i64::try_from(len).unwrap_or(0) - 1;
                (lower, upper)
            })
            .collect::<Vec<_>>();
        Ok(array_result_with_bounds(&bounds, elements))
    } else {
        Ok(Value::Array(elements))
    }
}

fn parse_array_fill_vector<'a>(
    value: &'a Value,
    type_error: &str,
    dimensionality_detail: &str,
) -> DbResult<Cow<'a, [Value]>> {
    let (bounds, elements) =
        coerce_to_array_with_bounds(value).ok_or_else(|| DbError::internal(type_error))?;
    if bounds.len() > 1
        || elements
            .iter()
            .any(|element| matches!(element, Value::Array(_)))
    {
        return Err(DbError::internal("wrong number of array subscripts")
            .with_client_detail(dimensionality_detail));
    }
    Ok(elements)
}

fn array_fill_dimension_value(value: &Value) -> DbResult<usize> {
    match value {
        Value::Int(n) if *n >= 0 => usize::try_from(*n)
            .map_err(|_| DbError::program_limit("array_fill() dimension exceeds platform limits")),
        Value::BigInt(n) if *n >= 0 => Ok(usize::try_from(*n).map_err(|_| {
            DbError::program_limit("array_fill() dimension exceeds platform limits")
        })?),
        Value::Null => Err(DbError::internal("dimension values cannot be null")),
        Value::Int(_) | Value::BigInt(_) => Err(DbError::internal(
            "array_fill() dimensions must be greater than or equal to 0",
        )),
        _ => Err(DbError::internal(
            "array_fill() dimensions must be integers",
        )),
    }
}

fn array_fill_lower_bound_value(value: &Value) -> DbResult<i64> {
    match value {
        Value::Int(n) => Ok(i64::from(*n)),
        Value::BigInt(n) => Ok(*n),
        Value::Null => Err(DbError::internal("dimension values cannot be null")),
        _ => Err(DbError::internal(
            "array_fill() dimensions must be integers",
        )),
    }
}

pub(crate) fn eval_trim_array(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "trim_array")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let (bounds, elements) = coerce_to_array_with_bounds(&args[0])
        .ok_or_else(|| DbError::internal("trim_array() first argument must be an array"))?;
    let n = match &args[1] {
        Value::Int(n) => i64::from(*n),
        Value::BigInt(n) => *n,
        Value::Null => return Ok(Value::Null),
        _ => {
            return Err(DbError::internal(
                "trim_array() second argument must be an integer",
            ));
        }
    };
    let max = i64::try_from(elements.len()).unwrap_or(i64::MAX);
    if n < 0 || n > max {
        return Err(DbError::internal(format!(
            "number of elements to trim must be between 0 and {max}"
        )));
    }
    let trimmed = elements[..elements.len() - usize::try_from(n).unwrap_or(0)].to_vec();
    let _ = bounds;
    Ok(Value::Array(trimmed))
}

pub(crate) fn eval_array_sample(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "array_sample")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let (bounds, elements) = coerce_to_array_with_bounds(&args[0])
        .ok_or_else(|| DbError::internal("array_sample() first argument must be an array"))?;
    let n = match &args[1] {
        Value::Int(n) => i64::from(*n),
        Value::BigInt(n) => *n,
        Value::Null => return Ok(Value::Null),
        _ => {
            return Err(DbError::internal(
                "array_sample() second argument must be an integer",
            ));
        }
    };
    let max = i64::try_from(elements.len()).unwrap_or(i64::MAX);
    if n < 0 || n > max {
        return Err(DbError::internal(format!(
            "sample size must be between 0 and {max}"
        )));
    }
    let n = usize::try_from(n).unwrap_or(0);
    // Sample n elements randomly without replacement
    use std::collections::HashSet;
    if n >= elements.len() {
        // If n >= array length, return a shuffled copy of all elements
        let mut result = elements.into_owned();
        // Simple Fisher-Yates shuffle using system randomness
        for i in (1..result.len()).rev() {
            let j = usize::try_from(rand_u64()).unwrap_or(usize::MAX) % (i + 1);
            result.swap(i, j);
        }
        if bounds.is_empty() {
            return Ok(Value::Array(result));
        }
        let mut new_bounds = bounds;
        new_bounds[0] = (1, i64::try_from(result.len()).unwrap_or(0));
        return Ok(array_result_with_bounds(&new_bounds, result));
    }
    let mut chosen = HashSet::new();
    let mut result = Vec::with_capacity(n);
    while result.len() < n {
        let idx = usize::try_from(rand_u64()).unwrap_or(usize::MAX) % elements.len();
        if chosen.insert(idx) {
            result.push(elements[idx].clone());
        }
    }
    if bounds.is_empty() {
        Ok(Value::Array(result))
    } else {
        let mut new_bounds = bounds;
        new_bounds[0] = (1, i64::try_from(result.len()).unwrap_or(0));
        Ok(array_result_with_bounds(&new_bounds, result))
    }
}

pub(crate) fn eval_array_shuffle(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "array_shuffle")?;
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let (bounds, elements) = coerce_to_array_with_bounds(&args[0])
        .ok_or_else(|| DbError::internal("array_shuffle() first argument must be an array"))?;
    let mut result = elements.into_owned();
    // Fisher-Yates shuffle
    for i in (1..result.len()).rev() {
        let j = usize::try_from(rand_u64()).unwrap_or(usize::MAX) % (i + 1);
        result.swap(i, j);
    }
    if bounds.is_empty() {
        Ok(Value::Array(result))
    } else {
        Ok(array_result_with_bounds(&bounds, result))
    }
}

/// Simple random u64 using system entropy (no external dependency needed).
fn rand_u64() -> u64 {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seed = u64::try_from(seed).unwrap_or(u64::MAX);
    // xorshift64
    let mut x = seed ^ 0x517cc1b727220a95;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

/// Convert a `Value` to its display string for `array_to_string`.
fn value_to_display_string(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Int(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::Real(n) => n.to_string(),
        Value::Double(n) => n.to_string(),
        Value::Boolean(b) => if *b { "true" } else { "false" }.to_owned(),
        Value::Null => String::new(),
        other => format!("{other:?}"),
    }
}
