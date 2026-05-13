use super::geometric::{parse_box_text, parse_point_text};
use super::value_convert::i64_to_f64;
use super::*;
use std::borrow::Cow;

const MAX_ARRAY_ASSIGN_RESULT_LEN: usize = 1_000_000;
const MAX_ARRAY_SLICE_DEPTH: usize = 256;

#[derive(Clone, Debug)]
struct ArrayValue {
    lower_bound: i64,
    elements: Vec<Value>,
}

impl ArrayValue {
    fn len(&self) -> usize {
        self.elements.len()
    }

    fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    fn upper_bound(&self) -> Option<i64> {
        self.lower_bound
            .checked_add(i64::try_from(self.elements.len()).ok()?.checked_sub(1)?)
    }
}

pub(super) fn eval_array_get(args: &[Value]) -> Value {
    if args.len() < 2 {
        return Value::Null;
    }
    match &args[0] {
        Value::Null => Value::Null,
        Value::Jsonb(json) => {
            let extracted = match &args[1] {
                Value::Null => return Value::Null,
                Value::Int(index) => jsonb_subscript_by_index(json, i64::from(*index)),
                Value::BigInt(index) => jsonb_subscript_by_index(json, *index),
                Value::Text(key) => jsonb_subscript_by_key(json, key),
                _ => None,
            };
            extracted.map_or(Value::Null, Value::Jsonb)
        }
        Value::Array(_) | Value::Text(_) => {
            let idx = match &args[1] {
                Value::Null => return Value::Null,
                Value::Int(i) => i64::from(*i),
                Value::BigInt(i) => *i,
                _ => return Value::Null,
            };
            let Some(array) = coerce_array_value(&args[0]) else {
                return Value::Null;
            };
            let offset = idx - array.lower_bound;
            if offset >= 0 {
                let offset = usize::try_from(offset).unwrap_or(usize::MAX);
                if offset < array.elements.len() {
                    return array.elements[offset].clone();
                }
            }
            Value::Null
        }
        _ => Value::Null,
    }
}

pub(super) fn eval_array_slice(args: &[Value]) -> DbResult<Value> {
    if args.len() < 5 || !(args.len() - 1).is_multiple_of(4) {
        return Ok(Value::Null);
    }

    let slice_specs = match parse_array_slice_specs(args)? {
        ParsedArraySliceSpecs::Null => return Ok(Value::Null),
        ParsedArraySliceSpecs::Specs(specs) => specs,
    };

    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Array(_) | Value::Text(_) => {
            if let Some((bounds, elements)) = coerce_multidim_array_value(&args[0]) {
                Ok(slice_array_dimensions(
                    elements.as_ref(),
                    &bounds,
                    &slice_specs,
                ))
            } else {
                Ok(Value::Null)
            }
        }
        _ => Ok(Value::Null),
    }
}

pub(super) fn eval_array_assign(args: &[Value]) -> DbResult<Value> {
    let (steps, replacement) = parse_array_assign_args(args)?;
    assign_array_steps(args[0].clone(), &steps, replacement)
}

pub(super) fn eval_fixed_array_slice(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        return Ok(Value::Null);
    }
    Err(fixed_length_array_slice_error())
}

pub(super) fn eval_fixed_array_assign(args: &[Value]) -> DbResult<Value> {
    let (steps, replacement) = parse_array_assign_args(args)?;
    assign_fixed_array_steps(args[0].clone(), &steps, replacement)
}

fn parse_array_assign_args(args: &[Value]) -> DbResult<(Vec<ArrayAssignStep>, Value)> {
    if args.len() < 5 || args.len() % 3 != 2 {
        return Err(DbError::internal(
            "__aiondb_array_assign() received malformed arguments",
        ));
    }

    let mut steps = Vec::new();
    let mut i = 1;
    while i + 3 < args.len() {
        let mode = match &args[i] {
            Value::Text(mode) => mode.as_str(),
            Value::Null => {
                return Err(DbError::internal(
                    "__aiondb_array_assign() step mode cannot be null",
                ));
            }
            _ => {
                return Err(DbError::internal(
                    "__aiondb_array_assign() step mode must be text",
                ));
            }
        };
        match mode {
            "index" => {
                let index = parse_array_assign_index(&args[i + 1])?;
                steps.push(ArrayAssignStep::Index(index));
            }
            "slice" => {
                let lower = parse_optional_array_index(&args[i + 1])?;
                let upper = parse_optional_array_index(&args[i + 2])?;
                steps.push(ArrayAssignStep::Slice(lower, upper));
            }
            _ => {
                return Err(DbError::internal(format!(
                    "__aiondb_array_assign() received unknown mode '{mode}'"
                )));
            }
        }
        i += 3;
    }

    let replacement = args
        .last()
        .cloned()
        .ok_or_else(|| DbError::internal("__aiondb_array_assign() missing replacement value"))?;
    Ok((steps, replacement))
}

#[derive(Clone, Debug)]
enum ArrayAssignIndex {
    Integer(i64),
    Text(String),
}

#[derive(Clone, Debug)]
enum ArrayAssignStep {
    Index(ArrayAssignIndex),
    Slice(Option<i64>, Option<i64>),
}

enum ParsedArraySliceBound {
    Omitted,
    Null,
    Index(i64),
    Invalid,
}

#[derive(Clone, Copy)]
struct ArraySliceSpec {
    lower: Option<i64>,
    upper: Option<i64>,
    lower_omitted: bool,
    upper_omitted: bool,
}

enum ParsedArraySliceSpecs {
    Null,
    Specs(Vec<ArraySliceSpec>),
}

#[derive(Clone, Copy, Debug)]
struct FixedPointValue {
    x: f64,
    y: f64,
}

enum FixedArrayBase {
    Null,
    Point(FixedPointValue),
}

fn parse_array_slice_omitted_flag(value: &Value, bound_name: &str) -> DbResult<bool> {
    match value {
        Value::Boolean(flag) => Ok(*flag),
        Value::Null => Ok(false),
        _ => Err(DbError::internal(format!(
            "__aiondb_array_slice() {bound_name} omitted flag must be boolean"
        ))),
    }
}

fn parse_array_slice_bound(value: &Value, omitted: bool) -> ParsedArraySliceBound {
    if omitted {
        return ParsedArraySliceBound::Omitted;
    }

    match value {
        Value::Null => ParsedArraySliceBound::Null,
        Value::Int(i) => ParsedArraySliceBound::Index(i64::from(*i)),
        Value::BigInt(i) => ParsedArraySliceBound::Index(*i),
        _ => ParsedArraySliceBound::Invalid,
    }
}

fn parse_array_slice_specs(args: &[Value]) -> DbResult<ParsedArraySliceSpecs> {
    let mut specs = Vec::with_capacity((args.len() - 1) / 4);
    for chunk in args[1..].chunks_exact(4) {
        let lower_omitted = parse_array_slice_omitted_flag(&chunk[2], "lower")?;
        let upper_omitted = parse_array_slice_omitted_flag(&chunk[3], "upper")?;
        let lower = parse_array_slice_bound(&chunk[0], lower_omitted);
        let upper = parse_array_slice_bound(&chunk[1], upper_omitted);

        let (lower, upper) = match (lower, upper) {
            (ParsedArraySliceBound::Index(lower), ParsedArraySliceBound::Index(upper)) => {
                (Some(lower), Some(upper))
            }
            (ParsedArraySliceBound::Index(lower), ParsedArraySliceBound::Omitted) => {
                (Some(lower), None)
            }
            (ParsedArraySliceBound::Omitted, ParsedArraySliceBound::Index(upper)) => {
                (None, Some(upper))
            }
            (ParsedArraySliceBound::Omitted, ParsedArraySliceBound::Omitted) => (None, None),
            (ParsedArraySliceBound::Null, _) | (_, ParsedArraySliceBound::Null) => {
                return Ok(ParsedArraySliceSpecs::Null);
            }
            _ => return Ok(ParsedArraySliceSpecs::Null),
        };

        specs.push(ArraySliceSpec {
            lower,
            upper,
            lower_omitted,
            upper_omitted,
        });
    }
    Ok(ParsedArraySliceSpecs::Specs(specs))
}

fn slice_array_dimensions(
    elements: &[Value],
    explicit_bounds: &[(i64, i64)],
    specs: &[ArraySliceSpec],
) -> Value {
    slice_array_dimensions_inner(elements, explicit_bounds, specs, 0)
        .unwrap_or_else(|| Value::Array(Vec::new()))
}

fn slice_array_dimensions_inner(
    elements: &[Value],
    explicit_bounds: &[(i64, i64)],
    specs: &[ArraySliceSpec],
    depth: usize,
) -> Option<Value> {
    if depth >= MAX_ARRAY_SLICE_DEPTH {
        return None;
    }
    if elements.is_empty() {
        return Some(Value::Array(Vec::new()));
    }

    let spec = *specs.first()?;
    let (current_lower, current_upper) =
        current_dimension_bounds(elements.len(), explicit_bounds.first().copied())?;
    let start = if spec.lower_omitted {
        current_lower
    } else {
        spec.lower?
    }
    .max(current_lower);
    let end = if spec.upper_omitted {
        current_upper
    } else {
        spec.upper?
    }
    .min(current_upper);

    if end < start || end < current_lower || start > current_upper {
        return Some(Value::Array(Vec::new()));
    }

    let start_idx = usize::try_from(start - current_lower).ok()?;
    let end_idx = usize::try_from(end - current_lower + 1).ok()?;
    let selected = &elements[start_idx..end_idx];

    if specs.len() == 1 {
        return Some(Value::Array(selected.to_vec()));
    }

    let child_bounds = if explicit_bounds.len() > 1 {
        &explicit_bounds[1..]
    } else {
        &[]
    };
    let mut nested = Vec::with_capacity(selected.len());
    for element in selected {
        let (child_explicit_bounds, child_elements) =
            coerce_nested_multidim_array_value(element, child_bounds)?;
        let child_slice: &[Value] = &child_elements;
        let Some(child_value) = slice_array_dimensions_inner(
            child_slice,
            &child_explicit_bounds,
            &specs[1..],
            depth + 1,
        ) else {
            return Some(Value::Array(Vec::new()));
        };
        nested.push(child_value);
    }

    Some(Value::Array(nested))
}

fn current_dimension_bounds(len: usize, explicit: Option<(i64, i64)>) -> Option<(i64, i64)> {
    if let Some(bounds) = explicit {
        Some(bounds)
    } else if len == 0 {
        None
    } else {
        Some((1, i64::try_from(len).ok()?))
    }
}

fn coerce_multidim_array_value(value: &Value) -> Option<(Vec<(i64, i64)>, Cow<'_, [Value]>)> {
    match value {
        Value::Array(array) => Some((Vec::new(), Cow::Borrowed(array.as_slice()))),
        Value::Text(text) => {
            let (bounds, body) = split_multidim_array_bound_prefix(text)?;
            let parsed = parse_pg_array_literal_simple_with_bounds(body)?;
            Some((bounds, Cow::Owned(parsed.elements)))
        }
        _ => None,
    }
}

fn coerce_nested_multidim_array_value<'a>(
    value: &'a Value,
    explicit_bounds: &[(i64, i64)],
) -> Option<(Vec<(i64, i64)>, Cow<'a, [Value]>)> {
    match value {
        Value::Array(array) => Some((explicit_bounds.to_vec(), Cow::Borrowed(array.as_slice()))),
        Value::Text(text) => {
            let (bounds, body) = split_multidim_array_bound_prefix(text)?;
            let parsed = parse_pg_array_literal_simple_with_bounds(body)?;
            Some((bounds, Cow::Owned(parsed.elements)))
        }
        _ => None,
    }
}

fn parse_optional_array_index(value: &Value) -> DbResult<Option<i64>> {
    match value {
        Value::Null => Ok(None),
        Value::Int(i) => Ok(Some(i64::from(*i))),
        Value::BigInt(i) => Ok(Some(*i)),
        _ => Err(DbError::syntax_error(
            "array subscripts in assignment must be integers",
        )),
    }
}

fn parse_array_assign_index(value: &Value) -> DbResult<ArrayAssignIndex> {
    match value {
        Value::Null => Err(DbError::syntax_error(
            "array subscript in assignment must not be null",
        )),
        Value::Int(i) => Ok(ArrayAssignIndex::Integer(i64::from(*i))),
        Value::BigInt(i) => Ok(ArrayAssignIndex::Integer(*i)),
        Value::Text(text) => Ok(ArrayAssignIndex::Text(text.clone())),
        _ => Err(DbError::syntax_error(
            "array subscripts in assignment must be integers",
        )),
    }
}

fn assign_array_steps(
    base: Value,
    steps: &[ArrayAssignStep],
    replacement: Value,
) -> DbResult<Value> {
    if steps.is_empty() {
        return Ok(replacement);
    }

    if matches!(base, Value::Jsonb(_))
        || (matches!(base, Value::Null) && jsonb_steps_use_text_key(steps))
    {
        return assign_jsonb_steps(base, steps, replacement);
    }

    match &steps[0] {
        ArrayAssignStep::Index(ArrayAssignIndex::Integer(index)) => {
            assign_array_index(base, *index, &steps[1..], replacement)
        }
        ArrayAssignStep::Index(ArrayAssignIndex::Text(_)) => Err(DbError::syntax_error(
            "array subscripts in assignment must be integers",
        )),
        ArrayAssignStep::Slice(lower, upper) => {
            assign_array_slice(base, *lower, *upper, &steps[1..], replacement)
        }
    }
}

fn assign_fixed_array_steps(
    base: Value,
    steps: &[ArrayAssignStep],
    replacement: Value,
) -> DbResult<Value> {
    let Some(first_step) = steps.first() else {
        return Ok(replacement);
    };
    let index = match first_step {
        ArrayAssignStep::Index(ArrayAssignIndex::Integer(index)) => *index,
        ArrayAssignStep::Index(ArrayAssignIndex::Text(_)) => {
            return Err(DbError::syntax_error(
                "array subscripts in assignment must be integers",
            ));
        }
        ArrayAssignStep::Slice(_, _) => return Err(fixed_length_array_slice_error()),
    };
    if !rest_steps_are_empty(&steps[1..]) {
        return Err(DbError::feature_not_supported(
            "nested fixed-length array assignment is not supported yet",
        ));
    }
    if !matches!(index, 0 | 1) {
        return Err(fixed_length_array_subscript_out_of_range());
    }

    match fixed_array_base(base)? {
        FixedArrayBase::Null => Ok(Value::Null),
        FixedArrayBase::Point(mut point) => {
            if matches!(replacement, Value::Null) {
                return Ok(Value::Text(format_fixed_point(point)));
            }

            let coordinate = fixed_point_coordinate_from_value(&replacement)?;
            if index == 0 {
                point.x = coordinate;
            } else {
                point.y = coordinate;
            }
            Ok(Value::Text(format_fixed_point(point)))
        }
    }
}

fn rest_steps_are_empty(rest: &[ArrayAssignStep]) -> bool {
    rest.is_empty()
}

fn jsonb_steps_use_text_key(steps: &[ArrayAssignStep]) -> bool {
    steps
        .iter()
        .any(|step| matches!(step, ArrayAssignStep::Index(ArrayAssignIndex::Text(_))))
}

fn assign_jsonb_steps(
    base: Value,
    steps: &[ArrayAssignStep],
    replacement: Value,
) -> DbResult<Value> {
    let target = match base {
        Value::Jsonb(json) => json,
        Value::Null => serde_json::Value::Null,
        other => {
            return Err(DbError::syntax_error(format!(
                "array assignment requires an array target, got {}",
                other
                    .data_type()
                    .map_or_else(|| "unknown".to_owned(), |dt| dt.to_string())
            )));
        }
    };
    let path = jsonb_assign_path(steps)?;
    let replacement_json = super::json_helpers::value_to_json(&replacement);
    Ok(Value::Jsonb(super::jsonb::jsonb_set_impl(
        target,
        &path,
        &replacement_json,
        true,
    )))
}

fn jsonb_assign_path(steps: &[ArrayAssignStep]) -> DbResult<Vec<Cow<'static, str>>> {
    let mut path = Vec::with_capacity(steps.len());
    for step in steps {
        match step {
            ArrayAssignStep::Index(ArrayAssignIndex::Integer(index)) => {
                path.push(Cow::Owned(index.to_string()));
            }
            ArrayAssignStep::Index(ArrayAssignIndex::Text(key)) => {
                path.push(Cow::Owned(key.clone()));
            }
            ArrayAssignStep::Slice(_, _) => {
                return Err(DbError::feature_not_supported(
                    "jsonb slice assignment is not supported",
                ));
            }
        }
    }
    Ok(path)
}

fn fixed_array_base(value: Value) -> DbResult<FixedArrayBase> {
    match value {
        Value::Null => Ok(FixedArrayBase::Null),
        Value::Text(text) => parse_fixed_point(&text)
            .map(FixedArrayBase::Point)
            .ok_or_else(cannot_subscript_text_error),
        _ => Err(cannot_subscript_text_error()),
    }
}

fn fixed_point_coordinate_from_value(value: &Value) -> DbResult<f64> {
    match value {
        Value::Int(i) => Ok(f64::from(*i)),
        Value::BigInt(i) => Ok(i64_to_f64(*i)),
        Value::Real(f) => Ok(f64::from(*f)),
        Value::Double(f) => Ok(*f),
        Value::Numeric(n) => n
            .to_string()
            .parse::<f64>()
            .map_err(|_| expected_numeric_value_error()),
        Value::Text(text) => {
            parse_fixed_point_coordinate(text).ok_or_else(expected_numeric_value_error)
        }
        _ => Err(expected_numeric_value_error()),
    }
}

fn fixed_length_array_slice_error() -> DbError {
    DbError::feature_not_supported("slices of fixed-length arrays not implemented")
}

fn fixed_length_array_subscript_out_of_range() -> DbError {
    DbError::syntax_error("array subscript out of range")
}

fn cannot_subscript_text_error() -> DbError {
    DbError::syntax_error("cannot subscript type text because it does not support subscripting")
}

fn expected_numeric_value_error() -> DbError {
    DbError::internal("expected a numeric value (Int, BigInt, Real, Double, or Numeric)")
}

fn jsonb_subscript_by_key(json: &serde_json::Value, key: &str) -> Option<serde_json::Value> {
    json.as_object().and_then(|object| object.get(key).cloned())
}

fn jsonb_subscript_by_index(json: &serde_json::Value, index: i64) -> Option<serde_json::Value> {
    let array = json.as_array()?;
    if array.is_empty() {
        return None;
    }
    let len = i64::try_from(array.len()).ok()?;
    let normalized = if index < 0 {
        len.checked_add(index)?
    } else {
        index
    };
    if normalized < 0 || normalized >= len {
        return None;
    }
    let normalized = usize::try_from(normalized).ok()?;
    array.get(normalized).cloned()
}

fn parse_fixed_point(text: &str) -> Option<FixedPointValue> {
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .unwrap_or(trimmed);
    let (x, y) = inner.split_once(',')?;
    Some(FixedPointValue {
        x: parse_fixed_point_coordinate(x)?,
        y: parse_fixed_point_coordinate(y)?,
    })
}

fn parse_fixed_point_coordinate(text: &str) -> Option<f64> {
    let trimmed = text.trim();
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

fn format_fixed_point(point: FixedPointValue) -> String {
    format!(
        "({},{})",
        format_fixed_point_coordinate(point.x),
        format_fixed_point_coordinate(point.y)
    )
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
    if !text.contains(['e', 'E']) && text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    text
}

fn value_to_assignable_array_with_tail_bounds(
    value: Value,
) -> DbResult<(ArrayValue, Vec<(i64, i64)>)> {
    match value {
        Value::Null => Ok((
            ArrayValue {
                lower_bound: 1,
                elements: Vec::new(),
            },
            Vec::new(),
        )),
        Value::Array(array) => Ok((
            ArrayValue {
                lower_bound: 1,
                elements: array,
            },
            Vec::new(),
        )),
        Value::Text(text) => {
            let (bounds, body) = split_multidim_array_bound_prefix(&text)
                .ok_or_else(|| DbError::invalid_input_syntax("array", &text))?;
            let parsed = parse_pg_array_literal_simple_with_bounds(body)
                .ok_or_else(|| DbError::invalid_input_syntax("array", &text))?;
            let lower_bound = bounds
                .first()
                .map_or(parsed.lower_bound, |(lower, _)| *lower);
            let tail_bounds = if bounds.len() > 1 {
                bounds[1..].to_vec()
            } else {
                Vec::new()
            };
            Ok((
                ArrayValue {
                    lower_bound,
                    elements: parsed.elements,
                },
                tail_bounds,
            ))
        }
        other => Err(DbError::syntax_error(format!(
            "array assignment requires an array target, got {}",
            other
                .data_type()
                .map_or_else(|| "unknown".to_owned(), |dt| dt.to_string())
        ))),
    }
}

fn array_value_from_parts_with_tail_bounds(array: ArrayValue, tail_bounds: &[(i64, i64)]) -> Value {
    if (array.lower_bound == 1 && tail_bounds.is_empty()) || array.is_empty() {
        Value::Array(array.elements)
    } else {
        let mut prefix = format!(
            "[{}:{}]",
            array.lower_bound,
            array.upper_bound().unwrap_or(array.lower_bound - 1)
        );
        use std::fmt::Write;
        for (lower, upper) in tail_bounds {
            let _ = write!(prefix, "[{lower}:{upper}]");
        }
        Value::Text(format!("{prefix}={}", Value::Array(array.elements)))
    }
}

fn decorate_nested_array_value(value: Value, tail_bounds: &[(i64, i64)]) -> Value {
    if tail_bounds.is_empty() {
        return value;
    }
    match value {
        Value::Array(elements) => array_value_from_parts_with_tail_bounds(
            ArrayValue {
                lower_bound: tail_bounds[0].0,
                elements,
            },
            &tail_bounds[1..],
        ),
        other => other,
    }
}

fn compact_nested_array_value(value: Value, tail_bounds: &[(i64, i64)]) -> Value {
    if tail_bounds.is_empty() {
        return value;
    }
    match value {
        Value::Text(text) => {
            let Some((bounds, body)) = split_multidim_array_bound_prefix(&text) else {
                return Value::Text(text);
            };
            if bounds.is_empty() {
                return Value::Text(text);
            }
            parse_pg_array_literal_simple_with_bounds(body)
                .map(|parsed| Value::Array(parsed.elements))
                .unwrap_or(Value::Text(text))
        }
        other => other,
    }
}

fn expand_slice_assignment_replacement(
    replacement: Value,
    expected_len: usize,
    has_open_bound: bool,
) -> DbResult<Vec<Value>> {
    let mut items = match replacement {
        Value::Null => Vec::new(),
        Value::Array(items) => items,
        Value::Text(text) => parse_pg_array_literal_simple(&text)
            .ok_or_else(|| DbError::invalid_input_syntax("array", &text))?,
        other if expected_len == 1 => vec![other],
        _ => {
            return Err(DbError::syntax_error(
                "array slice assignment requires an array value",
            ));
        }
    };

    if has_open_bound && items.len() < expected_len {
        return Err(DbError::syntax_error("source array too small"));
    }
    if items.len() < expected_len {
        items.resize(expected_len, Value::Null);
    } else if items.len() > expected_len {
        items.truncate(expected_len);
    }
    Ok(items)
}

fn split_nested_slice_replacements(
    replacement: Value,
    expected_len: usize,
    has_open_bound: bool,
) -> DbResult<Vec<Value>> {
    match replacement {
        Value::Array(items) => {
            if expected_len == 1 && !replacement_items_are_nested_arrays(&items) {
                Ok(vec![Value::Array(items)])
            } else {
                expand_slice_assignment_replacement(
                    Value::Array(items),
                    expected_len,
                    has_open_bound,
                )
            }
        }
        Value::Text(text) => {
            let parsed = parse_pg_array_literal_simple_with_bounds(&text)
                .ok_or_else(|| DbError::invalid_input_syntax("array", &text))?;
            if expected_len == 1 && !replacement_items_are_nested_arrays(&parsed.elements) {
                Ok(vec![array_value_from_parts(parsed)])
            } else {
                expand_slice_assignment_replacement(
                    Value::Array(parsed.elements),
                    expected_len,
                    has_open_bound,
                )
            }
        }
        Value::Null if expected_len == 1 => Ok(vec![Value::Null]),
        other if expected_len == 1 => Ok(vec![other]),
        other => expand_slice_assignment_replacement(other, expected_len, has_open_bound),
    }
}

fn replacement_items_are_nested_arrays(items: &[Value]) -> bool {
    items.iter().any(value_is_array_like)
}

fn value_is_array_like(value: &Value) -> bool {
    match value {
        Value::Array(_) => true,
        Value::Text(text) => parse_pg_array_literal_simple_with_bounds(text).is_some(),
        _ => false,
    }
}

fn assign_array_index(
    base: Value,
    index: i64,
    rest: &[ArrayAssignStep],
    replacement: Value,
) -> DbResult<Value> {
    let (mut array, tail_bounds) = value_to_assignable_array_with_tail_bounds(base)?;
    if array.is_empty() {
        let nested = compact_nested_array_value(
            assign_array_steps(
                decorate_nested_array_value(Value::Null, &tail_bounds),
                rest,
                replacement,
            )?,
            &tail_bounds,
        );
        array.lower_bound = index;
        array.elements = vec![nested];
        return Ok(array_value_from_parts_with_tail_bounds(array, &tail_bounds));
    }

    if index >= array.lower_bound {
        let slot = usize::try_from(index - array.lower_bound)
            .map_err(|_| array_assignment_limit_error())?;
        let target_len = slot
            .checked_add(1)
            .ok_or_else(array_assignment_limit_error)?;
        ensure_array_assignment_len(target_len)?;
        if array.elements.len() < target_len {
            array.elements.resize(target_len, Value::Null);
        }

        let current = decorate_nested_array_value(array.elements[slot].clone(), &tail_bounds);
        array.elements[slot] = compact_nested_array_value(
            assign_array_steps(current, rest, replacement)?,
            &tail_bounds,
        );
        return Ok(array_value_from_parts_with_tail_bounds(array, &tail_bounds));
    }

    let front_slots = array
        .lower_bound
        .checked_sub(index)
        .ok_or_else(array_assignment_limit_error)
        .and_then(|slots| usize::try_from(slots).map_err(|_| array_assignment_limit_error()))?;
    let target_len = array
        .elements
        .len()
        .checked_add(front_slots)
        .ok_or_else(array_assignment_limit_error)?;
    ensure_array_assignment_len(target_len)?;

    let nested = compact_nested_array_value(
        assign_array_steps(
            decorate_nested_array_value(Value::Null, &tail_bounds),
            rest,
            replacement,
        )?,
        &tail_bounds,
    );
    let mut prefix = Vec::with_capacity(front_slots);
    prefix.push(nested);
    while prefix.len() < front_slots {
        prefix.push(Value::Null);
    }
    prefix.extend(array.elements);
    array.lower_bound = index;
    array.elements = prefix;
    Ok(array_value_from_parts_with_tail_bounds(array, &tail_bounds))
}

fn assign_array_slice(
    base: Value,
    lower: Option<i64>,
    upper: Option<i64>,
    rest: &[ArrayAssignStep],
    replacement: Value,
) -> DbResult<Value> {
    let (mut array, tail_bounds) = value_to_assignable_array_with_tail_bounds(base)?;
    let original_len = array.len();
    let has_open_bound = lower.is_none() || upper.is_none();

    if has_open_bound && original_len == 0 {
        return Err(DbError::syntax_error("array slice subscript must provide both boundaries")
            .with_client_detail(
                "When assigning to a slice of an empty array value, slice boundaries must be fully specified.",
            ));
    }

    let start = lower.unwrap_or(array.lower_bound);
    let end = if has_open_bound && original_len > 0 {
        upper.unwrap_or(
            array
                .upper_bound()
                .ok_or_else(array_assignment_limit_error)?,
        )
    } else {
        upper.ok_or_else(|| {
            DbError::internal("__aiondb_array_assign() slice upper bound missing for closed slice")
        })?
    };
    if end < start {
        return Ok(array_value_from_parts_with_tail_bounds(array, &tail_bounds));
    }

    if start < array.lower_bound {
        let front_slots = usize::try_from(array.lower_bound - start)
            .map_err(|_| array_assignment_limit_error())?;
        let target_len = array
            .elements
            .len()
            .checked_add(front_slots)
            .ok_or_else(array_assignment_limit_error)?;
        ensure_array_assignment_len(target_len)?;
        let mut prefix = vec![Value::Null; front_slots];
        prefix.extend(array.elements);
        array.lower_bound = start;
        array.elements = prefix;
    }

    let current_upper = array
        .upper_bound()
        .ok_or_else(array_assignment_limit_error)?;
    if end > current_upper {
        let extend_by =
            usize::try_from(end - current_upper).map_err(|_| array_assignment_limit_error())?;
        let target_len = array
            .elements
            .len()
            .checked_add(extend_by)
            .ok_or_else(array_assignment_limit_error)?;
        ensure_array_assignment_len(target_len)?;
        array.elements.resize(target_len, Value::Null);
    }

    let start_idx =
        usize::try_from(start - array.lower_bound).map_err(|_| array_assignment_limit_error())?;
    let end_idx =
        usize::try_from(end - array.lower_bound).map_err(|_| array_assignment_limit_error())?;
    let expected_len = end_idx - start_idx + 1;
    ensure_array_assignment_len(array.elements.len())?;
    if rest.is_empty() {
        let replacement_items =
            expand_slice_assignment_replacement(replacement, expected_len, has_open_bound)?;
        for (offset, value) in replacement_items.into_iter().enumerate() {
            array.elements[start_idx + offset] = value;
        }
        return Ok(array_value_from_parts_with_tail_bounds(array, &tail_bounds));
    }

    let payloads = split_nested_slice_replacements(replacement, expected_len, has_open_bound)?;
    for (offset, payload) in payloads.into_iter().enumerate() {
        let current =
            decorate_nested_array_value(array.elements[start_idx + offset].clone(), &tail_bounds);
        array.elements[start_idx + offset] =
            compact_nested_array_value(assign_array_steps(current, rest, payload)?, &tail_bounds);
    }

    Ok(array_value_from_parts_with_tail_bounds(array, &tail_bounds))
}

fn ensure_array_assignment_len(len: usize) -> DbResult<()> {
    if len > MAX_ARRAY_ASSIGN_RESULT_LEN {
        return Err(array_assignment_limit_error());
    }
    Ok(())
}

fn array_assignment_limit_error() -> DbError {
    DbError::program_limit(format!(
        "array assignment would create too many elements (max {MAX_ARRAY_ASSIGN_RESULT_LEN})"
    ))
}

fn parse_pg_array_literal_simple(s: &str) -> Option<Vec<Value>> {
    parse_pg_array_literal_simple_with_bounds(s).map(|array| array.elements)
}

fn parse_pg_array_literal_simple_with_bounds(s: &str) -> Option<ArrayValue> {
    let s = s.trim();
    let (lower_bound, s) = split_array_bound_prefix(s)?;
    let lower_bound = lower_bound.unwrap_or(1);
    if !s.starts_with('{') || !s.ends_with('}') {
        return None;
    }
    let inner = &s[1..s.len() - 1];
    if inner.is_empty() {
        return Some(ArrayValue {
            lower_bound,
            elements: Vec::new(),
        });
    }
    let trimmed = inner.trim_start();
    let elements = if trimmed.starts_with('{') {
        let parts = split_top_level(inner);
        parts
            .into_iter()
            .map(|part| {
                let part = part.trim();
                if part.starts_with('{') {
                    if let Some(inner_arr) = parse_pg_array_literal_simple(part) {
                        Value::Array(inner_arr)
                    } else {
                        Value::Text(part.to_owned())
                    }
                } else {
                    parse_simple_element(part)
                }
            })
            .collect()
    } else {
        let parts = split_top_level(inner);
        parts
            .into_iter()
            .map(|p| parse_simple_element(p.trim()))
            .collect()
    };
    Some(ArrayValue {
        lower_bound,
        elements,
    })
}

fn coerce_array_value(value: &Value) -> Option<ArrayValue> {
    match value {
        Value::Array(array) => Some(ArrayValue {
            lower_bound: 1,
            elements: array.clone(),
        }),
        Value::Text(text) => parse_pg_array_literal_simple_with_bounds(text)
            .or_else(|| {
                parse_box_text(text).ok().map(|(a, b)| ArrayValue {
                    lower_bound: 0,
                    elements: vec![
                        Value::Text(format_fixed_point(FixedPointValue { x: a.x, y: a.y })),
                        Value::Text(format_fixed_point(FixedPointValue { x: b.x, y: b.y })),
                    ],
                })
            })
            .or_else(|| {
                parse_point_text(text).ok().map(|point| ArrayValue {
                    lower_bound: 0,
                    elements: vec![Value::Double(point.x), Value::Double(point.y)],
                })
            }),
        _ => None,
    }
}

fn array_value_from_parts(array: ArrayValue) -> Value {
    if array.lower_bound == 1 || array.is_empty() {
        Value::Array(array.elements)
    } else {
        Value::Text(format_array_with_lower_bound(
            array.lower_bound,
            &array.elements,
        ))
    }
}

fn format_array_with_lower_bound(lower_bound: i64, elements: &[Value]) -> String {
    let upper_bound = lower_bound + i64::try_from(elements.len()).unwrap_or(0) - 1;
    format!(
        "[{lower_bound}:{upper_bound}]={}",
        Value::Array(elements.to_vec())
    )
}

fn split_array_bound_prefix(s: &str) -> Option<(Option<i64>, &str)> {
    let s = s.trim();
    if !s.starts_with('[') {
        return Some((None, s));
    }

    let mut remaining = s;
    let mut first_lower = None;
    while let Some(rest) = remaining.strip_prefix('[') {
        let end = rest.find(']')?;
        let range = &rest[..end];
        let (lower, _) = range.split_once(':')?;
        let lower = lower.trim().parse::<i64>().ok()?;
        first_lower.get_or_insert(lower);
        remaining = &rest[end + 1..];
    }

    let remaining = remaining.trim_start();
    let remaining = remaining.strip_prefix('=')?.trim_start();
    Some((first_lower, remaining))
}

fn split_multidim_array_bound_prefix(s: &str) -> Option<(Vec<(i64, i64)>, &str)> {
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

fn split_top_level(s: &str) -> Vec<&str> {
    // Pre-size by counting `,` bytes (upper bound on top-level
    // elements; over-counts by quoted/nested commas but never
    // under-shoots).
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

fn parse_simple_element(s: &str) -> Value {
    if s.eq_ignore_ascii_case("NULL") {
        return Value::Null;
    }
    let unquoted = if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        aiondb_core::pg_array_unescape_quoted(&s[1..s.len() - 1])
    } else {
        s.to_owned()
    };
    if let Ok(i) = unquoted.parse::<i32>() {
        return Value::Int(i);
    }
    if let Ok(i) = unquoted.parse::<i64>() {
        return Value::BigInt(i);
    }
    if let Ok(f) = unquoted.parse::<f64>() {
        return Value::Double(f);
    }
    Value::Text(unquoted)
}
