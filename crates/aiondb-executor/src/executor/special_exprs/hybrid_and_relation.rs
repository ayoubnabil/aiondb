use super::*;
use aiondb_core::{bounded_hnsw_ef_search, TupleId, HNSW_MAX_EF_SEARCH, VECTOR_MAX_K};
use rayon::iter::{IndexedParallelIterator, IntoParallelRefIterator, ParallelIterator};

// Vector/hybrid/full-text/recommend resolution + catalog-lookup methods,
// continuation of `impl Executor` below. Helper types/fns in this module
// are visible to the submodule as its descendant.
mod resolve;

const VECTOR_TOP_K_ADAPTIVE_HNSW_SAFETY_NUMERATOR: usize = 5;
const VECTOR_TOP_K_ADAPTIVE_HNSW_SAFETY_DENOMINATOR: usize = 4;
const VECTOR_TOP_K_ADAPTIVE_HNSW_MAX_GROWTH_FACTOR: usize = 6;
const VECTOR_TOP_K_EXACT_TUPLE_FETCH_THRESHOLD: usize = 2048;
const VECTOR_FILTER_TUPLE_FETCH_VALIDATION_THRESHOLD: usize = 4096;
const FULL_TEXT_MAX_RANK: f64 = 0.6;

#[derive(Clone, Debug)]
struct CompiledVectorTopKFilterCondition {
    ordinal: usize,
    column_id: ColumnId,
    predicate: CompiledVectorTopKFilterPredicate,
}

#[derive(Clone, Debug)]
enum CompiledVectorTopKFilterPredicate {
    Match(Value),
    Range {
        gt: Option<f64>,
        gte: Option<f64>,
        lt: Option<f64>,
        lte: Option<f64>,
    },
}

#[derive(Clone, Debug, Default)]
pub(in crate::executor) struct CompiledVectorTopKFilter {
    must: Vec<CompiledVectorTopKFilterCondition>,
    should: Vec<CompiledVectorTopKFilterCondition>,
    must_not: Vec<CompiledVectorTopKFilterCondition>,
}

impl CompiledVectorTopKFilterCondition {
    fn matches(&self, row: &Row) -> bool {
        let candidate = row.values.get(self.ordinal).unwrap_or(&Value::Null);
        match &self.predicate {
            CompiledVectorTopKFilterPredicate::Match(expected) => match expected {
                Value::Null => candidate.is_null(),
                expected => candidate == expected,
            },
            CompiledVectorTopKFilterPredicate::Range { gt, gte, lt, lte } => {
                let Some(value) = vector_filter_candidate_to_f64(candidate) else {
                    return false;
                };
                if gt.is_some_and(|bound| value <= bound) {
                    return false;
                }
                if gte.is_some_and(|bound| value < bound) {
                    return false;
                }
                if lt.is_some_and(|bound| value >= bound) {
                    return false;
                }
                if lte.is_some_and(|bound| value > bound) {
                    return false;
                }
                true
            }
        }
    }
}

impl CompiledVectorTopKFilter {
    fn matches(&self, row: &Row) -> bool {
        if self.must.iter().any(|condition| !condition.matches(row)) {
            return false;
        }
        if self.must_not.iter().any(|condition| condition.matches(row)) {
            return false;
        }
        self.should.is_empty() || self.should.iter().any(|condition| condition.matches(row))
    }
}

fn next_vector_top_k_hnsw_limit(
    current_limit: usize,
    matched_rows: usize,
    target_rows: usize,
) -> usize {
    if current_limit == 0 {
        return target_rows.min(VECTOR_MAX_K).max(1);
    }
    if matched_rows >= target_rows {
        return current_limit;
    }
    let target_rows = target_rows.max(1);
    let current_limit_u128 = current_limit as u128;
    let target_rows_u128 = target_rows as u128;
    let matched_rows_u128 = matched_rows as u128;
    let estimated_required = if matched_rows_u128 == 0 {
        current_limit_u128.saturating_mul(VECTOR_TOP_K_ADAPTIVE_HNSW_MAX_GROWTH_FACTOR as u128)
    } else {
        current_limit_u128
            .saturating_mul(target_rows_u128)
            .div_ceil(matched_rows_u128)
    };
    let with_safety_margin = estimated_required
        .saturating_mul(VECTOR_TOP_K_ADAPTIVE_HNSW_SAFETY_NUMERATOR as u128)
        .div_ceil(VECTOR_TOP_K_ADAPTIVE_HNSW_SAFETY_DENOMINATOR as u128);
    let max_growth =
        current_limit_u128.saturating_mul(VECTOR_TOP_K_ADAPTIVE_HNSW_MAX_GROWTH_FACTOR as u128);
    let capped = with_safety_margin.min(max_growth).min(VECTOR_MAX_K as u128);
    usize::try_from(capped).unwrap_or(VECTOR_MAX_K)
}

fn vector_filter_json_literal_to_value(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(v) => Value::Boolean(*v),
        serde_json::Value::Number(v) => {
            if let Some(int) = v.as_i64() {
                Value::BigInt(int)
            } else {
                Value::Double(v.as_f64().unwrap_or(f64::NAN))
            }
        }
        serde_json::Value::String(v) => Value::Text(v.clone()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => Value::Jsonb(value.clone()),
    }
}

fn vector_hit_json_number(value: f64) -> serde_json::Value {
    serde_json::Number::from_f64(value).map_or(serde_json::Value::Null, serde_json::Value::Number)
}

fn vector_hit_value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Boolean(b) => serde_json::Value::Bool(*b),
        Value::Int(n) => serde_json::Value::Number(i64::from(*n).into()),
        Value::BigInt(n) => serde_json::Value::Number((*n).into()),
        Value::Real(f) => serde_json::Number::from_f64(*f as f64)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::Double(f) => serde_json::Number::from_f64(*f)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::Text(s) => serde_json::Value::String(s.clone()),
        Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(vector_hit_value_to_json).collect())
        }
        Value::Jsonb(j) => j.clone(),
        _ => serde_json::Value::String(format!("{v}")),
    }
}

fn vector_filter_candidate_to_f64(value: &Value) -> Option<f64> {
    let coerced = aiondb_eval::coerce_value(value.clone(), &DataType::Double).ok()?;
    let Value::Double(number) = coerced else {
        return None;
    };
    if number.is_finite() {
        Some(number)
    } else {
        None
    }
}

fn vector_filter_supports_numeric_range(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Int | DataType::BigInt | DataType::Real | DataType::Double | DataType::Numeric
    )
}

#[derive(Clone, Debug, Default)]
struct HybridRrfFusionEntry {
    fused_score: f64,
    dense_rank: Option<usize>,
    sparse_rank: Option<usize>,
    dense_score: Option<f64>,
    sparse_score: Option<f64>,
    dense_distance: Option<f64>,
    sparse_distance: Option<f64>,
    payload: Option<serde_json::Value>,
}

#[derive(Clone, Debug)]
struct HybridDbsfSourceHit {
    id: i64,
    rank: usize,
    raw_score: f64,
    score: Option<f64>,
    distance: Option<f64>,
    payload: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default)]
struct HybridDbsfFusionEntry {
    fused_score: f64,
    dense_rank: Option<usize>,
    sparse_rank: Option<usize>,
    dense_score: Option<f64>,
    sparse_score: Option<f64>,
    dense_distance: Option<f64>,
    sparse_distance: Option<f64>,
    dense_normalized_score: Option<f64>,
    sparse_normalized_score: Option<f64>,
    payload: Option<serde_json::Value>,
}

fn parse_rrf_weight_arg(value: Option<&Value>, arg_name: &str) -> DbResult<f64> {
    let Some(value) = value else {
        return Ok(1.0);
    };
    let coerced = aiondb_eval::coerce_value(value.clone(), &DataType::Double)?;
    let Value::Double(weight) = coerced else {
        return Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            format!("{arg_name} must be numeric"),
        ));
    };
    if !weight.is_finite() || weight < 0.0 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("{arg_name} must be a finite non-negative number"),
        ));
    }
    Ok(weight)
}

fn parse_rrf_hits_arg(
    value: &Value,
    arg_name: &str,
) -> DbResult<Vec<serde_json::Map<String, serde_json::Value>>> {
    let hits = match value {
        Value::Null => return Ok(Vec::new()),
        Value::Array(values) => {
            let mut hits = Vec::with_capacity(values.len());
            for entry in values {
                match entry {
                    Value::Null => {}
                    Value::Jsonb(serde_json::Value::Object(object)) => hits.push(object.clone()),
                    Value::Jsonb(_) => {
                        return Err(DbError::bind_error(
                            SqlState::DatatypeMismatch,
                            format!("{arg_name} entries must be JSON objects"),
                        ));
                    }
                    other => {
                        return Err(DbError::bind_error(
                            SqlState::DatatypeMismatch,
                            format!("{arg_name} entries must be jsonb hits, got {other:?}"),
                        ));
                    }
                }
            }
            hits
        }
        Value::Jsonb(serde_json::Value::Array(values)) => values
            .iter()
            .map(|entry| {
                entry.as_object().cloned().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        format!("{arg_name} entries must be JSON objects"),
                    )
                })
            })
            .collect::<DbResult<Vec<_>>>()?,
        Value::Jsonb(_) => {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!("{arg_name} must be an array of hit objects"),
            ));
        }
        other => {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!("{arg_name} must be an array of hit objects, got {other:?}"),
            ));
        }
    };
    Ok(hits)
}

fn read_hit_id(hit: &serde_json::Map<String, serde_json::Value>, arg_name: &str) -> DbResult<i64> {
    hit.get("id")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| {
            DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("{arg_name} hit object is missing integer field \"id\""),
            )
        })
}

fn read_hit_score_for_dbsf(hit: &serde_json::Map<String, serde_json::Value>) -> Option<f64> {
    if let Some(score) = hit.get("score").and_then(serde_json::Value::as_f64) {
        return score.is_finite().then_some(score);
    }
    if let Some(distance) = hit.get("distance").and_then(serde_json::Value::as_f64) {
        return distance.is_finite().then_some(-distance);
    }
    None
}

fn parse_prefetch_hit_id_json(value: &serde_json::Value, arg_name: &str) -> DbResult<Option<i64>> {
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(number) => number.as_i64().map(Some).ok_or_else(|| {
            DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("{arg_name} numeric entries must be int64 identifiers"),
            )
        }),
        serde_json::Value::Object(object) => object
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .map(Some)
            .ok_or_else(|| {
                DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!("{arg_name} hit objects must contain integer field \"id\""),
                )
            }),
        _ => Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            format!("{arg_name} entries must be integer ids or hit objects"),
        )),
    }
}

fn parse_prefetch_hit_ids_arg(value: &Value, arg_name: &str) -> DbResult<Vec<i64>> {
    let mut parsed = Vec::new();
    match value {
        Value::Null => {}
        Value::Array(values) => {
            for entry in values {
                match entry {
                    Value::Null => {}
                    Value::Int(id) => parsed.push(i64::from(*id)),
                    Value::BigInt(id) => parsed.push(*id),
                    Value::Jsonb(json) => {
                        if let Some(id) = parse_prefetch_hit_id_json(json, arg_name)? {
                            parsed.push(id);
                        }
                    }
                    other => {
                        return Err(DbError::bind_error(
                            SqlState::DatatypeMismatch,
                            format!(
                                "{arg_name} entries must be bigint ids or jsonb hit objects, got {other:?}"
                            ),
                        ));
                    }
                }
            }
        }
        Value::Jsonb(serde_json::Value::Array(values)) => {
            for entry in values {
                if let Some(id) = parse_prefetch_hit_id_json(entry, arg_name)? {
                    parsed.push(id);
                }
            }
        }
        Value::Jsonb(_) => {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!("{arg_name} must be a JSON array of ids or hit objects"),
            ));
        }
        other => {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!("{arg_name} must be an array of ids or hit objects, got {other:?}"),
            ));
        }
    }

    let mut seen = std::collections::HashSet::with_capacity(parsed.len());
    let mut deduplicated = Vec::with_capacity(parsed.len());
    for id in parsed {
        if seen.insert(id) {
            deduplicated.push(id);
        }
    }
    Ok(deduplicated)
}

#[derive(Clone, Debug)]
enum RecommendExampleSpec {
    Id(i64),
    Vector(aiondb_core::VectorValue),
}

fn vector_dims_from_type(vector_type: &DataType, arg_name: &str) -> DbResult<u32> {
    let DataType::Vector { dims, .. } = vector_type else {
        return Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            format!("{arg_name} target column is not a vector"),
        ));
    };
    Ok(*dims)
}

fn coerce_value_to_vector(
    value: Value,
    vector_type: &DataType,
    arg_name: &str,
) -> DbResult<aiondb_core::VectorValue> {
    let coerced = aiondb_eval::coerce_value(value, vector_type)?;
    let Value::Vector(vector) = coerced else {
        return Err(DbError::internal(format!(
            "{arg_name} coercion did not produce a vector"
        )));
    };
    Ok(vector)
}

fn validate_recommend_vector_dims(
    vector: &aiondb_core::VectorValue,
    expected_dims: u32,
    arg_name: &str,
) -> DbResult<()> {
    if vector.dims != expected_dims {
        return Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            format!(
                "{arg_name} vector has dimensions {}, expected {expected_dims}",
                vector.dims
            ),
        ));
    }
    Ok(())
}

fn parse_recommend_numeric_array_as_vector(
    entries: &[serde_json::Value],
    expected_dims: u32,
    arg_name: &str,
) -> DbResult<aiondb_core::VectorValue> {
    if entries.len() != usize::try_from(expected_dims).unwrap_or(usize::MAX) {
        return Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            format!(
                "{arg_name} vector array has {} elements, expected {expected_dims}",
                entries.len()
            ),
        ));
    }
    let mut values = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let Some(number) = entry.as_f64() else {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!("{arg_name} vector element #{index} must be numeric"),
            ));
        };
        if !number.is_finite() {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("{arg_name} vector element #{index} must be finite"),
            ));
        }
        values.push(f64_to_f32(number, arg_name)?);
    }
    Ok(aiondb_core::VectorValue::new(expected_dims, values))
}

fn parse_recommend_value_array_as_vector(
    entries: &[Value],
    expected_dims: u32,
    arg_name: &str,
) -> DbResult<aiondb_core::VectorValue> {
    if entries.len() != usize::try_from(expected_dims).unwrap_or(usize::MAX) {
        return Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            format!(
                "{arg_name} vector array has {} elements, expected {expected_dims}",
                entries.len()
            ),
        ));
    }
    let mut values = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let coerced = aiondb_eval::coerce_value(entry.clone(), &DataType::Double)?;
        let Value::Double(number) = coerced else {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!("{arg_name} vector element #{index} must be numeric"),
            ));
        };
        if !number.is_finite() {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("{arg_name} vector element #{index} must be finite"),
            ));
        }
        values.push(f64_to_f32(number, arg_name)?);
    }
    Ok(aiondb_core::VectorValue::new(expected_dims, values))
}

fn json_array_looks_like_vector(entries: &[serde_json::Value], expected_dims: u32) -> bool {
    entries.len() == usize::try_from(expected_dims).unwrap_or(usize::MAX)
        && entries
            .iter()
            .all(|entry| entry.as_f64().is_some_and(f64::is_finite))
}

fn value_array_looks_like_vector(entries: &[Value], expected_dims: u32) -> bool {
    entries.len() == usize::try_from(expected_dims).unwrap_or(usize::MAX)
        && entries.iter().all(|entry| {
            aiondb_eval::coerce_value(entry.clone(), &DataType::Double)
                .ok()
                .and_then(|value| match value {
                    Value::Double(number) => Some(number.is_finite()),
                    _ => None,
                })
                .unwrap_or(false)
        })
}

fn parse_recommend_specs_from_json(
    value: &serde_json::Value,
    vector_type: &DataType,
    expected_dims: u32,
    arg_name: &str,
) -> DbResult<Vec<RecommendExampleSpec>> {
    match value {
        serde_json::Value::Null => Ok(Vec::new()),
        serde_json::Value::Number(number) => number
            .as_i64()
            .map(|id| vec![RecommendExampleSpec::Id(id)])
            .ok_or_else(|| {
                DbError::bind_error(
                    SqlState::DatatypeMismatch,
                    format!("{arg_name} numeric examples must be int64 identifiers"),
                )
            }),
        serde_json::Value::String(text) => {
            parse_recommend_specs_from_text(text, vector_type, expected_dims, arg_name)
        }
        serde_json::Value::Array(entries) => {
            if json_array_looks_like_vector(entries, expected_dims) {
                return Ok(vec![RecommendExampleSpec::Vector(
                    parse_recommend_numeric_array_as_vector(entries, expected_dims, arg_name)?,
                )]);
            }
            let mut parsed = Vec::new();
            for entry in entries {
                parsed.extend(parse_recommend_specs_from_json(
                    entry,
                    vector_type,
                    expected_dims,
                    arg_name,
                )?);
            }
            Ok(parsed)
        }
        serde_json::Value::Object(object) => {
            let id_entry = object.get("id");
            let vector_entry = object.get("vector");
            if id_entry.is_some() && vector_entry.is_some() {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!("{arg_name} example object cannot contain both \"id\" and \"vector\""),
                ));
            }
            if let Some(id_entry) = id_entry {
                let Some(id) = id_entry.as_i64() else {
                    return Err(DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        format!("{arg_name} example id must be int64"),
                    ));
                };
                return Ok(vec![RecommendExampleSpec::Id(id)]);
            }
            if let Some(vector_entry) = vector_entry {
                let vector = if let Some(vector_text) = vector_entry.as_str() {
                    coerce_value_to_vector(
                        Value::Text(vector_text.to_owned()),
                        vector_type,
                        arg_name,
                    )?
                } else if let Some(entries) = vector_entry.as_array() {
                    if json_array_looks_like_vector(entries, expected_dims) {
                        parse_recommend_numeric_array_as_vector(entries, expected_dims, arg_name)?
                    } else {
                        let serialized = serde_json::to_string(vector_entry).map_err(|err| {
                            DbError::bind_error(
                                SqlState::InvalidParameterValue,
                                format!("{arg_name} vector serialization failed: {err}"),
                            )
                        })?;
                        coerce_value_to_vector(Value::Text(serialized), vector_type, arg_name)?
                    }
                } else {
                    let serialized = serde_json::to_string(vector_entry).map_err(|err| {
                        DbError::bind_error(
                            SqlState::InvalidParameterValue,
                            format!("{arg_name} vector serialization failed: {err}"),
                        )
                    })?;
                    coerce_value_to_vector(Value::Text(serialized), vector_type, arg_name)?
                };
                validate_recommend_vector_dims(&vector, expected_dims, arg_name)?;
                return Ok(vec![RecommendExampleSpec::Vector(vector)]);
            }
            Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("{arg_name} example objects must contain \"id\" or \"vector\""),
            ))
        }
        serde_json::Value::Bool(_) => Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            format!("{arg_name} examples cannot be boolean values"),
        )),
    }
}

fn parse_recommend_specs_from_text(
    text: &str,
    vector_type: &DataType,
    expected_dims: u32,
    arg_name: &str,
) -> DbResult<Vec<RecommendExampleSpec>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let first_char = trimmed.as_bytes().first().copied().map(char::from);
    if matches!(first_char, Some('[' | '{')) {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return parse_recommend_specs_from_json(&parsed, vector_type, expected_dims, arg_name);
        }
    }
    if let Ok(id) = trimmed.parse::<i64>() {
        return Ok(vec![RecommendExampleSpec::Id(id)]);
    }
    let vector = coerce_value_to_vector(Value::Text(trimmed.to_owned()), vector_type, arg_name)?;
    validate_recommend_vector_dims(&vector, expected_dims, arg_name)?;
    Ok(vec![RecommendExampleSpec::Vector(vector)])
}

fn parse_recommend_example_specs(
    value: &Value,
    vector_type: &DataType,
    expected_dims: u32,
    arg_name: &str,
) -> DbResult<Vec<RecommendExampleSpec>> {
    match value {
        Value::Null => Ok(Vec::new()),
        Value::Int(id) => Ok(vec![RecommendExampleSpec::Id(i64::from(*id))]),
        Value::BigInt(id) => Ok(vec![RecommendExampleSpec::Id(*id)]),
        Value::Vector(vector) => {
            validate_recommend_vector_dims(vector, expected_dims, arg_name)?;
            Ok(vec![RecommendExampleSpec::Vector(vector.clone())])
        }
        Value::Text(text) => {
            parse_recommend_specs_from_text(text, vector_type, expected_dims, arg_name)
        }
        Value::Jsonb(json) => {
            parse_recommend_specs_from_json(json, vector_type, expected_dims, arg_name)
        }
        Value::Array(entries) => {
            if value_array_looks_like_vector(entries, expected_dims) {
                return Ok(vec![RecommendExampleSpec::Vector(
                    parse_recommend_value_array_as_vector(entries, expected_dims, arg_name)?,
                )]);
            }
            let mut parsed = Vec::new();
            for entry in entries {
                parsed.extend(parse_recommend_example_specs(
                    entry,
                    vector_type,
                    expected_dims,
                    arg_name,
                )?);
            }
            Ok(parsed)
        }
        other => {
            let vector = coerce_value_to_vector(other.clone(), vector_type, arg_name)?;
            validate_recommend_vector_dims(&vector, expected_dims, arg_name)?;
            Ok(vec![RecommendExampleSpec::Vector(vector)])
        }
    }
}

fn collect_recommend_example_ids(specs: &[RecommendExampleSpec]) -> std::collections::HashSet<i64> {
    let mut ids = std::collections::HashSet::new();
    for spec in specs {
        if let RecommendExampleSpec::Id(id) = spec {
            ids.insert(*id);
        }
    }
    ids
}

fn materialize_recommend_vectors(
    specs: &[RecommendExampleSpec],
    id_vectors: &std::collections::HashMap<i64, aiondb_core::VectorValue>,
    expected_dims: u32,
    arg_name: &str,
) -> DbResult<Vec<aiondb_core::VectorValue>> {
    let mut vectors = Vec::with_capacity(specs.len());
    for spec in specs {
        match spec {
            RecommendExampleSpec::Vector(vector) => {
                validate_recommend_vector_dims(vector, expected_dims, arg_name)?;
                vectors.push(vector.clone());
            }
            RecommendExampleSpec::Id(id) => {
                let Some(vector) = id_vectors.get(id) else {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        format!("{arg_name} references unknown id {id}"),
                    ));
                };
                validate_recommend_vector_dims(vector, expected_dims, arg_name)?;
                vectors.push(vector.clone());
            }
        }
    }
    Ok(vectors)
}

fn centroid_vector(
    vectors: &[aiondb_core::VectorValue],
    expected_dims: u32,
    arg_name: &str,
) -> DbResult<aiondb_core::VectorValue> {
    if vectors.is_empty() {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("{arg_name} must contain at least one example"),
        ));
    }
    let dims = usize::try_from(expected_dims).map_err(|_| {
        DbError::bind_error(
            SqlState::NumericValueOutOfRange,
            format!("{arg_name} dimension count is out of range"),
        )
    })?;
    let mut sums = vec![0.0_f64; dims];
    for vector in vectors {
        validate_recommend_vector_dims(vector, expected_dims, arg_name)?;
        for (index, value) in vector.values.iter().enumerate() {
            sums[index] += f64::from(*value);
        }
    }
    let denominator = usize_to_f64(vectors.len());
    let mut averaged = Vec::with_capacity(dims);
    for value in sums {
        let mean = value / denominator;
        if !mean.is_finite() {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("{arg_name} produced a non-finite centroid vector"),
            ));
        }
        averaged.push(f64_to_f32(mean, arg_name)?);
    }
    Ok(aiondb_core::VectorValue::new(expected_dims, averaged))
}

#[derive(Clone, Copy, Debug)]
enum FullTextQueryMode {
    Plain,
    Phrase,
    Websearch,
    Raw,
}

fn full_text_query_mode_name(mode: FullTextQueryMode) -> &'static str {
    match mode {
        FullTextQueryMode::Plain => "plain",
        FullTextQueryMode::Phrase => "phrase",
        FullTextQueryMode::Websearch => "websearch",
        FullTextQueryMode::Raw => "raw",
    }
}

fn hybrid_vector_metric_name(metric: HybridVectorMetric) -> &'static str {
    match metric {
        HybridVectorMetric::L2 => "l2",
        HybridVectorMetric::Cosine => "cosine",
        HybridVectorMetric::InnerProduct => "inner_product",
        HybridVectorMetric::Manhattan => "manhattan",
    }
}

#[derive(Clone, Copy, Debug)]
enum HybridSearchFusionMethod {
    Rrf,
    Dbsf,
}

#[derive(Clone, Debug, Default)]
struct HybridSearchTopKOptionOverrides {
    metric: Option<HybridVectorMetric>,
    query_mode: Option<FullTextQueryMode>,
    config: Option<String>,
    fusion: Option<HybridSearchFusionMethod>,
    dense_weight: Option<f64>,
    sparse_weight: Option<f64>,
    rrf_k: Option<usize>,
    source_k: Option<usize>,
    offset: Option<usize>,
    vector_ef_search: Option<usize>,
    vector_distance_threshold: Option<f64>,
    vector_exact: Option<bool>,
    vector_score_threshold: Option<f64>,
    text_score_threshold: Option<f64>,
    filter: Option<VectorTopKFilterSpec>,
}

#[derive(Clone, Debug, Default)]
struct FullTextTopKOptionOverrides {
    query_mode: Option<FullTextQueryMode>,
    config: Option<String>,
    score_threshold: Option<f64>,
    offset: Option<usize>,
    filter: Option<VectorTopKFilterSpec>,
}

#[derive(Clone, Debug)]
struct FullTextTopHit {
    score: f64,
    id: i64,
    payload: serde_json::Map<String, serde_json::Value>,
}

impl PartialEq for FullTextTopHit {
    fn eq(&self, other: &Self) -> bool {
        self.score.total_cmp(&other.score) == std::cmp::Ordering::Equal && self.id == other.id
    }
}

impl Eq for FullTextTopHit {}

impl PartialOrd for FullTextTopHit {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FullTextTopHit {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.score.total_cmp(&other.score) {
            // BinaryHeap is a max-heap; make the worst hit sort greatest so
            // peek()/peek_mut() exposes the candidate to replace.
            std::cmp::Ordering::Less => std::cmp::Ordering::Greater,
            std::cmp::Ordering::Greater => std::cmp::Ordering::Less,
            std::cmp::Ordering::Equal => self.id.cmp(&other.id),
        }
    }
}

fn full_text_hit_is_better(score: f64, id: i64, other: &FullTextTopHit) -> bool {
    match score.total_cmp(&other.score) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => id < other.id,
    }
}

fn parse_full_text_query_mode_name(mode: &str) -> DbResult<FullTextQueryMode> {
    if mode.eq_ignore_ascii_case("plain")
        || mode.eq_ignore_ascii_case("plainto")
        || mode.eq_ignore_ascii_case("plainto_tsquery")
    {
        return Ok(FullTextQueryMode::Plain);
    }
    if mode.eq_ignore_ascii_case("phrase")
        || mode.eq_ignore_ascii_case("phraseto")
        || mode.eq_ignore_ascii_case("phraseto_tsquery")
    {
        return Ok(FullTextQueryMode::Phrase);
    }
    if mode.eq_ignore_ascii_case("websearch") || mode.eq_ignore_ascii_case("websearch_to_tsquery") {
        return Ok(FullTextQueryMode::Websearch);
    }
    if mode.eq_ignore_ascii_case("raw")
        || mode.eq_ignore_ascii_case("to_tsquery")
        || mode.eq_ignore_ascii_case("tsquery")
    {
        return Ok(FullTextQueryMode::Raw);
    }
    Err(DbError::bind_error(
        SqlState::InvalidParameterValue,
        format!(
            "full_text_top_k_hits() query_mode must be one of plain, phrase, websearch, raw; got \"{mode}\""
        ),
    ))
}

fn parse_full_text_query_mode_arg(value: Option<&Value>) -> DbResult<FullTextQueryMode> {
    let Some(value) = value else {
        return Ok(FullTextQueryMode::Plain);
    };
    let mode = expect_text_arg(value, "full_text_top_k_hits() query_mode")?;
    parse_full_text_query_mode_name(mode)
}

fn parse_full_text_config_arg(value: Option<&Value>) -> DbResult<String> {
    let Some(value) = value else {
        return Ok("english".to_owned());
    };
    let config = expect_text_arg(value, "full_text_top_k_hits() config")?;
    Ok(config.to_owned())
}

fn parse_full_text_score_threshold_arg(value: Option<&Value>) -> DbResult<Option<f64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let coerced = aiondb_eval::coerce_value(value.clone(), &DataType::Double)?;
    let Value::Double(score_threshold) = coerced else {
        return Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            "full_text_top_k_hits() score_threshold must be numeric",
        ));
    };
    if !score_threshold.is_finite() {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "full_text_top_k_hits() score_threshold must be finite",
        ));
    }
    Ok(Some(score_threshold))
}

fn parse_full_text_top_k_options_arg(
    value: Option<&Value>,
) -> DbResult<FullTextTopKOptionOverrides> {
    let Some(value) = value else {
        return Ok(FullTextTopKOptionOverrides::default());
    };
    let parsed = match value {
        Value::Jsonb(json) => json.clone(),
        Value::Text(text) => serde_json::from_str::<serde_json::Value>(text).map_err(|err| {
            DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("full_text_top_k_hits() options must be valid JSON: {err}"),
            )
        })?,
        other => {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!("full_text_top_k_hits() options must be jsonb or text, got {other:?}"),
            ));
        }
    };
    let object = parsed.as_object().ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            "full_text_top_k_hits() options must be a JSON object",
        )
    })?;
    let mut options = FullTextTopKOptionOverrides::default();
    for (raw_key, raw_value) in object {
        match raw_key.to_ascii_lowercase().as_str() {
            "query_mode" => {
                let mode = raw_value.as_str().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "full_text_top_k_hits() options.query_mode must be a string",
                    )
                })?;
                options.query_mode = Some(parse_full_text_query_mode_name(mode)?);
            }
            "config" => {
                let config = raw_value.as_str().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "full_text_top_k_hits() options.config must be a string",
                    )
                })?;
                options.config = Some(config.to_owned());
            }
            "score_threshold" => {
                let threshold = raw_value.as_f64().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "full_text_top_k_hits() options.score_threshold must be numeric",
                    )
                })?;
                if !threshold.is_finite() {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "full_text_top_k_hits() options.score_threshold must be finite",
                    ));
                }
                options.score_threshold = Some(threshold);
            }
            "offset" => {
                let offset = match (raw_value.as_u64(), raw_value.as_i64()) {
                    (Some(value), _) => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "full_text_top_k_hits() options.offset is out of range",
                        )
                    })?,
                    (None, Some(value)) if value >= 0 => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "full_text_top_k_hits() options.offset is out of range",
                        )
                    })?,
                    _ => {
                        return Err(DbError::bind_error(
                            SqlState::DatatypeMismatch,
                            "full_text_top_k_hits() options.offset must be an integer",
                        ));
                    }
                };
                options.offset = Some(offset);
            }
            "filter" => {
                options.filter = Some(parse_vector_top_k_filter_spec(raw_value)?);
            }
            other => {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!("full_text_top_k_hits() options contains unknown key \"{other}\""),
                ));
            }
        }
    }
    Ok(options)
}

fn extract_quoted_tsquery_terms(tsquery: &str) -> Vec<String> {
    let bytes = tsquery.as_bytes();
    let mut terms = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut index = 0usize;

    while index < bytes.len() {
        if bytes[index] != b'\'' {
            index += 1;
            continue;
        }
        index += 1; // skip opening quote
        let mut term = String::new();
        while index < bytes.len() {
            if bytes[index] == b'\'' {
                if index + 1 < bytes.len() && bytes[index + 1] == b'\'' {
                    term.push('\'');
                    index += 2;
                    continue;
                }
                break;
            }
            term.push(bytes[index] as char);
            index += 1;
        }
        if index < bytes.len() && bytes[index] == b'\'' {
            index += 1; // skip closing quote
        }
        let normalized = term.trim().to_lowercase();
        if normalized.is_empty() {
            continue;
        }
        if seen.insert(normalized.clone()) {
            terms.push(normalized);
        }
    }

    terms
}

fn parse_hybrid_search_fusion_name(value: &str) -> DbResult<HybridSearchFusionMethod> {
    if value.eq_ignore_ascii_case("rrf") {
        return Ok(HybridSearchFusionMethod::Rrf);
    }
    if value.eq_ignore_ascii_case("dbsf") {
        return Ok(HybridSearchFusionMethod::Dbsf);
    }
    Err(DbError::bind_error(
        SqlState::InvalidParameterValue,
        format!("hybrid_search_top_k_hits() fusion must be one of rrf, dbsf; got \"{value}\""),
    ))
}

fn parse_hybrid_search_top_k_options_arg(
    value: Option<&Value>,
) -> DbResult<HybridSearchTopKOptionOverrides> {
    let Some(value) = value else {
        return Ok(HybridSearchTopKOptionOverrides::default());
    };
    let parsed = match value {
        Value::Jsonb(json) => json.clone(),
        Value::Text(text) => serde_json::from_str::<serde_json::Value>(text).map_err(|err| {
            DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("hybrid_search_top_k_hits() options must be valid JSON: {err}"),
            )
        })?,
        other => {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!("hybrid_search_top_k_hits() options must be jsonb or text, got {other:?}"),
            ));
        }
    };
    let object = parsed.as_object().ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            "hybrid_search_top_k_hits() options must be a JSON object",
        )
    })?;

    let mut options = HybridSearchTopKOptionOverrides::default();
    for (raw_key, raw_value) in object {
        match raw_key.to_ascii_lowercase().as_str() {
            "metric" => {
                let metric = raw_value.as_str().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "hybrid_search_top_k_hits() options.metric must be a string",
                    )
                })?;
                options.metric = Some(parse_vector_metric_name(metric)?);
            }
            "query_mode" => {
                let mode = raw_value.as_str().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "hybrid_search_top_k_hits() options.query_mode must be a string",
                    )
                })?;
                options.query_mode = Some(parse_full_text_query_mode_name(mode)?);
            }
            "config" => {
                let config = raw_value.as_str().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "hybrid_search_top_k_hits() options.config must be a string",
                    )
                })?;
                options.config = Some(config.to_owned());
            }
            "fusion" => {
                let fusion = raw_value.as_str().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "hybrid_search_top_k_hits() options.fusion must be a string",
                    )
                })?;
                options.fusion = Some(parse_hybrid_search_fusion_name(fusion)?);
            }
            "dense_weight" => {
                let value = raw_value.as_f64().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "hybrid_search_top_k_hits() options.dense_weight must be numeric",
                    )
                })?;
                if !value.is_finite() || value < 0.0 {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "hybrid_search_top_k_hits() options.dense_weight must be a finite non-negative number",
                    ));
                }
                options.dense_weight = Some(value);
            }
            "sparse_weight" => {
                let value = raw_value.as_f64().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "hybrid_search_top_k_hits() options.sparse_weight must be numeric",
                    )
                })?;
                if !value.is_finite() || value < 0.0 {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "hybrid_search_top_k_hits() options.sparse_weight must be a finite non-negative number",
                    ));
                }
                options.sparse_weight = Some(value);
            }
            "rrf_k" => {
                let rrf_k = match (raw_value.as_u64(), raw_value.as_i64()) {
                    (Some(value), _) => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "hybrid_search_top_k_hits() options.rrf_k is out of range",
                        )
                    })?,
                    (None, Some(value)) if value >= 0 => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "hybrid_search_top_k_hits() options.rrf_k is out of range",
                        )
                    })?,
                    _ => {
                        return Err(DbError::bind_error(
                            SqlState::DatatypeMismatch,
                            "hybrid_search_top_k_hits() options.rrf_k must be an integer",
                        ));
                    }
                };
                options.rrf_k = Some(rrf_k.max(1));
            }
            "source_k" => {
                let source_k = match (raw_value.as_u64(), raw_value.as_i64()) {
                    (Some(value), _) => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "hybrid_search_top_k_hits() options.source_k is out of range",
                        )
                    })?,
                    (None, Some(value)) if value >= 0 => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "hybrid_search_top_k_hits() options.source_k is out of range",
                        )
                    })?,
                    _ => {
                        return Err(DbError::bind_error(
                            SqlState::DatatypeMismatch,
                            "hybrid_search_top_k_hits() options.source_k must be an integer",
                        ));
                    }
                };
                if source_k == 0 {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "hybrid_search_top_k_hits() options.source_k must be >= 1",
                    ));
                }
                options.source_k = Some(source_k);
            }
            "offset" => {
                let offset = match (raw_value.as_u64(), raw_value.as_i64()) {
                    (Some(value), _) => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "hybrid_search_top_k_hits() options.offset is out of range",
                        )
                    })?,
                    (None, Some(value)) if value >= 0 => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "hybrid_search_top_k_hits() options.offset is out of range",
                        )
                    })?,
                    _ => {
                        return Err(DbError::bind_error(
                            SqlState::DatatypeMismatch,
                            "hybrid_search_top_k_hits() options.offset must be an integer",
                        ));
                    }
                };
                options.offset = Some(offset);
            }
            "vector_ef_search" => {
                let ef_search = match (raw_value.as_u64(), raw_value.as_i64()) {
                    (Some(value), _) => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "hybrid_search_top_k_hits() options.vector_ef_search is out of range",
                        )
                    })?,
                    (None, Some(value)) if value >= 0 => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "hybrid_search_top_k_hits() options.vector_ef_search is out of range",
                        )
                    })?,
                    _ => {
                        return Err(DbError::bind_error(
                            SqlState::DatatypeMismatch,
                            "hybrid_search_top_k_hits() options.vector_ef_search must be an integer",
                        ));
                    }
                };
                if ef_search == 0 {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "hybrid_search_top_k_hits() options.vector_ef_search must be >= 1",
                    ));
                }
                options.vector_ef_search = Some(ef_search.min(HNSW_MAX_EF_SEARCH));
            }
            "vector_distance_threshold" => {
                let threshold = raw_value.as_f64().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "hybrid_search_top_k_hits() options.vector_distance_threshold must be numeric",
                    )
                })?;
                if !threshold.is_finite() || threshold < 0.0 {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "hybrid_search_top_k_hits() options.vector_distance_threshold must be a finite non-negative number",
                    ));
                }
                options.vector_distance_threshold = Some(threshold);
            }
            "vector_exact" => {
                let exact = raw_value.as_bool().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "hybrid_search_top_k_hits() options.vector_exact must be boolean",
                    )
                })?;
                options.vector_exact = Some(exact);
            }
            "vector_score_threshold" => {
                let threshold = raw_value.as_f64().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "hybrid_search_top_k_hits() options.vector_score_threshold must be numeric",
                    )
                })?;
                if !threshold.is_finite() {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "hybrid_search_top_k_hits() options.vector_score_threshold must be finite",
                    ));
                }
                options.vector_score_threshold = Some(threshold);
            }
            "text_score_threshold" => {
                let threshold = raw_value.as_f64().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "hybrid_search_top_k_hits() options.text_score_threshold must be numeric",
                    )
                })?;
                if !threshold.is_finite() {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "hybrid_search_top_k_hits() options.text_score_threshold must be finite",
                    ));
                }
                options.text_score_threshold = Some(threshold);
            }
            "filter" => {
                options.filter = Some(parse_vector_top_k_filter_spec(raw_value)?);
            }
            other => {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!("hybrid_search_top_k_hits() options contains unknown key \"{other}\""),
                ));
            }
        }
    }
    Ok(options)
}

fn tsquery_has_disjunction_or_negation(tsquery: &str) -> bool {
    let bytes = tsquery.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'\'' {
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\'' {
                    if index + 1 < bytes.len() && bytes[index + 1] == b'\'' {
                        index += 2;
                        continue;
                    }
                    index += 1;
                    break;
                }
                index += 1;
            }
            continue;
        }
        if bytes[index] == b'|' || bytes[index] == b'!' {
            return true;
        }
        index += 1;
    }
    false
}

fn vector_top_k_filter_condition_to_json(
    condition: &VectorTopKFilterCondition,
) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "key".to_owned(),
        serde_json::Value::String(condition.key.clone()),
    );
    match &condition.predicate {
        VectorTopKFilterPredicateSpec::Match(value) => {
            let mut match_object = serde_json::Map::new();
            match_object.insert("value".to_owned(), value.clone());
            object.insert("match".to_owned(), serde_json::Value::Object(match_object));
        }
        VectorTopKFilterPredicateSpec::Range(range) => {
            let mut range_object = serde_json::Map::new();
            if let Some(value) = range.gt {
                range_object.insert("gt".to_owned(), vector_hit_json_number(value));
            }
            if let Some(value) = range.gte {
                range_object.insert("gte".to_owned(), vector_hit_json_number(value));
            }
            if let Some(value) = range.lt {
                range_object.insert("lt".to_owned(), vector_hit_json_number(value));
            }
            if let Some(value) = range.lte {
                range_object.insert("lte".to_owned(), vector_hit_json_number(value));
            }
            object.insert("range".to_owned(), serde_json::Value::Object(range_object));
        }
    }
    serde_json::Value::Object(object)
}

fn vector_top_k_filter_spec_to_json(filter: &VectorTopKFilterSpec) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    if !filter.must.is_empty() {
        object.insert(
            "must".to_owned(),
            serde_json::Value::Array(
                filter
                    .must
                    .iter()
                    .map(vector_top_k_filter_condition_to_json)
                    .collect(),
            ),
        );
    }
    if !filter.should.is_empty() {
        object.insert(
            "should".to_owned(),
            serde_json::Value::Array(
                filter
                    .should
                    .iter()
                    .map(vector_top_k_filter_condition_to_json)
                    .collect(),
            ),
        );
    }
    if !filter.must_not.is_empty() {
        object.insert(
            "must_not".to_owned(),
            serde_json::Value::Array(
                filter
                    .must_not
                    .iter()
                    .map(vector_top_k_filter_condition_to_json)
                    .collect(),
            ),
        );
    }
    serde_json::Value::Object(object)
}

fn literal_expr_from_value(value: Value) -> TypedExpr {
    let data_type = value.data_type().unwrap_or(DataType::Text);
    let nullable = value.is_null();
    TypedExpr::literal(value, data_type, nullable)
}

fn collect_dbsf_source_hits(
    hits: &[serde_json::Map<String, serde_json::Value>],
    arg_name: &str,
    context: &ExecutionContext,
) -> DbResult<Vec<HybridDbsfSourceHit>> {
    let mut collected = Vec::with_capacity(hits.len());
    let mut seen = std::collections::HashSet::new();
    for (rank, hit) in hits.iter().enumerate() {
        context.check_deadline()?;
        let id = read_hit_id(hit, arg_name)?;
        if !seen.insert(id) {
            continue;
        }
        let Some(raw_score) = read_hit_score_for_dbsf(hit) else {
            continue;
        };
        collected.push(HybridDbsfSourceHit {
            id,
            rank: rank.saturating_add(1),
            raw_score,
            score: hit.get("score").and_then(serde_json::Value::as_f64),
            distance: hit.get("distance").and_then(serde_json::Value::as_f64),
            payload: hit.get("payload").cloned(),
        });
    }
    Ok(collected)
}

fn compute_dbsf_normalized_scores(source_hits: &[HybridDbsfSourceHit]) -> Vec<f64> {
    if source_hits.is_empty() {
        return Vec::new();
    }
    let mut score_sum = 0.0_f64;
    for hit in source_hits {
        score_sum += hit.raw_score;
    }
    let mean = score_sum / usize_to_f64(source_hits.len());
    let mut variance_sum = 0.0_f64;
    for hit in source_hits {
        let centered = hit.raw_score - mean;
        variance_sum += centered * centered;
    }
    let variance = variance_sum / usize_to_f64(source_hits.len());
    let std_dev = variance.sqrt();
    if !mean.is_finite() || !std_dev.is_finite() {
        return vec![0.0; source_hits.len()];
    }
    if std_dev <= f64::EPSILON {
        return vec![0.5; source_hits.len()];
    }
    let low = mean - (3.0 * std_dev);
    let high = mean + (3.0 * std_dev);
    let span = high - low;
    if !low.is_finite() || !high.is_finite() || !span.is_finite() || span <= f64::EPSILON {
        return vec![0.5; source_hits.len()];
    }
    source_hits
        .iter()
        .map(|hit| ((hit.raw_score - low) / span).clamp(0.0, 1.0))
        .collect()
}

impl Executor {
    fn best_eq_lookup_index_for_column(
        indexes: &[IndexDescriptor],
        column_id: ColumnId,
    ) -> Option<IndexId> {
        let mut best: Option<(IndexId, bool, usize)> = None;
        for index in indexes {
            let Some(first_key_column) = index.key_columns.first() else {
                continue;
            };
            if first_key_column.column_id != column_id {
                continue;
            }
            let candidate = (index.index_id, index.unique, index.key_columns.len());
            match best {
                None => best = Some(candidate),
                Some((_, best_unique, best_key_len))
                    if (candidate.1 && !best_unique)
                        || (candidate.1 == best_unique && candidate.2 < best_key_len) =>
                {
                    best = Some(candidate);
                }
                _ => {}
            }
        }
        best.map(|(index_id, _, _)| index_id)
    }

    fn load_rows_by_bigint_ids(
        &self,
        context: &ExecutionContext,
        table: &TableDescriptor,
        ids: &[i64],
    ) -> DbResult<std::collections::HashMap<i64, Row>> {
        let mut rows_by_id = std::collections::HashMap::with_capacity(ids.len());
        if ids.is_empty() {
            return Ok(rows_by_id);
        }

        let id_column = table
            .columns
            .first()
            .ok_or_else(|| DbError::internal("table has no identifier column"))?;
        let indexes = self
            .catalog_reader
            .list_indexes(context.txn_id, table.table_id)?;
        if let Some(index_id) = Self::best_eq_lookup_index_for_column(&indexes, id_column.column_id)
        {
            for id in ids {
                context.check_deadline()?;
                if rows_by_id.contains_key(id) {
                    continue;
                }
                let mut stream = self.scan_index_locked(
                    context,
                    table.table_id,
                    index_id,
                    exact_lookup_key_range(&Value::BigInt(*id)),
                    None,
                )?;
                while let Some(record) = stream.next()? {
                    context.check_deadline()?;
                    let row_id = aiondb_eval::coerce_value(
                        record.row.values.first().cloned().unwrap_or(Value::Null),
                        &DataType::BigInt,
                    )?;
                    let Value::BigInt(row_id) = row_id else {
                        continue;
                    };
                    if row_id == *id {
                        rows_by_id.insert(row_id, record.row);
                        break;
                    }
                }
            }
            return Ok(rows_by_id);
        }

        let target_ids: std::collections::HashSet<i64> = ids.iter().copied().collect();
        let mut stream = self.scan_table_locked(context, table.table_id, None)?;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let id = aiondb_eval::coerce_value(
                record.row.values.first().cloned().unwrap_or(Value::Null),
                &DataType::BigInt,
            )?;
            let Value::BigInt(id) = id else {
                continue;
            };
            if target_ids.contains(&id) && !rows_by_id.contains_key(&id) {
                rows_by_id.insert(id, record.row);
                if rows_by_id.len() >= target_ids.len() {
                    break;
                }
            }
        }
        Ok(rows_by_id)
    }

    fn load_rows_by_tuple_ids(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        tuple_ids: &[TupleId],
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Vec<Row>> {
        if tuple_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;

        let mut rows = Vec::with_capacity(tuple_ids.len());
        for tuple_id in tuple_ids {
            context.check_deadline()?;
            let Some(row) = self.storage_dml.fetch(
                context.txn_id,
                &context.snapshot,
                table_id,
                *tuple_id,
                projected_columns.clone(),
            )?
            else {
                continue;
            };
            rows.push(row);
        }
        Ok(rows)
    }

    fn resolve_graph_neighbor_edge_meta(
        &self,
        context: &ExecutionContext,
        edge_label: &str,
    ) -> DbResult<GraphNeighborEdgeMeta> {
        // The cache stores keys in lowercase (see the insert at the bottom of
        // this fn). For already-lowercase labels --- the common case for SQL
        // string literals like 'bench_follow' --- we can borrow `edge_label`
        // directly and skip the per-call String allocation.
        let needs_lower = edge_label.bytes().any(|b| b.is_ascii_uppercase());
        let mut owned_lower = String::new();
        let lookup_key: &str = if needs_lower {
            owned_lower = edge_label.to_ascii_lowercase();
            owned_lower.as_str()
        } else {
            edge_label
        };
        if let Some(meta) = self
            .graph_neighbor_meta
            .lock()
            .map_err(|error| DbError::internal(format!("graph metadata cache poisoned: {error}")))?
            .get(lookup_key)
            .cloned()
        {
            return Ok(meta);
        }
        // Cache miss: ensure we have an owned lowercase copy to insert below.
        let cache_key = if needs_lower {
            owned_lower
        } else {
            edge_label.to_owned()
        };

        let edge = self
            .catalog_reader
            .get_edge_label(context.txn_id, edge_label)?
            .ok_or_else(|| {
                DbError::bind_error(
                    SqlState::UndefinedObject,
                    format!("edge label \"{edge_label}\" does not exist"),
                )
            })?;
        let edge_table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, edge.table_id)?
            .ok_or_else(|| DbError::internal("edge label backing table not found"))?;
        let (source_idx, target_idx) =
            self.resolve_edge_endpoint_columns_for_label(context, &edge)?;
        let source_type = edge_table
            .columns
            .get(source_idx)
            .map(|column| column.data_type.clone())
            .ok_or_else(|| DbError::internal("edge table missing source_id column"))?;
        let target_type = edge_table
            .columns
            .get(target_idx)
            .map(|column| column.data_type.clone())
            .ok_or_else(|| DbError::internal("edge table missing target_id column"))?;
        let meta = GraphNeighborEdgeMeta {
            table_id: edge.table_id,
            source_idx,
            target_idx,
            source_type,
            target_type,
            use_table_adjacency: edge.endpoints.is_none(),
        };
        self.graph_neighbor_meta
            .lock()
            .map_err(|error| DbError::internal(format!("graph metadata cache poisoned: {error}")))?
            .insert(cache_key, meta.clone());
        Ok(meta)
    }

    pub(crate) fn resolve_graph_neighbors(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Array(Vec::new()));
        }
        if !(2..=4).contains(&arg_values.len()) {
            return Err(DbError::internal(
                "graph_neighbors() expects 2, 3, or 4 arguments",
            ));
        }

        let edge_label = expect_text_arg(&arg_values[0], "graph_neighbors() edge label")?;
        let (direction, limit) = parse_graph_neighbor_options(&arg_values)?;
        if limit == Some(0) {
            return Ok(Value::Array(Vec::new()));
        }
        let meta = self.resolve_graph_neighbor_edge_meta(context, edge_label)?;

        // Pre-size both the neighbor list and the dedup set when the caller
        // supplied a limit. With no limit we fall back to a small but
        // non-zero hint so the first dozen pushes don't trigger Vec::reserve.
        // Capping at 1024 avoids a multi-MiB up-front HashSet allocation
        // when a malicious caller passes a huge limit.
        let capacity_hint = limit.map_or(16, |max_rows| max_rows.min(1024));
        let mut neighbors = Vec::with_capacity(capacity_hint);
        let mut seen_neighbors = GraphNeighborSeen::new(limit, capacity_hint);
        match direction {
            GraphNeighborDirection::Outgoing => self.collect_graph_neighbors(
                context,
                meta.table_id,
                &arg_values[1],
                &meta.source_type,
                meta.source_idx,
                meta.target_idx,
                true,
                meta.use_table_adjacency,
                limit,
                &mut neighbors,
                &mut seen_neighbors,
            )?,
            GraphNeighborDirection::Incoming => self.collect_graph_neighbors(
                context,
                meta.table_id,
                &arg_values[1],
                &meta.target_type,
                meta.target_idx,
                meta.source_idx,
                false,
                meta.use_table_adjacency,
                limit,
                &mut neighbors,
                &mut seen_neighbors,
            )?,
            GraphNeighborDirection::Both => {
                self.collect_graph_neighbors(
                    context,
                    meta.table_id,
                    &arg_values[1],
                    &meta.source_type,
                    meta.source_idx,
                    meta.target_idx,
                    true,
                    meta.use_table_adjacency,
                    limit,
                    &mut neighbors,
                    &mut seen_neighbors,
                )?;
                if limit.map_or(true, |max_rows| neighbors.len() < max_rows) {
                    self.collect_graph_neighbors(
                        context,
                        meta.table_id,
                        &arg_values[1],
                        &meta.target_type,
                        meta.target_idx,
                        meta.source_idx,
                        false,
                        meta.use_table_adjacency,
                        limit,
                        &mut neighbors,
                        &mut seen_neighbors,
                    )?;
                }
            }
        }

        Ok(Value::Array(neighbors))
    }

    pub(crate) fn resolve_graph_neighbors_rows(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Vec<Row>> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Vec::new());
        }
        if !(2..=4).contains(&arg_values.len()) {
            return Err(DbError::internal(
                "graph_neighbors() expects 2, 3, or 4 arguments",
            ));
        }

        let edge_label = expect_text_arg(&arg_values[0], "graph_neighbors() edge label")?;
        let (direction, limit) = parse_graph_neighbor_options(&arg_values)?;
        if limit == Some(0) {
            return Ok(Vec::new());
        }
        let meta = self.resolve_graph_neighbor_edge_meta(context, edge_label)?;
        let capacity_hint = limit.map_or(16, |max_rows| max_rows.min(1024));
        let mut rows = Vec::with_capacity(capacity_hint);
        let mut seen_neighbors = GraphNeighborSeen::new(limit, capacity_hint);
        match direction {
            GraphNeighborDirection::Outgoing => self.collect_graph_neighbors(
                context,
                meta.table_id,
                &arg_values[1],
                &meta.source_type,
                meta.source_idx,
                meta.target_idx,
                true,
                meta.use_table_adjacency,
                limit,
                &mut rows,
                &mut seen_neighbors,
            )?,
            GraphNeighborDirection::Incoming => self.collect_graph_neighbors(
                context,
                meta.table_id,
                &arg_values[1],
                &meta.target_type,
                meta.target_idx,
                meta.source_idx,
                false,
                meta.use_table_adjacency,
                limit,
                &mut rows,
                &mut seen_neighbors,
            )?,
            GraphNeighborDirection::Both => {
                self.collect_graph_neighbors(
                    context,
                    meta.table_id,
                    &arg_values[1],
                    &meta.source_type,
                    meta.source_idx,
                    meta.target_idx,
                    true,
                    meta.use_table_adjacency,
                    limit,
                    &mut rows,
                    &mut seen_neighbors,
                )?;
                if limit.map_or(true, |max_rows| rows.len() < max_rows) {
                    self.collect_graph_neighbors(
                        context,
                        meta.table_id,
                        &arg_values[1],
                        &meta.target_type,
                        meta.target_idx,
                        meta.source_idx,
                        false,
                        meta.use_table_adjacency,
                        limit,
                        &mut rows,
                        &mut seen_neighbors,
                    )?;
                }
            }
        }

        Ok(rows)
    }

    fn collect_graph_neighbors<O: GraphNeighborOutput>(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        seed_id: &Value,
        endpoint_type: &DataType,
        endpoint_idx: usize,
        neighbor_idx: usize,
        outgoing: bool,
        use_table_adjacency: bool,
        limit: Option<usize>,
        output: &mut O,
        seen: &mut GraphNeighborSeen,
    ) -> DbResult<()> {
        if limit.is_some_and(|max_rows| output.len() >= max_rows) {
            return Ok(());
        }
        let probe = aiondb_eval::coerce_value(seed_id.clone(), endpoint_type)?;
        if !use_table_adjacency {
            if let Some(index_id) =
                self.find_btree_index_for_column_ordinal(context, edge_table_id, endpoint_idx)?
            {
                return self.collect_graph_neighbors_by_index(
                    context,
                    edge_table_id,
                    index_id,
                    &probe,
                    endpoint_idx,
                    neighbor_idx,
                    limit,
                    output,
                    seen,
                );
            }
            return self.collect_graph_neighbors_by_scan(
                context,
                edge_table_id,
                &probe,
                endpoint_idx,
                neighbor_idx,
                limit,
                output,
                seen,
            );
        }
        match self.storage_dml.adjacency_neighbor_cursor(
            context.txn_id,
            &context.snapshot,
            edge_table_id,
            &probe,
            outgoing,
        ) {
            Ok(mut neighbors) => {
                if neighbors.remaining_hint() == 0
                    && !self
                        .storage_dml
                        .adjacency_index_available(context.txn_id, edge_table_id)
                {
                    return self.collect_graph_neighbors_by_scan(
                        context,
                        edge_table_id,
                        &probe,
                        endpoint_idx,
                        neighbor_idx,
                        limit,
                        output,
                        seen,
                    );
                }
                while let Some(neighbor) = neighbors.next_neighbor() {
                    if limit.is_some_and(|max_rows| output.len() >= max_rows) {
                        break;
                    }
                    context.check_deadline()?;
                    push_bigint_neighbor_with_seen(Some(&neighbor), output, seen)?;
                }
                return Ok(());
            }
            Err(err) if err.sqlstate() == SqlState::FeatureNotSupported => {}
            Err(err) => return Err(err),
        }
        match self.storage_dml.adjacency_lookup(
            context.txn_id,
            &context.snapshot,
            edge_table_id,
            &probe,
            outgoing,
        ) {
            Ok(tuple_ids) => {
                if tuple_ids.is_empty()
                    && !self
                        .storage_dml
                        .adjacency_index_available(context.txn_id, edge_table_id)
                {
                    return self.collect_graph_neighbors_by_scan(
                        context,
                        edge_table_id,
                        &probe,
                        endpoint_idx,
                        neighbor_idx,
                        limit,
                        output,
                        seen,
                    );
                }
                for tuple_id in tuple_ids {
                    if limit.is_some_and(|max_rows| output.len() >= max_rows) {
                        break;
                    }
                    context.check_deadline()?;
                    let Some(row) = self.storage_dml.fetch(
                        context.txn_id,
                        &context.snapshot,
                        edge_table_id,
                        tuple_id,
                        None,
                    )?
                    else {
                        continue;
                    };
                    push_bigint_neighbor_with_seen(row.values.get(neighbor_idx), output, seen)?;
                }
                Ok(())
            }
            Err(err) if err.sqlstate() == SqlState::FeatureNotSupported => self
                .collect_graph_neighbors_by_scan(
                    context,
                    edge_table_id,
                    &probe,
                    endpoint_idx,
                    neighbor_idx,
                    limit,
                    output,
                    seen,
                ),
            Err(err) => Err(err),
        }
    }

    fn collect_graph_neighbors_by_index<O: GraphNeighborOutput>(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        index_id: IndexId,
        probe: &Value,
        endpoint_idx: usize,
        neighbor_idx: usize,
        limit: Option<usize>,
        output: &mut O,
        seen: &mut GraphNeighborSeen,
    ) -> DbResult<()> {
        let projected_columns = self.table_column_ids_for_ordinals(
            context,
            edge_table_id,
            &[endpoint_idx, neighbor_idx],
        )?;
        let mut stream = self.scan_index_locked(
            context,
            edge_table_id,
            index_id,
            exact_lookup_key_range(probe),
            projected_columns,
        )?;
        while let Some(record) = stream.next()? {
            if limit.is_some_and(|max_rows| output.len() >= max_rows) {
                break;
            }
            context.check_deadline()?;
            push_bigint_neighbor_with_seen(record.row.values.get(1), output, seen)?;
        }
        Ok(())
    }

    fn collect_graph_neighbors_by_scan<O: GraphNeighborOutput>(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        probe: &Value,
        endpoint_idx: usize,
        neighbor_idx: usize,
        limit: Option<usize>,
        output: &mut O,
        seen: &mut GraphNeighborSeen,
    ) -> DbResult<()> {
        let projected_columns = self.table_column_ids_for_ordinals(
            context,
            edge_table_id,
            &[endpoint_idx, neighbor_idx],
        )?;
        let mut stream = self.scan_table_locked(context, edge_table_id, projected_columns)?;
        while let Some(record) = stream.next()? {
            if limit.is_some_and(|max_rows| output.len() >= max_rows) {
                break;
            }
            context.check_deadline()?;
            let current = record.row.values.first().cloned().unwrap_or(Value::Null);
            let matches = if current == *probe {
                true
            } else {
                matches!(
                    compare_runtime_values(&current, probe)?,
                    Some(std::cmp::Ordering::Equal)
                )
            };
            if !matches {
                continue;
            }
            push_bigint_neighbor_with_seen(record.row.values.get(1), output, seen)?;
        }
        Ok(())
    }
}
