#![allow(dead_code)]

use std::borrow::Cow;

use super::geometric::{
    bounds_contains, parse_box_text, parse_geometry_bounds, parse_point_text, point_inside_box,
};
use super::value_convert::to_i32_saturating;
use aiondb_core::{DbError, DbResult, ErrorReport, SqlState, Value};

/// `jsonb_to_tsvector(config_name, jsonb, filter_array)` flattens the JSONB
/// document into the text values selected by `filter_array` (e.g.
/// `'["string"]'`, `'["all"]'`) and tokenizes them as a tsvector. The
/// 2-argument form omits the config and defaults to `'simple'`.
pub(crate) fn eval_jsonb_to_tsvector(args: &[Value]) -> DbResult<Value> {
    let (config_arg, jsonb_arg, filter_arg) = match args.len() {
        2 => (None, &args[0], &args[1]),
        3 => (Some(&args[0]), &args[1], &args[2]),
        _ => {
            return Err(DbError::from_report(ErrorReport::new(
                SqlState::InvalidParameterValue,
                "jsonb_to_tsvector requires 2 or 3 arguments",
            )));
        }
    };
    if matches!(jsonb_arg, Value::Null) || matches!(filter_arg, Value::Null) {
        return Ok(Value::Null);
    }
    let json_value = match jsonb_arg {
        Value::Jsonb(v) => v.clone(),
        Value::Text(s) => {
            serde_json::from_str(s).map_err(|e| DbError::internal(format!("invalid JSON: {e}")))?
        }
        _ => return Ok(Value::Null),
    };
    let filter_set = parse_jsonb_tsvector_filter(filter_arg)?;
    let mut acc = String::new();
    collect_jsonb_tsvector_text(&json_value, &filter_set, &mut acc, 0);
    let use_english =
        matches!(config_arg, Some(Value::Text(s)) if s.eq_ignore_ascii_case("english"));
    let lexemes = super::textsearch::tokenize_to_lexemes(&acc, use_english);
    Ok(Value::Text(super::textsearch::format_tsvector(&lexemes)))
}

#[derive(Default)]
struct JsonbTsvectorFilter {
    strings: bool,
    numerics: bool,
    booleans: bool,
    keys: bool,
}

fn parse_jsonb_tsvector_filter(value: &Value) -> DbResult<JsonbTsvectorFilter> {
    let mut filter = JsonbTsvectorFilter::default();
    let entries: Vec<String> = match value {
        Value::Array(items) => items
            .iter()
            .map(|v| match v {
                Value::Text(s) => s.clone(),
                other => other.to_string(),
            })
            .collect(),
        Value::Jsonb(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        Value::Text(s) => {
            if let Ok(serde_json::Value::Array(items)) =
                serde_json::from_str::<serde_json::Value>(s)
            {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            } else {
                let trimmed = s.trim();
                let inner = trimmed
                    .strip_prefix('{')
                    .and_then(|t| t.strip_suffix('}'))
                    .unwrap_or(trimmed);
                inner.split(',').map(|p| p.trim().to_owned()).collect()
            }
        }
        _ => Vec::new(),
    };
    for raw in entries {
        match raw.trim().trim_matches('"').to_ascii_lowercase().as_str() {
            "string" => filter.strings = true,
            "numeric" => filter.numerics = true,
            "boolean" => filter.booleans = true,
            "key" => filter.keys = true,
            "all" => {
                filter.strings = true;
                filter.numerics = true;
                filter.booleans = true;
                filter.keys = true;
            }
            other if !other.is_empty() => {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::InvalidParameterValue,
                    format!("wrong flag in flag array: \"{other}\""),
                )));
            }
            _ => {}
        }
    }
    if !(filter.strings || filter.numerics || filter.booleans || filter.keys) {
        filter.strings = true;
    }
    Ok(filter)
}

fn collect_jsonb_tsvector_text(
    value: &serde_json::Value,
    filter: &JsonbTsvectorFilter,
    out: &mut String,
    depth: usize,
) {
    if depth >= MAX_JSON_DEPTH {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if filter.keys {
                    out.push(' ');
                    out.push_str(k);
                }
                collect_jsonb_tsvector_text(v, filter, out, depth + 1);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                collect_jsonb_tsvector_text(v, filter, out, depth + 1);
            }
        }
        serde_json::Value::String(s) if filter.strings => {
            out.push(' ');
            out.push_str(s);
        }
        serde_json::Value::Number(n) if filter.numerics => {
            use std::fmt::Write;
            out.push(' ');
            // Stream the number directly into the output buffer
            // instead of allocating a transient `n.to_string()`.
            let _ = write!(out, "{n}");
        }
        serde_json::Value::Bool(b) if filter.booleans => {
            out.push(' ');
            out.push_str(if *b { "true" } else { "false" });
        }
        _ => {}
    }
}

pub(crate) fn eval_jsonb_typeof(args: &[Value]) -> DbResult<Value> {
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    let Some(json) = coerce_to_jsonb_value(arg)? else {
        return Ok(Value::Null);
    };
    let type_name = match json.as_ref() {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    };
    Ok(Value::Text(type_name.to_owned()))
}

pub(crate) fn eval_jsonb_array_length(args: &[Value]) -> DbResult<Value> {
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    let Some(json) = coerce_to_jsonb_value(arg)? else {
        return Ok(Value::Null);
    };
    match json.as_ref() {
        serde_json::Value::Array(arr) => Ok(Value::Int(to_i32_saturating(arr.len()))),
        serde_json::Value::Object(_) => Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidParameterValue,
            "cannot get array length of a non-array",
        ))),
        _ => Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidParameterValue,
            "cannot get array length of a scalar",
        ))),
    }
}

pub(crate) fn eval_jsonb_build_object(args: &[Value]) -> DbResult<Value> {
    if !args.len().is_multiple_of(2) {
        return Err(DbError::from_report(
            ErrorReport::new(
                SqlState::InvalidParameterValue,
                "argument list must have even number of elements",
            )
            .with_client_hint(
                "The arguments of jsonb_build_object() must consist of alternating keys and values.",
            ),
        ));
    }
    let mut map = serde_json::Map::new();
    for chunk in args.chunks(2) {
        if matches!(chunk[0], Value::Null) {
            return Err(DbError::from_report(ErrorReport::new(
                SqlState::NotNullViolation,
                "argument 1: key must not be null",
            )));
        }
        match &chunk[0] {
            Value::Array(_) | Value::Jsonb(_) => {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::InvalidParameterValue,
                    "key value must be scalar, not array, composite, or json",
                )));
            }
            _ => {}
        }
        let key = match &chunk[0] {
            Value::Text(s) => s.clone(),
            other => other.to_string(),
        };
        let val = value_to_json(&chunk[1]);
        map.insert(key, val);
    }
    Ok(Value::Jsonb(serde_json::Value::Object(map)))
}

pub(crate) fn eval_jsonb_build_array(args: &[Value]) -> Value {
    let arr: Vec<serde_json::Value> = args.iter().map(value_to_json).collect();
    Value::Jsonb(serde_json::Value::Array(arr))
}

pub(crate) fn eval_jsonb_strip_nulls(args: &[Value]) -> DbResult<Value> {
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    let Some(json) = coerce_to_jsonb_value(arg)? else {
        return Ok(Value::Null);
    };
    Ok(Value::Jsonb(strip_nulls(json.as_ref())))
}

const MAX_JSON_DEPTH: usize = 128;

fn strip_nulls(v: &serde_json::Value) -> serde_json::Value {
    strip_nulls_inner(v, 0)
}

fn strip_nulls_inner(v: &serde_json::Value, depth: usize) -> serde_json::Value {
    if depth >= MAX_JSON_DEPTH {
        return v.clone();
    }
    match v {
        serde_json::Value::Object(map) => {
            let filtered: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), strip_nulls_inner(v, depth + 1)))
                .collect();
            serde_json::Value::Object(filtered)
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.iter()
                .map(|v| strip_nulls_inner(v, depth + 1))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn value_to_json(v: &Value) -> serde_json::Value {
    super::json_helpers::value_to_json(v)
}

/// Parse a `PostgreSQL` text-array path like `'{a,b,c}'` into a list of string keys.
fn parse_text_path(s: &str) -> Vec<Cow<'_, str>> {
    let trimmed = s.trim();
    let inner = trimmed
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(trimmed);
    if inner.is_empty() {
        return Vec::new();
    }
    inner.split(',').map(|p| Cow::Borrowed(p.trim())).collect()
}

fn json_path_component(value: &Value) -> Cow<'_, str> {
    match value {
        Value::Text(text) => Cow::Borrowed(text.as_str()),
        other => Cow::Owned(other.to_string()),
    }
}

/// Follow a path of keys/indices through a JSON value.
fn follow_path<'a, S: AsRef<str>>(
    mut current: &'a serde_json::Value,
    path: &[S],
) -> Option<&'a serde_json::Value> {
    for key in path {
        let key = key.as_ref();
        current = if let Some(obj) = current.as_object() {
            obj.get(key)?
        } else if let Some(arr) = current.as_array() {
            let idx: i64 = key.parse().ok()?;
            let resolved = if idx >= 0 {
                usize::try_from(idx).ok()?
            } else {
                let abs = usize::try_from(idx.unsigned_abs()).ok()?;
                arr.len().checked_sub(abs)?
            };
            arr.get(resolved)?
        } else {
            return None;
        };
    }
    Some(current)
}

/// `jsonb_set(target, path, new_value [, create_missing])`
pub(crate) fn eval_jsonb_set(args: &[Value]) -> DbResult<Value> {
    if args.len() < 3 || args.len() > 4 {
        return Err(DbError::internal("jsonb_set requires 3 or 4 arguments"));
    }
    if args.iter().take(3).any(Value::is_null) {
        return Ok(Value::Null);
    }
    let target_owned;
    let target = match &args[0] {
        Value::Jsonb(j) => j,
        Value::Text(s) => {
            target_owned = serde_json::from_str(s)
                .map_err(|e| DbError::internal(format!("jsonb_set() first arg: {e}")))?;
            &target_owned
        }
        _ => return Err(DbError::internal("jsonb_set() first arg must be jsonb")),
    };
    let path_str = match &args[1] {
        Value::Text(s) => parse_text_path(s),
        Value::Array(arr) => arr.iter().map(json_path_component).collect(),
        _ => return Err(DbError::internal("jsonb_set() path must be text or text[]")),
    };
    let new_value_owned;
    let new_value = match &args[2] {
        Value::Jsonb(j) => j,
        Value::Text(s) => {
            new_value_owned = serde_json::from_str(s)
                .map_err(|e| DbError::internal(format!("jsonb_set() third arg: {e}")))?;
            &new_value_owned
        }
        other => {
            // Auto-coerce other types to JSON
            new_value_owned = value_to_json(other);
            &new_value_owned
        }
    };
    let create_missing = if args.len() == 4 {
        match &args[3] {
            Value::Boolean(b) => *b,
            _ => true,
        }
    } else {
        true
    };
    let target_owned = target.clone();
    let result = jsonb_set_impl(target_owned, &path_str, new_value, create_missing);
    Ok(Value::Jsonb(result))
}

/// In-place recursive set: takes ownership of `target` and mutates only the
/// nodes along the path, avoiding full clones at every recursion level.
pub(super) fn jsonb_set_impl(
    mut target: serde_json::Value,
    path: &[Cow<'_, str>],
    new_value: &serde_json::Value,
    create_missing: bool,
) -> serde_json::Value {
    if path.is_empty() {
        return new_value.clone();
    }
    let key = path[0].as_ref();
    let rest = &path[1..];
    match &mut target {
        serde_json::Value::Object(map) => {
            if rest.is_empty() {
                if create_missing || map.contains_key(key) {
                    map.insert(key.to_owned(), new_value.clone());
                }
            } else if let Some(existing) = map.remove(key) {
                // Remove, recurse on the owned child, re-insert.
                let updated = jsonb_set_impl(existing, rest, new_value, create_missing);
                map.insert(key.to_owned(), updated);
            } else if create_missing {
                let empty = serde_json::Value::Object(serde_json::Map::new());
                let updated = jsonb_set_impl(empty, rest, new_value, create_missing);
                map.insert(key.to_owned(), updated);
            }
            target
        }
        serde_json::Value::Array(arr) => {
            if let Ok(idx) = key.parse::<usize>() {
                if idx < arr.len() {
                    if rest.is_empty() {
                        arr[idx] = new_value.clone();
                    } else {
                        // Swap out the element, recurse on the owned value, swap back.
                        let elem = std::mem::replace(&mut arr[idx], serde_json::Value::Null);
                        arr[idx] = jsonb_set_impl(elem, rest, new_value, create_missing);
                    }
                }
            }
            target
        }
        _ => target,
    }
}

/// `jsonb_extract_path(from_json, VARIADIC path_elems)`
pub(crate) fn eval_jsonb_extract_path(args: &[Value]) -> DbResult<Value> {
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    let Some(json) = coerce_to_jsonb_value(arg)? else {
        return Ok(Value::Null);
    };
    let path: Vec<Cow<'_, str>> = args[1..].iter().map(json_path_component).collect();
    match follow_path(json.as_ref(), &path) {
        Some(v) => Ok(Value::Jsonb(v.clone())),
        None => Ok(Value::Null),
    }
}

/// `jsonb_extract_path_text(from_json, VARIADIC path_elems)`
pub(crate) fn eval_jsonb_extract_path_text(args: &[Value]) -> DbResult<Value> {
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    let Some(json) = coerce_to_jsonb_value(arg)? else {
        return Ok(Value::Null);
    };
    let path: Vec<Cow<'_, str>> = args[1..].iter().map(json_path_component).collect();
    Ok(json_path_text_result(follow_path(json.as_ref(), &path)))
}

/// `jsonb_object_keys(jsonb)` - SRF returning keys as text rows
pub(crate) fn eval_jsonb_object_keys(args: &[Value]) -> DbResult<Value> {
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    let Some(json) = coerce_to_jsonb_value(arg)? else {
        return Ok(Value::Null);
    };
    match json.as_ref() {
        serde_json::Value::Object(map) => Ok(Value::Array(
            map.keys().cloned().map(Value::Text).collect::<Vec<_>>(),
        )),
        serde_json::Value::Array(_) => Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidParameterValue,
            "cannot call jsonb_object_keys on an array",
        ))),
        _ => Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidParameterValue,
            "cannot call jsonb_object_keys on a scalar",
        ))),
    }
}

/// `jsonb_pretty(jsonb)` - pretty-print JSON with PG-compatible 4-space indent
pub(crate) fn eval_jsonb_pretty(args: &[Value]) -> DbResult<Value> {
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    let Some(json) = coerce_to_jsonb_value(arg)? else {
        return Ok(Value::Null);
    };
    let v = json.as_ref();
    Ok(Value::Text(aiondb_core::value::pg_jsonb_pretty(v)))
}

// ── JSONB operator helpers used by the evaluator ──

/// Follow a text-path through a JSON value, returning JSONB.
pub(crate) fn eval_json_path_get(left: &Value, right: &Value) -> Value {
    let Value::Jsonb(json) = left else {
        return Value::Null;
    };
    match right {
        Value::Text(s) => {
            // The overwhelmingly common shape is `data -> 'key'` where `s`
            // is a bare key with no `{...}` envelope and no comma separator.
            // Skip the parse_text_path Vec allocation in that case and
            // descend with a single-element stack slice.
            if let Some(key) = single_text_path_key(s) {
                return match follow_path(json, &[key]) {
                    Some(v) => Value::Jsonb(v.clone()),
                    None => Value::Null,
                };
            }
            let path = parse_text_path(s);
            match follow_path(json, &path) {
                Some(v) => Value::Jsonb(v.clone()),
                None => Value::Null,
            }
        }
        Value::Array(arr) => {
            let path: Vec<Cow<'_, str>> = arr.iter().map(json_path_component).collect();
            match follow_path(json, &path) {
                Some(v) => Value::Jsonb(v.clone()),
                None => Value::Null,
            }
        }
        _ => Value::Null,
    }
}

/// Follow a text-path through a JSON value, returning TEXT.
/// PostgreSQL semantics: a missing path or a JSON `null` value both yield SQL NULL.
pub(crate) fn eval_json_path_get_text(left: &Value, right: &Value) -> Value {
    let Value::Jsonb(json) = left else {
        return Value::Null;
    };
    match right {
        Value::Text(s) => {
            if let Some(key) = single_text_path_key(s) {
                return json_path_text_result(follow_path(json, &[key]));
            }
            let path = parse_text_path(s);
            json_path_text_result(follow_path(json, &path))
        }
        Value::Int(i) => {
            let key = i.to_string();
            let path = [Cow::Owned(key)];
            json_path_text_result(follow_path(json, &path))
        }
        Value::BigInt(i) => {
            let key = i.to_string();
            let path = [Cow::Owned(key)];
            json_path_text_result(follow_path(json, &path))
        }
        Value::Array(arr) => {
            let path: Vec<Cow<'_, str>> = arr.iter().map(json_path_component).collect();
            json_path_text_result(follow_path(json, &path))
        }
        _ => Value::Null,
    }
}

/// Return `Some(key)` when `s` is a bare single-key path (no `{...}`
/// envelope, no comma separator). Returns the trimmed key as a `&str`
/// borrowed from `s`. `None` for empty paths or anything that would need
/// a multi-element parse.
#[inline]
fn single_text_path_key(s: &str) -> Option<&str> {
    let trimmed = s.trim();
    let inner = if trimmed.starts_with('{') && trimmed.ends_with('}') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    if inner.is_empty() || inner.contains(',') {
        return None;
    }
    Some(inner.trim())
}

fn json_path_text_result(found: Option<&serde_json::Value>) -> Value {
    match found {
        Some(serde_json::Value::Null) | None => Value::Null,
        Some(serde_json::Value::String(s)) => Value::Text(s.clone()),
        Some(v) => Value::Text(aiondb_core::value::pg_jsonb_to_string(v)),
    }
}

// ── Per-thread text→jsonb parse cache ───────────────────────────────
//
// `WHERE jsonb_col @> '{"a":1}'` was re-running `serde_json::from_str`
// on the right-hand literal for every row of the scan. Cache the
// parsed `serde_json::Value` so the parse is amortised. Keyed on the
// raw text - same trick as jsonpath_cache. Capped at 256 entries to
// keep memory bounded; a full cache simply clears.
const JSONB_TEXT_CACHE_CAP: usize = 256;

thread_local! {
    static JSONB_TEXT_CACHE: std::cell::RefCell<
        std::collections::HashMap<String, std::sync::Arc<serde_json::Value>>,
    > = std::cell::RefCell::new(std::collections::HashMap::with_capacity(16));
}

fn parse_text_to_jsonb_cached(input: &str) -> DbResult<std::sync::Arc<serde_json::Value>> {
    JSONB_TEXT_CACHE.with(|cell| {
        if let Some(hit) = cell.borrow().get(input) {
            return Ok(std::sync::Arc::clone(hit));
        }
        let parsed: serde_json::Value = serde_json::from_str(input)
            .map_err(|e| DbError::internal(format!("invalid JSON: {e}")))?;
        let arc = std::sync::Arc::new(parsed);
        let mut map = cell.borrow_mut();
        if map.len() >= JSONB_TEXT_CACHE_CAP {
            map.clear();
        }
        map.insert(input.to_owned(), std::sync::Arc::clone(&arc));
        Ok(arc)
    })
}

/// JSONB containment: does `left` contain `right`?
pub(crate) fn eval_json_contains(left: &Value, right: &Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Jsonb(a), Value::Jsonb(b)) => Ok(Value::Boolean(json_contains(a, b))),
        (Value::Jsonb(a), Value::Text(b)) => {
            let parsed = parse_text_to_jsonb_cached(b)?;
            Ok(Value::Boolean(json_contains(a, parsed.as_ref())))
        }
        (Value::Text(a), Value::Jsonb(b)) => {
            let parsed = parse_text_to_jsonb_cached(a)?;
            Ok(Value::Boolean(json_contains(parsed.as_ref(), b)))
        }
        (Value::Text(left_text), Value::Text(right_text)) => {
            // Both-JSON path takes precedence over the range-literal heuristic
            // because `{"a":"b"}` lexically looks like a multirange literal.
            if let (Ok(la), Ok(rb)) = (
                serde_json::from_str::<serde_json::Value>(left_text),
                serde_json::from_str::<serde_json::Value>(right_text),
            ) {
                return Ok(Value::Boolean(json_contains(&la, &rb)));
            }
            if let (Ok(left_bounds), Ok(right_bounds)) = (
                parse_geometry_bounds(left_text),
                parse_geometry_bounds(right_text),
            ) {
                return Ok(Value::Boolean(bounds_contains(left_bounds, right_bounds)));
            }
            if parse_box_text(left_text).is_ok() && parse_point_text(right_text).is_ok() {
                let (corner_a, corner_b) = parse_box_text(left_text)?;
                let point = parse_point_text(right_text)?;
                return Ok(Value::Boolean(point_inside_box(point, corner_a, corner_b)));
            }
            if super::range::looks_like_range(left_text) {
                if super::range::looks_like_multirange(right_text) {
                    return super::range::eval_range_contains_multirange(left, right);
                }
                if super::range::looks_like_range(right_text) {
                    return super::range::eval_range_contains_range(left, right);
                }
                return super::range::eval_range_contains_elem(left, right);
            }
            if super::range::looks_like_multirange(left_text) {
                if super::range::looks_like_multirange(right_text) {
                    return super::range::eval_multirange_contains_multirange(left, right);
                }
                if super::range::looks_like_range(right_text) {
                    return super::range::eval_multirange_contains_range(left, right);
                }
                return super::range::eval_multirange_contains_elem(left, right);
            }
            Ok(Value::Boolean(false))
        }
        (_, Value::Text(rs)) if super::range::looks_like_range(rs) => {
            super::range::eval_range_contains_elem(right, left)
        }
        (Value::Text(ls), _) if super::range::looks_like_multirange(ls) => {
            super::range::eval_multirange_contains_elem(left, right)
        }
        (Value::Text(ls), _) if super::range::looks_like_range(ls) => {
            super::range::eval_range_contains_elem(left, right)
        }
        _ => Ok(Value::Boolean(false)),
    }
}

/// JSONB contained-by: is `left` contained by `right`?
pub(crate) fn eval_json_contained_by(left: &Value, right: &Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Jsonb(_) | Value::Text(_), Value::Jsonb(_)) | (Value::Jsonb(_), Value::Text(_)) => {
            eval_json_contains(right, left)
        }
        (Value::Text(left_text), Value::Text(right_text)) => {
            if let (Ok(la), Ok(rb)) = (
                serde_json::from_str::<serde_json::Value>(left_text),
                serde_json::from_str::<serde_json::Value>(right_text),
            ) {
                return Ok(Value::Boolean(json_contains(&rb, &la)));
            }
            if let (Ok(left_bounds), Ok(right_bounds)) = (
                parse_geometry_bounds(left_text),
                parse_geometry_bounds(right_text),
            ) {
                return Ok(Value::Boolean(bounds_contains(right_bounds, left_bounds)));
            }
            // Geometric containment: point <@ box.
            if parse_point_text(left_text).is_ok() && parse_box_text(right_text).is_ok() {
                let point = parse_point_text(left_text)?;
                let (corner_a, corner_b) = parse_box_text(right_text)?;
                return Ok(Value::Boolean(point_inside_box(point, corner_a, corner_b)));
            }
            if super::range::looks_like_range(right_text) {
                if super::range::looks_like_range(left_text) {
                    return super::range::eval_range_contains_range(right, left);
                }
                if super::range::looks_like_multirange(left_text) {
                    return super::range::eval_range_contains_multirange(right, left);
                }
                return super::range::eval_range_contains_elem(right, left);
            }
            if super::range::looks_like_multirange(right_text) {
                if super::range::looks_like_multirange(left_text) {
                    return super::range::eval_multirange_contained_by_multirange(left, right);
                }
                if super::range::looks_like_range(left_text) {
                    return super::range::eval_range_contained_by_multirange(left, right);
                }
                return super::range::eval_elem_contained_by_multirange(left, right);
            }
            Ok(Value::Boolean(false))
        }
        // Geometric containment: point <@ box.
        (_, Value::Text(rs)) if super::range::looks_like_multirange(rs) => {
            super::range::eval_multirange_contains_elem(right, left)
        }
        (_, Value::Text(rs)) if super::range::looks_like_range(rs) => {
            super::range::eval_range_contains_elem(right, left)
        }
        _ => eval_json_contains(right, left),
    }
}

fn json_contains(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    json_contains_inner(a, b, 0)
}

fn json_contains_inner(a: &serde_json::Value, b: &serde_json::Value, depth: usize) -> bool {
    if depth >= MAX_JSON_DEPTH {
        return a == b;
    }
    match (a, b) {
        (serde_json::Value::Object(am), serde_json::Value::Object(bm)) => {
            bm.iter().all(|(k, bv)| {
                am.get(k)
                    .is_some_and(|av| json_contains_inner(av, bv, depth + 1))
            })
        }
        (serde_json::Value::Array(aa), serde_json::Value::Array(ba)) => ba
            .iter()
            .all(|bv| aa.iter().any(|av| json_contains_inner(av, bv, depth + 1))),
        // PostgreSQL: a JSON array contains a scalar `b` if any element equals `b`.
        (serde_json::Value::Array(aa), _) => {
            aa.iter().any(|av| json_contains_inner(av, b, depth + 1))
        }
        _ => a == b,
    }
}

/// JSONB key exists: `jsonb ? text`
pub(crate) fn eval_json_key_exists(left: &Value, right: &Value) -> DbResult<Value> {
    if matches!(left, Value::Null) || matches!(right, Value::Null) {
        return Ok(Value::Null);
    }
    let Some(json) = coerce_to_jsonb_value(left)? else {
        return Ok(Value::Null);
    };
    // Borrow the key as &str when it's already textual (the 99% case
    // for `WHERE jsonb_col ? 'literal'` filters). Only fall back to
    // `to_string` for non-text keys, which is a rare shape.
    let key_owned: String;
    let key_ref: &str = match right {
        Value::Text(s) => s.as_str(),
        other => {
            key_owned = other.to_string();
            key_owned.as_str()
        }
    };
    let exists = match json.as_ref() {
        serde_json::Value::Object(map) => map.contains_key(key_ref),
        serde_json::Value::Array(arr) => arr
            .iter()
            .any(|v| matches!(v, serde_json::Value::String(s) if s == key_ref)),
        serde_json::Value::String(s) => s == key_ref,
        _ => false,
    };
    Ok(Value::Boolean(exists))
}

fn collect_text_keys(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|v| match v {
                Value::Text(s) => Some(s.clone()),
                Value::Null => None,
                other => Some(other.to_string()),
            })
            .collect(),
        Value::Text(s) => {
            let trimmed = s.trim();
            let inner = trimmed
                .strip_prefix('{')
                .and_then(|t| t.strip_suffix('}'))
                .unwrap_or(trimmed);
            if inner.is_empty() {
                Vec::new()
            } else {
                inner
                    .split(',')
                    .map(|p| p.trim().trim_matches('"').to_owned())
                    .collect()
            }
        }
        _ => Vec::new(),
    }
}

fn json_has_key(json: &serde_json::Value, key: &str) -> bool {
    match json {
        serde_json::Value::Object(map) => map.contains_key(key),
        serde_json::Value::Array(arr) => arr
            .iter()
            .any(|v| matches!(v, serde_json::Value::String(s) if s == key)),
        serde_json::Value::String(s) => s == key,
        _ => false,
    }
}

/// JSONB any-key exists: `jsonb ?| text[]`
pub(crate) fn eval_json_any_key_exists(left: &Value, right: &Value) -> DbResult<Value> {
    if matches!(left, Value::Null) || matches!(right, Value::Null) {
        return Ok(Value::Null);
    }
    let Some(json) = coerce_to_jsonb_value(left)? else {
        return Ok(Value::Null);
    };
    let keys = collect_text_keys(right);
    Ok(Value::Boolean(
        keys.iter().any(|k| json_has_key(json.as_ref(), k)),
    ))
}

/// JSONB all-keys exist: `jsonb ?& text[]`
pub(crate) fn eval_json_all_keys_exist(left: &Value, right: &Value) -> DbResult<Value> {
    if matches!(left, Value::Null) || matches!(right, Value::Null) {
        return Ok(Value::Null);
    }
    let Some(json) = coerce_to_jsonb_value(left)? else {
        return Ok(Value::Null);
    };
    let keys = collect_text_keys(right);
    Ok(Value::Boolean(
        keys.iter().all(|k| json_has_key(json.as_ref(), k)),
    ))
}

fn composite_text_field(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    if !composite_field_needs_quotes(value) {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len() + 2);
    write_composite_quoted_field(&mut out, value);
    out
}

/// Append the composite-text representation of `value` to `out`,
/// matching `composite_text_field`'s output but without the
/// intermediate `String` allocation. NULL fields render as the
/// empty string in composite literal format.
fn push_composite_text_field(out: &mut String, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };
    if !composite_field_needs_quotes(value) {
        out.push_str(value);
        return;
    }
    write_composite_quoted_field(out, value);
}

#[inline]
fn composite_field_needs_quotes(value: &str) -> bool {
    value.is_empty()
        || value
            .chars()
            .any(|ch| matches!(ch, ',' | '"' | '(' | ')' | '\\'))
}

/// Append the `"…"` form of a composite field to `out`, doubling
/// `"` characters per PG's record literal format. Lets callers
/// stream the rendering into an existing buffer instead of
/// allocating an intermediate quoted String + drop.
fn write_composite_quoted_field(out: &mut String, value: &str) {
    out.push('"');
    // Bulk-copy chunks between `"` characters via `push_str`. Single
    // trigger byte (`"`, 0x22) which never collides with UTF-8 leading
    // bytes (>= 0x80), so slicing on raw byte indices stays at valid
    // char boundaries.
    let bytes = value.as_bytes();
    let mut last = 0usize;
    for (idx, &b) in bytes.iter().enumerate() {
        if b != b'"' {
            continue;
        }
        if idx > last {
            out.push_str(&value[last..idx]);
        }
        out.push_str("\"\"");
        last = idx + 1;
    }
    if last < bytes.len() {
        out.push_str(&value[last..]);
    }
    out.push('"');
}

/// Coerce a value into a borrowed `serde_json::Value` for jsonb scalar inputs.
/// Returns `Ok(None)` for SQL NULL and parses Text inputs as JSON.
fn coerce_to_jsonb_value(value: &Value) -> DbResult<Option<Cow<'_, serde_json::Value>>> {
    match value {
        Value::Null => Ok(None),
        Value::Jsonb(v) => Ok(Some(Cow::Borrowed(v))),
        Value::Text(s) => serde_json::from_str::<serde_json::Value>(s)
            .map(|v| Some(Cow::Owned(v)))
            .map_err(|_| {
                DbError::from_report(ErrorReport::new(
                    SqlState::InvalidTextRepresentation,
                    "invalid input syntax for type json",
                ))
            }),
        _ => Ok(None),
    }
}

fn require_object_input<'a>(
    value: &'a serde_json::Value,
    func_name: &str,
) -> DbResult<&'a serde_json::Map<String, serde_json::Value>> {
    match value {
        serde_json::Value::Object(map) => Ok(map),
        serde_json::Value::Array(_) => Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidParameterValue,
            format!("cannot call {func_name} on an array"),
        ))),
        _ => Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidParameterValue,
            format!("cannot call {func_name} on a scalar"),
        ))),
    }
}

fn require_array_input<'a>(
    value: &'a serde_json::Value,
    func_name: &str,
) -> DbResult<&'a Vec<serde_json::Value>> {
    match value {
        serde_json::Value::Array(arr) => Ok(arr),
        serde_json::Value::Object(_) => Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidParameterValue,
            format!("cannot extract elements from an object in {func_name}"),
        ))),
        _ => Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidParameterValue,
            format!("cannot extract elements from a scalar in {func_name}"),
        ))),
    }
}

pub(crate) fn eval_jsonb_each(args: &[Value]) -> DbResult<Value> {
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    let Some(json) = coerce_to_jsonb_value(arg)? else {
        return Ok(Value::Null);
    };
    let map = require_object_input(json.as_ref(), "jsonb_each")?;
    let mut rows = Vec::with_capacity(map.len());
    let mut value_buf = String::new();
    for (key, value) in map {
        // Render the jsonb value into a reused scratch buffer, then
        // stream the `(key,value)` tuple straight into the output
        // String. Saves the format!/intermediate-composite_text_field
        // allocations per row.
        value_buf.clear();
        use std::fmt::Write;
        let _ = write!(
            &mut value_buf,
            "{}",
            aiondb_core::value::PgJsonbDisplay(value)
        );
        let mut tuple = String::with_capacity(key.len() + value_buf.len() + 4);
        tuple.push('(');
        push_composite_text_field(&mut tuple, Some(key));
        tuple.push(',');
        push_composite_text_field(&mut tuple, Some(&value_buf));
        tuple.push(')');
        rows.push(Value::Text(tuple));
    }
    Ok(Value::Array(rows))
}

pub(crate) fn eval_jsonb_each_text(args: &[Value]) -> DbResult<Value> {
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    let Some(json) = coerce_to_jsonb_value(arg)? else {
        return Ok(Value::Null);
    };
    let map = require_object_input(json.as_ref(), "jsonb_each_text")?;
    let mut rows = Vec::with_capacity(map.len());
    let mut value_buf = String::new();
    for (key, value) in map {
        let value_str: Option<&str> = match value {
            serde_json::Value::Null => None,
            serde_json::Value::String(s) => Some(s.as_str()),
            _ => {
                value_buf.clear();
                use std::fmt::Write;
                let _ = write!(
                    &mut value_buf,
                    "{}",
                    aiondb_core::value::PgJsonbDisplay(value)
                );
                Some(value_buf.as_str())
            }
        };
        let value_len = value_str.map_or(0, str::len);
        let mut tuple = String::with_capacity(key.len() + value_len + 4);
        tuple.push('(');
        push_composite_text_field(&mut tuple, Some(key));
        tuple.push(',');
        push_composite_text_field(&mut tuple, value_str);
        tuple.push(')');
        rows.push(Value::Text(tuple));
    }
    Ok(Value::Array(rows))
}

pub(crate) fn eval_jsonb_array_elements(args: &[Value]) -> DbResult<Value> {
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    // Borrow the input JSON tree so the `Value::Jsonb` case skips the
    // deep tree clone entirely. We still need owned `Value::Jsonb`
    // entries for the result - that's one clone per array element,
    // versus the "full tree clone + per-element clone" pair.
    let Some(json) = coerce_to_jsonb_value(arg)? else {
        return Ok(Value::Null);
    };
    let arr = require_array_input(json.as_ref(), "jsonb_array_elements")?;
    let mut out = Vec::with_capacity(arr.len());
    for elem in arr {
        out.push(Value::Jsonb(elem.clone()));
    }
    Ok(Value::Array(out))
}

pub(crate) fn eval_jsonb_array_elements_text(args: &[Value]) -> DbResult<Value> {
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    let Some(json) = coerce_to_jsonb_value(arg)? else {
        return Ok(Value::Null);
    };
    let arr = require_array_input(json.as_ref(), "jsonb_array_elements_text")?;
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        out.push(match entry {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::String(text) => Value::Text(text.clone()),
            _ => Value::Text(aiondb_core::value::pg_jsonb_to_string(entry)),
        });
    }
    Ok(Value::Array(out))
}

pub(crate) fn eval_jsonb_each_keys(args: &[Value]) -> DbResult<Value> {
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    let Some(json) = coerce_to_jsonb_value(arg)? else {
        return Ok(Value::Null);
    };
    let map = require_object_input(json.as_ref(), "jsonb_each")?;
    Ok(Value::Array(
        map.keys().cloned().map(Value::Text).collect::<Vec<_>>(),
    ))
}

pub(crate) fn eval_jsonb_each_values(args: &[Value]) -> DbResult<Value> {
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    let Some(json) = coerce_to_jsonb_value(arg)? else {
        return Ok(Value::Null);
    };
    let map = require_object_input(json.as_ref(), "jsonb_each")?;
    Ok(Value::Array(
        map.values().cloned().map(Value::Jsonb).collect::<Vec<_>>(),
    ))
}

pub(crate) fn eval_jsonb_each_text_values(args: &[Value]) -> DbResult<Value> {
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    let Some(json) = coerce_to_jsonb_value(arg)? else {
        return Ok(Value::Null);
    };
    let map = require_object_input(json.as_ref(), "jsonb_each_text")?;
    Ok(Value::Array(
        map.values()
            .map(|value| match value {
                serde_json::Value::Null => Value::Null,
                serde_json::Value::String(text) => Value::Text(text.clone()),
                _ => Value::Text(aiondb_core::value::pg_jsonb_to_string(value)),
            })
            .collect::<Vec<_>>(),
    ))
}

fn parse_json_record_modes(value: &Value, func_name: &str) -> DbResult<Vec<String>> {
    match value {
        Value::Array(values) => Ok(values
            .iter()
            .map(|entry| match entry {
                Value::Text(text) => text.to_ascii_lowercase(),
                other => other.to_string().to_ascii_lowercase(),
            })
            .collect()),
        _ => Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidParameterValue,
            format!("{func_name} requires text[] mode metadata"),
        ))),
    }
}

fn parse_json_record_keys(value: &Value, func_name: &str) -> DbResult<Vec<String>> {
    match value {
        Value::Array(values) => Ok(values
            .iter()
            .map(|entry| match entry {
                Value::Text(text) => text.clone(),
                other => other.to_string(),
            })
            .collect()),
        _ => Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidParameterValue,
            format!("{func_name} requires text[] key metadata"),
        ))),
    }
}

fn json_array_element_to_pg_text(value: &serde_json::Value) -> String {
    let mut out = String::new();
    push_json_array_element_to_pg_text(&mut out, value);
    out
}

/// Append the PG-array-text rendering of `value` to `out`. Streaming
/// counterpart of `json_array_element_to_pg_text`: lets the array
/// builder accumulate elements directly into the output buffer
/// instead of allocating a fresh String per element.
fn push_json_array_element_to_pg_text(out: &mut String, value: &serde_json::Value) {
    use std::fmt::Write;
    match value {
        serde_json::Value::Null => out.push_str("NULL"),
        serde_json::Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        serde_json::Value::Number(n) => {
            let _ = write!(out, "{n}");
        }
        serde_json::Value::String(text) => {
            out.push('"');
            push_escaped_pg_array_text(out, text);
            out.push('"');
        }
        serde_json::Value::Array(values) => push_json_to_pg_array_literal(out, values),
        serde_json::Value::Object(_) => {
            // Nested objects are rendered via PgJsonbDisplay first
            // (streamed into a small scratch String), then escaped.
            let mut scratch = String::new();
            let _ = write!(
                &mut scratch,
                "{}",
                aiondb_core::value::PgJsonbDisplay(value)
            );
            out.push('"');
            push_escaped_pg_array_text(out, &scratch);
            out.push('"');
        }
    }
}

/// Append `text` to `out` with PG-array-quoting escapes (`\` → `\\`,
/// `"` → `\"`) inlined byte-by-byte to avoid the
/// `String::replace().replace()` chain that allocates twice.
fn push_escaped_pg_array_text(out: &mut String, text: &str) {
    // Bulk-copy chunks between trigger bytes (`\\`, `"`) via `push_str`
    // so the dominant trigger-free shape is one `push_str` of the whole
    // text. Both triggers are single-byte ASCII; UTF-8 leading bytes
    // (>= 0x80) never collide.
    let bytes = text.as_bytes();
    let mut last = 0usize;
    for (idx, &b) in bytes.iter().enumerate() {
        let escape = match b {
            b'\\' => "\\\\",
            b'"' => "\\\"",
            _ => continue,
        };
        if idx > last {
            out.push_str(&text[last..idx]);
        }
        out.push_str(escape);
        last = idx + 1;
    }
    if last < bytes.len() {
        out.push_str(&text[last..]);
    }
}

fn json_to_pg_array_literal(values: &[serde_json::Value]) -> String {
    let mut out = String::with_capacity(2 + values.len() * 8);
    push_json_to_pg_array_literal(&mut out, values);
    out
}

fn push_json_to_pg_array_literal(out: &mut String, values: &[serde_json::Value]) {
    out.push('{');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_json_array_element_to_pg_text(out, value);
    }
    out.push('}');
}

fn json_expected_array_error(key: &str, path: &[usize]) -> DbError {
    let pointer = if path.is_empty() {
        format!("key \"{key}\"")
    } else {
        let indexes = path
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join("][");
        format!("array element [{indexes}] of key \"{key}\"")
    };
    DbError::from_report(
        ErrorReport::new(SqlState::InvalidParameterValue, "expected JSON array")
            .with_client_detail(format!("See the value of {pointer}.")),
    )
}

fn json_malformed_array_error() -> DbError {
    DbError::from_report(
        ErrorReport::new(SqlState::InvalidParameterValue, "malformed JSON array")
            .with_client_detail(
                "Multidimensional arrays must have sub-arrays with matching dimensions.".to_owned(),
            ),
    )
}

fn array_dimensions(value: &serde_json::Value) -> Vec<usize> {
    let mut dims = Vec::new();
    let mut cursor = value;
    while let serde_json::Value::Array(values) = cursor {
        dims.push(values.len());
        if let Some(first) = values.first() {
            cursor = first;
        } else {
            break;
        }
    }
    dims
}

fn json_array_literal_checked(
    value: &serde_json::Value,
    key: &str,
    path: &mut Vec<usize>,
) -> DbResult<String> {
    let values = match value {
        serde_json::Value::Array(values) => values,
        _ => return Err(json_expected_array_error(key, path)),
    };
    let mut out = String::from("{");
    let mut saw_array = false;
    let mut saw_scalar = false;
    let mut nested_dims: Option<Vec<usize>> = None;
    for (index, entry) in values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        match entry {
            serde_json::Value::Array(_) => {
                saw_array = true;
                path.push(index);
                let child = json_array_literal_checked(entry, key, path)?;
                path.pop();
                out.push_str(&child);
                let child_dims = array_dimensions(entry);
                match &nested_dims {
                    Some(existing) if *existing != child_dims => {
                        return Err(json_malformed_array_error());
                    }
                    None => nested_dims = Some(child_dims),
                    _ => {}
                }
            }
            _ => {
                saw_scalar = true;
                if saw_array {
                    path.push(index);
                    let err = json_expected_array_error(key, path);
                    path.pop();
                    return Err(err);
                }
                out.push_str(&json_array_element_to_pg_text(entry));
            }
        }
    }
    if saw_array && saw_scalar {
        return Err(json_expected_array_error(key, path));
    }
    out.push('}');
    Ok(out)
}

fn extract_json_record_field_text(
    value: Option<&serde_json::Value>,
    mode: &str,
    key: &str,
) -> DbResult<Option<String>> {
    let value = match value {
        Some(value) => value,
        None => return Ok(None),
    };
    if matches!(value, serde_json::Value::Null) {
        return Ok(None);
    }
    Ok(match mode {
        "json" => Some(aiondb_core::value::pg_jsonb_to_string(value)),
        "array" => match value {
            serde_json::Value::Array(_) => {
                let mut path = Vec::new();
                Some(json_array_literal_checked(value, key, &mut path)?)
            }
            serde_json::Value::String(text) => Some(text.clone()),
            _ => return Err(json_expected_array_error(key, &[])),
        },
        _ => match value {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Bool(b) => Some(if *b {
                "true".to_owned()
            } else {
                "false".to_owned()
            }),
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                Some(aiondb_core::value::pg_jsonb_to_string(value))
            }
            serde_json::Value::Null => None,
        },
    })
}

fn row_field_lookup<'a>(
    row_map: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<&'a serde_json::Value> {
    if let Some(value) = row_map.get(key) {
        return Some(value);
    }
    let lower = key.to_ascii_lowercase();
    if let Some(index_text) = lower.strip_prefix('f') {
        if let Ok(index) = index_text.parse::<usize>() {
            let mut alternates = Vec::new();
            if index == 1 {
                alternates.push("x".to_owned());
            } else if index == 2 {
                alternates.push("y".to_owned());
            }
            if (1..=26).contains(&index) {
                let letter = ((b'a' + u8::try_from(index - 1).unwrap_or(0)) as char).to_string();
                alternates.push(letter);
            }
            for alternate in alternates {
                if let Some(value) = row_map.get(&alternate) {
                    return Some(value);
                }
            }
        }
    }
    None
}

fn parse_composite_text_fields(text: &str) -> Option<Vec<Option<String>>> {
    let inner = text.trim().strip_prefix('(')?.strip_suffix(')')?;
    if inner.is_empty() {
        return Some(Vec::new());
    }

    let mut fields = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut in_quotes = false;
    let mut chars = inner.chars();

    while let Some(ch) = chars.next() {
        if in_quotes {
            match ch {
                '\\' => current.push(chars.next()?),
                '"' => in_quotes = false,
                other => current.push(other),
            }
            continue;
        }

        match ch {
            '"' => {
                quoted = true;
                in_quotes = true;
            }
            ',' => {
                fields.push(if quoted || !current.is_empty() {
                    Some(current.clone())
                } else {
                    None
                });
                current.clear();
                quoted = false;
            }
            other => current.push(other),
        }
    }

    if in_quotes {
        return None;
    }

    fields.push(if quoted || !current.is_empty() {
        Some(current)
    } else {
        None
    });
    Some(fields)
}

fn record_default_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Text(text) => Some(text.clone()),
        Value::Boolean(flag) => Some(if *flag { "t" } else { "f" }.to_owned()),
        other => Some(other.to_string()),
    }
}

fn json_record_base_defaults(
    args: &[Value],
    json_index: usize,
    target_width: usize,
) -> Option<Vec<Option<String>>> {
    if json_index != 1 || args.is_empty() {
        return None;
    }
    let mut defaults = match &args[0] {
        Value::Array(values) => values.iter().map(record_default_text).collect::<Vec<_>>(),
        Value::Text(text) => parse_composite_text_fields(text)?,
        _ => return None,
    };
    if defaults.len() < target_width {
        defaults.resize(target_width, None);
    } else if defaults.len() > target_width {
        defaults.truncate(target_width);
    }
    Some(defaults)
}

fn json_record_row_values(
    row_map: &serde_json::Map<String, serde_json::Value>,
    keys: &[String],
    modes: &[String],
    defaults: Option<&[Option<String>]>,
) -> DbResult<Vec<Value>> {
    keys.iter()
        .enumerate()
        .map(|(index, key)| {
            let mode = modes.get(index).map(String::as_str).unwrap_or("scalar");
            let extracted =
                extract_json_record_field_text(row_field_lookup(row_map, key), mode, key)?;
            let value = match extracted {
                Some(text) => Some(text),
                None => defaults
                    .and_then(|items| items.get(index))
                    .cloned()
                    .flatten(),
            };
            Ok(value.map_or(Value::Null, Value::Text))
        })
        .collect::<DbResult<Vec<_>>>()
}

fn format_record_array_as_composite(row: &[Value]) -> Value {
    // Build the `(f1,f2,...)` composite text literal in a single
    // buffer. Per field we still allocate at most one rendered
    // String for non-Text variants (`other.to_string()`); the
    // surface allocations from the previous
    // `Vec<String>::join + format!("({})", ...)` sandwich are gone.
    let mut out = String::with_capacity(2 + row.len() * 8);
    out.push('(');
    for (i, value) in row.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match value {
            Value::Null => {} // empty field
            Value::Text(text) => write_composite_field(&mut out, text),
            Value::Boolean(flag) => out.push(if *flag { 't' } else { 'f' }),
            other => {
                let rendered = other.to_string();
                write_composite_field(&mut out, &rendered);
            }
        }
    }
    out.push(')');
    Value::Text(out)
}

#[inline]
fn write_composite_field(out: &mut String, value: &str) {
    if composite_field_needs_quotes(value) {
        write_composite_quoted_field(out, value);
    } else {
        out.push_str(value);
    }
}

fn eval_json_record_internal(
    args: &[Value],
    json_index: usize,
    set_returning: bool,
    func_name: &str,
) -> DbResult<Value> {
    let (keys, modes) = if args.len() >= json_index + 3 {
        (
            parse_json_record_keys(&args[args.len() - 2], func_name)?,
            parse_json_record_modes(&args[args.len() - 1], func_name)?,
        )
    } else if json_index == 1 && !args.is_empty() {
        match &args[0] {
            Value::Array(values) => {
                let keys = (1..=values.len())
                    .map(|index| format!("f{index}"))
                    .collect();
                let modes = vec!["scalar".to_owned(); values.len()];
                (keys, modes)
            }
            Value::Null => {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::InvalidParameterValue,
                    format!("could not determine row type for result of {func_name}"),
                )));
            }
            _ => {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::InvalidParameterValue,
                    format!("{func_name} requires json input plus key/mode metadata"),
                )));
            }
        }
    } else {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::InvalidParameterValue,
            format!("{func_name} requires json input plus key/mode metadata"),
        )));
    };
    let defaults = json_record_base_defaults(args, json_index, keys.len());
    // Borrow the input JSON tree via Cow so the dominant `Value::Jsonb`
    // input doesn't pay a deep tree clone before any field extraction.
    // `json_record_row_values` takes a borrowed `&serde_json::Map`, and
    // every match arm below now reads the already-borrowed tree.
    let Some(json) = coerce_to_jsonb_value(&args[json_index])? else {
        return Ok(if set_returning {
            Value::Array(Vec::new())
        } else {
            Value::Null
        });
    };
    if set_returning {
        let rows = match json.as_ref() {
            serde_json::Value::Array(items) => items,
            serde_json::Value::Object(_) => {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::InvalidParameterValue,
                    format!("cannot call {func_name} on a non-array"),
                )));
            }
            _ => {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::InvalidParameterValue,
                    format!("cannot call {func_name} on a scalar"),
                )));
            }
        };
        let mut out_rows = Vec::with_capacity(rows.len());
        for row in rows {
            let row_map = match row {
                serde_json::Value::Object(map) => map,
                _ => {
                    return Err(DbError::from_report(ErrorReport::new(
                        SqlState::InvalidParameterValue,
                        format!("{func_name} argument must be an array of objects"),
                    )));
                }
            };
            out_rows.push(Value::Array(json_record_row_values(
                row_map,
                &keys,
                &modes,
                defaults.as_deref(),
            )?));
        }
        return Ok(Value::Array(out_rows));
    }

    let row_map = match json.as_ref() {
        serde_json::Value::Object(map) => map,
        serde_json::Value::Array(_) => {
            return Err(DbError::from_report(ErrorReport::new(
                SqlState::InvalidParameterValue,
                format!("cannot call {func_name} on an array"),
            )));
        }
        _ => {
            return Err(DbError::from_report(ErrorReport::new(
                SqlState::InvalidParameterValue,
                format!("cannot call {func_name} on a scalar"),
            )));
        }
    };
    Ok(Value::Array(json_record_row_values(
        row_map,
        &keys,
        &modes,
        defaults.as_deref(),
    )?))
}

pub(crate) fn eval_aiondb_jsonb_to_record(args: &[Value]) -> DbResult<Value> {
    eval_json_record_internal(args, 0, false, "__aiondb_jsonb_to_record")
}

pub(crate) fn eval_aiondb_json_to_record(args: &[Value]) -> DbResult<Value> {
    eval_json_record_internal(args, 0, false, "__aiondb_json_to_record")
}

pub(crate) fn eval_aiondb_jsonb_populate_record(args: &[Value]) -> DbResult<Value> {
    eval_json_record_internal(args, 1, false, "__aiondb_jsonb_populate_record")
}

pub(crate) fn eval_aiondb_json_populate_record(args: &[Value]) -> DbResult<Value> {
    eval_json_record_internal(args, 1, false, "__aiondb_json_populate_record")
}

pub(crate) fn eval_aiondb_jsonb_to_recordset(args: &[Value]) -> DbResult<Value> {
    eval_json_record_internal(args, 0, true, "__aiondb_jsonb_to_recordset")
}

pub(crate) fn eval_aiondb_json_to_recordset(args: &[Value]) -> DbResult<Value> {
    eval_json_record_internal(args, 0, true, "__aiondb_json_to_recordset")
}

pub(crate) fn eval_aiondb_jsonb_populate_recordset(args: &[Value]) -> DbResult<Value> {
    eval_json_record_internal(args, 1, true, "__aiondb_jsonb_populate_recordset")
}

pub(crate) fn eval_aiondb_json_populate_recordset(args: &[Value]) -> DbResult<Value> {
    eval_json_record_internal(args, 1, true, "__aiondb_json_populate_recordset")
}

pub(crate) fn eval_jsonb_populate_record(args: &[Value]) -> DbResult<Value> {
    let row = eval_json_record_internal(args, 1, false, "jsonb_populate_record")?;
    let Value::Array(values) = row else {
        return Ok(Value::Null);
    };
    Ok(format_record_array_as_composite(&values))
}

pub(crate) fn eval_json_populate_record(args: &[Value]) -> DbResult<Value> {
    let row = eval_json_record_internal(args, 1, false, "json_populate_record")?;
    let Value::Array(values) = row else {
        return Ok(Value::Null);
    };
    Ok(format_record_array_as_composite(&values))
}

pub(crate) fn eval_jsonb_to_record(args: &[Value]) -> DbResult<Value> {
    let row = eval_json_record_internal(args, 0, false, "jsonb_to_record")?;
    let Value::Array(values) = row else {
        return Ok(Value::Null);
    };
    Ok(format_record_array_as_composite(&values))
}

pub(crate) fn eval_json_to_record(args: &[Value]) -> DbResult<Value> {
    let row = eval_json_record_internal(args, 0, false, "json_to_record")?;
    let Value::Array(values) = row else {
        return Ok(Value::Null);
    };
    Ok(format_record_array_as_composite(&values))
}

pub(crate) fn eval_jsonb_populate_recordset(args: &[Value]) -> DbResult<Value> {
    let rows = eval_json_record_internal(args, 1, true, "jsonb_populate_recordset")?;
    let Value::Array(items) = rows else {
        return Ok(Value::Array(Vec::new()));
    };
    let formatted = items
        .iter()
        .map(|row| match row {
            Value::Array(values) => format_record_array_as_composite(values),
            _ => Value::Null,
        })
        .collect();
    Ok(Value::Array(formatted))
}

pub(crate) fn eval_json_populate_recordset(args: &[Value]) -> DbResult<Value> {
    let rows = eval_json_record_internal(args, 1, true, "json_populate_recordset")?;
    let Value::Array(items) = rows else {
        return Ok(Value::Array(Vec::new()));
    };
    let formatted = items
        .iter()
        .map(|row| match row {
            Value::Array(values) => format_record_array_as_composite(values),
            _ => Value::Null,
        })
        .collect();
    Ok(Value::Array(formatted))
}

pub(crate) fn eval_jsonb_to_recordset(args: &[Value]) -> DbResult<Value> {
    let rows = eval_json_record_internal(args, 0, true, "jsonb_to_recordset")?;
    let Value::Array(items) = rows else {
        return Ok(Value::Array(Vec::new()));
    };
    let formatted = items
        .iter()
        .map(|row| match row {
            Value::Array(values) => format_record_array_as_composite(values),
            _ => Value::Null,
        })
        .collect();
    Ok(Value::Array(formatted))
}

pub(crate) fn eval_json_to_recordset(args: &[Value]) -> DbResult<Value> {
    let rows = eval_json_record_internal(args, 0, true, "json_to_recordset")?;
    let Value::Array(items) = rows else {
        return Ok(Value::Array(Vec::new()));
    };
    let formatted = items
        .iter()
        .map(|row| match row {
            Value::Array(values) => format_record_array_as_composite(values),
            _ => Value::Null,
        })
        .collect();
    Ok(Value::Array(formatted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonb_array_length_errors_on_non_array() {
        let err = eval_jsonb_array_length(&[Value::Jsonb(serde_json::json!({"a": 1}))])
            .expect_err("object must error");
        assert!(
            err.to_string()
                .contains("cannot get array length of a non-array"),
            "unexpected error: {err}"
        );

        let err = eval_jsonb_array_length(&[Value::Jsonb(serde_json::json!(1))])
            .expect_err("scalar must error");
        assert!(
            err.to_string()
                .contains("cannot get array length of a scalar"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn jsonb_object_keys_errors_on_non_object() {
        let err = eval_jsonb_object_keys(&[Value::Jsonb(serde_json::json!([1, 2, 3]))])
            .expect_err("array must error");
        assert!(
            err.to_string()
                .contains("cannot call jsonb_object_keys on an array"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn jsonb_each_returns_tuple_text_rows() {
        let rows = eval_jsonb_each(&[Value::Jsonb(serde_json::json!({"a": 1, "b": null}))])
            .expect("jsonb_each should succeed");
        let Value::Array(values) = rows else {
            panic!("expected array");
        };
        assert_eq!(values.len(), 2);
        assert_eq!(values[0], Value::Text("(a,1)".to_owned()));
        assert_eq!(values[1], Value::Text("(b,null)".to_owned()));
    }

    #[test]
    fn jsonb_array_elements_text_converts_non_string_values() {
        let rows = eval_jsonb_array_elements_text(&[Value::Jsonb(serde_json::json!([
            1,
            true,
            null,
            {"x": 1}
        ]))])
        .expect("jsonb_array_elements_text should succeed");
        assert_eq!(
            rows,
            Value::Array(vec![
                Value::Text("1".to_owned()),
                Value::Text("true".to_owned()),
                Value::Null,
                Value::Text("{\"x\": 1}".to_owned()),
            ])
        );
    }
}
