use super::*;
use aiondb_core::{bounded_hnsw_ef_search, TupleId, HNSW_MAX_EF_SEARCH, VECTOR_MAX_K};
use rayon::iter::{IndexedParallelIterator, IntoParallelRefIterator, ParallelIterator};

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
struct CompiledVectorTopKFilter {
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
        let mut seen_neighbors = std::collections::HashSet::<i64>::with_capacity(capacity_hint);
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

    fn collect_graph_neighbors(
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
        output: &mut Vec<Value>,
        seen: &mut std::collections::HashSet<i64>,
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
        match self.storage_dml.adjacency_neighbors(
            context.txn_id,
            &context.snapshot,
            edge_table_id,
            &probe,
            outgoing,
        ) {
            Ok(neighbors) => {
                if neighbors.is_empty()
                    && !self
                        .storage_dml
                        .adjacency_index_has_edges(context.txn_id, edge_table_id)
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
                for neighbor in neighbors {
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
                        .adjacency_index_has_edges(context.txn_id, edge_table_id)
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

    fn collect_graph_neighbors_by_index(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        index_id: IndexId,
        probe: &Value,
        endpoint_idx: usize,
        neighbor_idx: usize,
        limit: Option<usize>,
        output: &mut Vec<Value>,
        seen: &mut std::collections::HashSet<i64>,
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

    fn collect_graph_neighbors_by_scan(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        probe: &Value,
        endpoint_idx: usize,
        neighbor_idx: usize,
        limit: Option<usize>,
        output: &mut Vec<Value>,
        seen: &mut std::collections::HashSet<i64>,
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

    pub(super) fn resolve_vector_top_k_ids(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().take(4).any(Value::is_null) {
            return Ok(Value::Array(Vec::new()));
        }
        if !(4..=10).contains(&arg_values.len()) {
            return Err(DbError::internal(
                "vector_top_k_ids() expects between 4 and 10 arguments",
            ));
        }

        let table_name = expect_text_arg(&arg_values[0], "vector_top_k_ids() table name")?;
        let vector_column = expect_text_arg(&arg_values[1], "vector_top_k_ids() column name")?;
        let k = non_negative_usize_arg(&arg_values[3], "vector_top_k_ids() k")?;
        if k == 0 {
            return Ok(Value::Array(Vec::new()));
        }
        let optional_arg = |index: usize| arg_values.get(index).filter(|value| !value.is_null());
        let metric = parse_vector_metric_arg(optional_arg(4))?;
        let ef_search_override = parse_vector_ef_search_arg(optional_arg(5))?;
        let distance_threshold = parse_vector_distance_threshold_arg(optional_arg(6))?;
        let exact = parse_vector_exact_arg(optional_arg(7))?;
        let score_threshold = parse_vector_score_threshold_arg(optional_arg(8))?;
        let option_overrides = parse_vector_top_k_options_arg(optional_arg(9))?;
        let metric = option_overrides.metric.unwrap_or(metric);
        let ef_search_override = option_overrides.ef_search.or(ef_search_override);
        let ef_search_override = vector_ef_search_or_session_default(context, ef_search_override)?;
        let distance_threshold = option_overrides.distance_threshold.or(distance_threshold);
        let exact = option_overrides.exact.unwrap_or(exact);
        let score_threshold = option_overrides.score_threshold.or(score_threshold);
        let offset = option_overrides.offset.unwrap_or(0);
        let requested_result_count = k.checked_add(offset).ok_or_else(|| {
            DbError::bind_error(
                SqlState::NumericValueOutOfRange,
                "vector_top_k_ids() k + offset is out of range",
            )
        })?;

        let Some(relation) = self.find_relation_by_name(table_name, context)? else {
            return Err(DbError::bind_error(
                SqlState::UndefinedTable,
                format!("relation \"{table_name}\" does not exist"),
            ));
        };
        let ResolvedRelation::Table(table) = relation else {
            return Err(DbError::bind_error(
                SqlState::WrongObjectType,
                format!("relation \"{table_name}\" is not a table"),
            ));
        };
        if table.columns.is_empty() {
            return Err(DbError::internal("table has no identifier column"));
        }
        let vector_ordinal = table
            .columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(vector_column))
            .ok_or_else(|| {
                DbError::bind_error(
                    SqlState::UndefinedColumn,
                    format!(
                        "column \"{vector_column}\" does not exist on relation \"{table_name}\""
                    ),
                )
            })?;
        if !matches!(
            table
                .columns
                .get(vector_ordinal)
                .map(|column| &column.data_type),
            Some(DataType::Vector { .. })
        ) {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!(
                    "column \"{vector_column}\" on relation \"{table_name}\" is not a vector column"
                ),
            ));
        }
        let vector_type = table
            .columns
            .get(vector_ordinal)
            .map(|column| column.data_type.clone())
            .ok_or_else(|| DbError::internal("vector column descriptor not found"))?;
        let query_vector = match &arg_values[2] {
            Value::Vector(vector) => vector.clone(),
            other => {
                let coerced = aiondb_eval::coerce_value(other.clone(), &vector_type)?;
                let Value::Vector(vector) = coerced else {
                    return Err(DbError::internal(
                        "vector_top_k_ids() query vector coercion did not produce a vector",
                    ));
                };
                vector
            }
        };

        let requested_metric = hybrid_vector_metric_to_distance_metric(metric);
        let payload_filter =
            self.compile_vector_top_k_filter(&table, option_overrides.filter.as_ref())?;
        let ids = if exact {
            self.collect_vector_top_k_ids_exact(
                context,
                &table,
                vector_ordinal,
                &query_vector,
                metric,
                requested_result_count,
                offset,
                distance_threshold,
                score_threshold,
                payload_filter.as_ref(),
            )?
        } else {
            match self.find_hnsw_index_for_column(
                context,
                table.table_id,
                vector_ordinal,
                requested_metric,
            )? {
                Some(index_id) => {
                    let ef_search = ef_search_override
                        .unwrap_or_else(|| bounded_hnsw_ef_search(k))
                        .min(HNSW_MAX_EF_SEARCH);
                    self.collect_vector_top_k_ids_hnsw(
                        context,
                        table.table_id,
                        index_id,
                        vector_ordinal,
                        &query_vector,
                        metric,
                        requested_result_count,
                        offset,
                        ef_search,
                        distance_threshold,
                        score_threshold,
                        payload_filter.as_ref(),
                    )?
                }
                None => self.collect_vector_top_k_ids_exact(
                    context,
                    &table,
                    vector_ordinal,
                    &query_vector,
                    metric,
                    requested_result_count,
                    offset,
                    distance_threshold,
                    score_threshold,
                    payload_filter.as_ref(),
                )?,
            }
        };

        Ok(Value::Array(ids))
    }

    pub(super) fn resolve_vector_top_k_hits(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().take(4).any(Value::is_null) {
            return Ok(Value::Array(Vec::new()));
        }
        if !(4..=10).contains(&arg_values.len()) {
            return Err(DbError::internal(
                "vector_top_k_hits() expects between 4 and 10 arguments",
            ));
        }

        let table_name = expect_text_arg(&arg_values[0], "vector_top_k_hits() table name")?;
        let vector_column = expect_text_arg(&arg_values[1], "vector_top_k_hits() column name")?;
        let k = non_negative_usize_arg(&arg_values[3], "vector_top_k_hits() k")?;
        if k == 0 {
            return Ok(Value::Array(Vec::new()));
        }
        let optional_arg = |index: usize| arg_values.get(index).filter(|value| !value.is_null());
        let metric = parse_vector_metric_arg(optional_arg(4))?;
        let ef_search_override = parse_vector_ef_search_arg(optional_arg(5))?;
        let distance_threshold = parse_vector_distance_threshold_arg(optional_arg(6))?;
        let exact = parse_vector_exact_arg(optional_arg(7))?;
        let score_threshold = parse_vector_score_threshold_arg(optional_arg(8))?;
        let option_overrides = parse_vector_top_k_options_arg(optional_arg(9))?;
        let metric = option_overrides.metric.unwrap_or(metric);
        let ef_search_override = option_overrides.ef_search.or(ef_search_override);
        let ef_search_override = vector_ef_search_or_session_default(context, ef_search_override)?;
        let distance_threshold = option_overrides.distance_threshold.or(distance_threshold);
        let exact = option_overrides.exact.unwrap_or(exact);
        let score_threshold = option_overrides.score_threshold.or(score_threshold);
        let offset = option_overrides.offset.unwrap_or(0);
        let requested_result_count = k.checked_add(offset).ok_or_else(|| {
            DbError::bind_error(
                SqlState::NumericValueOutOfRange,
                "vector_top_k_hits() k + offset is out of range",
            )
        })?;

        let Some(relation) = self.find_relation_by_name(table_name, context)? else {
            return Err(DbError::bind_error(
                SqlState::UndefinedTable,
                format!("relation \"{table_name}\" does not exist"),
            ));
        };
        let ResolvedRelation::Table(table) = relation else {
            return Err(DbError::bind_error(
                SqlState::WrongObjectType,
                format!("relation \"{table_name}\" is not a table"),
            ));
        };
        if table.columns.is_empty() {
            return Err(DbError::internal("table has no identifier column"));
        }
        let vector_ordinal = table
            .columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(vector_column))
            .ok_or_else(|| {
                DbError::bind_error(
                    SqlState::UndefinedColumn,
                    format!(
                        "column \"{vector_column}\" does not exist on relation \"{table_name}\""
                    ),
                )
            })?;
        if !matches!(
            table
                .columns
                .get(vector_ordinal)
                .map(|column| &column.data_type),
            Some(DataType::Vector { .. })
        ) {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!(
                    "column \"{vector_column}\" on relation \"{table_name}\" is not a vector column"
                ),
            ));
        }
        let vector_type = table
            .columns
            .get(vector_ordinal)
            .map(|column| column.data_type.clone())
            .ok_or_else(|| DbError::internal("vector column descriptor not found"))?;
        let query_vector = match &arg_values[2] {
            Value::Vector(vector) => vector.clone(),
            other => {
                let coerced = aiondb_eval::coerce_value(other.clone(), &vector_type)?;
                let Value::Vector(vector) = coerced else {
                    return Err(DbError::internal(
                        "vector_top_k_hits() query vector coercion did not produce a vector",
                    ));
                };
                vector
            }
        };

        let requested_metric = hybrid_vector_metric_to_distance_metric(metric);
        let payload_filter =
            self.compile_vector_top_k_filter(&table, option_overrides.filter.as_ref())?;
        let ids = if exact {
            self.collect_vector_top_k_ids_exact(
                context,
                &table,
                vector_ordinal,
                &query_vector,
                metric,
                requested_result_count,
                offset,
                distance_threshold,
                score_threshold,
                payload_filter.as_ref(),
            )?
        } else {
            match self.find_hnsw_index_for_column(
                context,
                table.table_id,
                vector_ordinal,
                requested_metric,
            )? {
                Some(index_id) => {
                    let ef_search = ef_search_override
                        .unwrap_or_else(|| bounded_hnsw_ef_search(k))
                        .min(HNSW_MAX_EF_SEARCH);
                    self.collect_vector_top_k_ids_hnsw(
                        context,
                        table.table_id,
                        index_id,
                        vector_ordinal,
                        &query_vector,
                        metric,
                        requested_result_count,
                        offset,
                        ef_search,
                        distance_threshold,
                        score_threshold,
                        payload_filter.as_ref(),
                    )?
                }
                None => self.collect_vector_top_k_ids_exact(
                    context,
                    &table,
                    vector_ordinal,
                    &query_vector,
                    metric,
                    requested_result_count,
                    offset,
                    distance_threshold,
                    score_threshold,
                    payload_filter.as_ref(),
                )?,
            }
        };

        if ids.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }
        let mut ordered_ids = Vec::with_capacity(ids.len());
        let mut seen_ids = std::collections::HashSet::with_capacity(ids.len());
        for value in ids {
            let coerced = aiondb_eval::coerce_value(value, &DataType::BigInt)?;
            let Value::BigInt(id) = coerced else {
                continue;
            };
            if seen_ids.insert(id) {
                ordered_ids.push(id);
            }
        }
        if ordered_ids.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }

        let rows_by_id = self.load_rows_by_bigint_ids(context, &table, &ordered_ids)?;

        // Resolve the (ordinal, column-name) pairs that contribute to the
        // payload ONCE, outside the per-row loop. The previous code walked
        // every column for every result row and filtered out the id /
        // vector ordinals, which is wasted work proportional to
        // (#columns × #results).
        let payload_columns: Vec<(usize, String)> = table
            .columns
            .iter()
            .enumerate()
            .filter(|(ord, _)| *ord != 0 && *ord != vector_ordinal)
            .map(|(ord, col)| (ord, col.name.clone()))
            .collect();

        // Per-id distance compute + payload build are independent. Run them
        // in parallel across rayon workers; the order is established by the
        // HNSW/exact scan above and we preserve it via index-preserving
        // `collect`. `with_min_len(32)` keeps small result sets on a single
        // worker so the SIMD per-pair cost still dominates.
        let hit_opts: Vec<Option<Value>> = ordered_ids
            .par_iter()
            .with_min_len(32)
            .map(|id| -> DbResult<Option<Value>> {
                context.check_deadline()?;
                let Some(row) = rows_by_id.get(id) else {
                    return Ok(None);
                };
                let Some(Value::Vector(candidate_vector)) = row.values.get(vector_ordinal) else {
                    return Ok(None);
                };
                let distance = compute_vector_distance(metric, candidate_vector, &query_vector)?;
                let score = vector_similarity_score(metric, distance);
                let mut payload = serde_json::Map::with_capacity(payload_columns.len().min(1024));
                for (ordinal, name) in &payload_columns {
                    let Some(value) = row.values.get(*ordinal) else {
                        continue;
                    };
                    if value.is_null() {
                        continue;
                    }
                    payload.insert(name.clone(), vector_hit_value_to_json(value));
                }
                let mut hit = serde_json::Map::with_capacity(4);
                hit.insert("id".to_owned(), serde_json::Value::Number((*id).into()));
                hit.insert("distance".to_owned(), vector_hit_json_number(distance));
                hit.insert("score".to_owned(), vector_hit_json_number(score));
                hit.insert("payload".to_owned(), serde_json::Value::Object(payload));
                Ok(Some(Value::Jsonb(serde_json::Value::Object(hit))))
            })
            .collect::<DbResult<Vec<_>>>()?;
        let hits: Vec<Value> = hit_opts.into_iter().flatten().collect();
        Ok(Value::Array(hits))
    }

    pub(super) fn resolve_vector_prefetch_top_k_hits(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if !(5..=9).contains(&arg_values.len()) {
            return Err(DbError::internal(
                "vector_prefetch_top_k_hits() expects between 5 and 9 arguments",
            ));
        }
        if arg_values
            .iter()
            .enumerate()
            .any(|(index, value)| matches!(index, 0 | 1 | 2 | 4) && value.is_null())
        {
            return Ok(Value::Array(Vec::new()));
        }

        let table_name =
            expect_text_arg(&arg_values[0], "vector_prefetch_top_k_hits() table name")?;
        let vector_column =
            expect_text_arg(&arg_values[1], "vector_prefetch_top_k_hits() column name")?;
        let prefetch_ids = parse_prefetch_hit_ids_arg(
            &arg_values[3],
            "vector_prefetch_top_k_hits() prefetch hits",
        )?;
        if prefetch_ids.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }
        let k = non_negative_usize_arg(&arg_values[4], "vector_prefetch_top_k_hits() k")?;
        if k == 0 {
            return Ok(Value::Array(Vec::new()));
        }
        let optional_arg = |index: usize| arg_values.get(index).filter(|value| !value.is_null());
        let metric = parse_vector_metric_arg(optional_arg(5))?;
        let distance_threshold = parse_vector_distance_threshold_arg(optional_arg(6))?;
        let score_threshold = parse_vector_score_threshold_arg(optional_arg(7))?;
        let option_overrides = parse_vector_top_k_options_arg(optional_arg(8))?;
        if option_overrides.ef_search.is_some() {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "vector_prefetch_top_k_hits() does not support options.ef_search",
            ));
        }
        if option_overrides.exact.is_some() {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "vector_prefetch_top_k_hits() does not support options.exact",
            ));
        }
        let metric = option_overrides.metric.unwrap_or(metric);
        let distance_threshold = option_overrides.distance_threshold.or(distance_threshold);
        let score_threshold = option_overrides.score_threshold.or(score_threshold);
        let offset = option_overrides.offset.unwrap_or(0);
        let requested_result_count = k.checked_add(offset).ok_or_else(|| {
            DbError::bind_error(
                SqlState::NumericValueOutOfRange,
                "vector_prefetch_top_k_hits() k + offset is out of range",
            )
        })?;

        let Some(relation) = self.find_relation_by_name(table_name, context)? else {
            return Err(DbError::bind_error(
                SqlState::UndefinedTable,
                format!("relation \"{table_name}\" does not exist"),
            ));
        };
        let ResolvedRelation::Table(table) = relation else {
            return Err(DbError::bind_error(
                SqlState::WrongObjectType,
                format!("relation \"{table_name}\" is not a table"),
            ));
        };
        if table.columns.is_empty() {
            return Err(DbError::internal("table has no identifier column"));
        }
        let vector_ordinal = table
            .columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(vector_column))
            .ok_or_else(|| {
                DbError::bind_error(
                    SqlState::UndefinedColumn,
                    format!(
                        "column \"{vector_column}\" does not exist on relation \"{table_name}\""
                    ),
                )
            })?;
        if !matches!(
            table
                .columns
                .get(vector_ordinal)
                .map(|column| &column.data_type),
            Some(DataType::Vector { .. })
        ) {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!(
                    "column \"{vector_column}\" on relation \"{table_name}\" is not a vector column"
                ),
            ));
        }
        let vector_type = table
            .columns
            .get(vector_ordinal)
            .map(|column| column.data_type.clone())
            .ok_or_else(|| DbError::internal("vector column descriptor not found"))?;
        let query_vector = match &arg_values[2] {
            Value::Vector(vector) => vector.clone(),
            other => {
                let coerced = aiondb_eval::coerce_value(other.clone(), &vector_type)?;
                let Value::Vector(vector) = coerced else {
                    return Err(DbError::internal(
                        "vector_prefetch_top_k_hits() query vector coercion did not produce a vector",
                    ));
                };
                vector
            }
        };
        let payload_filter =
            self.compile_vector_top_k_filter(&table, option_overrides.filter.as_ref())?;
        let target_ids: std::collections::HashSet<i64> = prefetch_ids.into_iter().collect();
        let target_id_list = target_ids.iter().copied().collect::<Vec<_>>();
        let rows_by_id = self.load_rows_by_bigint_ids(context, &table, &target_id_list)?;

        // Per-id scoring is fully independent: SIMD distance, optional
        // payload-filter predicate, payload assembly. Run candidates in
        // parallel via rayon. `with_min_len(32)` keeps very small prefetch
        // sets (< 32 ids) on a single worker so the per-id SIMD cost still
        // dominates rayon overhead. `context` / `payload_filter` / `table`
        // / `query_vector` / `rows_by_id` are all `Sync` (built on
        // `Arc<…>` + `Mutex`/`RwLock` or plain immutable data), so each
        // worker can read them without coordination.
        let payload_filter_ref = payload_filter.as_ref();
        // Resolve payload columns once outside the per-id loop (see the
        // same pattern in `resolve_vector_top_k_hits`).
        let payload_columns: Vec<(usize, String)> = table
            .columns
            .iter()
            .enumerate()
            .filter(|(ord, _)| *ord != 0 && *ord != vector_ordinal)
            .map(|(ord, col)| (ord, col.name.clone()))
            .collect();
        let scored_opts: Vec<
            Option<(
                f64,
                i64,
                f64,
                f64,
                serde_json::Map<String, serde_json::Value>,
            )>,
        > = target_id_list
            .par_iter()
            .with_min_len(32)
            .map(
                |id| -> DbResult<
                    Option<(
                        f64,
                        i64,
                        f64,
                        f64,
                        serde_json::Map<String, serde_json::Value>,
                    )>,
                > {
                    context.check_deadline()?;
                    let Some(row) = rows_by_id.get(id) else {
                        return Ok(None);
                    };
                    if payload_filter_ref.is_some_and(|filter| !filter.matches(row)) {
                        return Ok(None);
                    }
                    let Some(Value::Vector(candidate_vector)) = row.values.get(vector_ordinal)
                    else {
                        return Ok(None);
                    };
                    let distance =
                        compute_vector_distance(metric, candidate_vector, &query_vector)?;
                    if !vector_candidate_passes_thresholds(
                        metric,
                        distance,
                        distance_threshold,
                        score_threshold,
                    ) {
                        return Ok(None);
                    }
                    let score = vector_similarity_score(metric, distance);
                    let mut payload =
                        serde_json::Map::with_capacity(payload_columns.len().min(1024));
                    for (ordinal, name) in &payload_columns {
                        let Some(value) = row.values.get(*ordinal) else {
                            continue;
                        };
                        if value.is_null() {
                            continue;
                        }
                        payload.insert(name.clone(), vector_hit_value_to_json(value));
                    }
                    let sortable_distance = if distance.is_nan() {
                        f64::INFINITY
                    } else {
                        distance
                    };
                    Ok(Some((sortable_distance, *id, distance, score, payload)))
                },
            )
            .collect::<DbResult<Vec<_>>>()?;
        let mut scored: Vec<(
            f64,
            i64,
            f64,
            f64,
            serde_json::Map<String, serde_json::Value>,
        )> = scored_opts.into_iter().flatten().collect();

        scored.sort_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
        });
        scored.truncate(requested_result_count);

        let final_count = requested_result_count.saturating_sub(offset);
        let mut hits = Vec::with_capacity(final_count);
        for (_, id, distance, score, payload) in scored.into_iter().skip(offset).take(final_count) {
            context.check_deadline()?;
            let mut hit = serde_json::Map::new();
            hit.insert("id".to_owned(), serde_json::Value::Number(id.into()));
            hit.insert("distance".to_owned(), vector_hit_json_number(distance));
            hit.insert("score".to_owned(), vector_hit_json_number(score));
            hit.insert("payload".to_owned(), serde_json::Value::Object(payload));
            hits.push(Value::Jsonb(serde_json::Value::Object(hit)));
        }
        Ok(Value::Array(hits))
    }

    pub(super) fn resolve_hybrid_fuse_rrf_hits(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if !(3..=6).contains(&arg_values.len()) {
            return Err(DbError::internal(
                "hybrid_fuse_rrf_hits() expects between 3 and 6 arguments",
            ));
        }

        let dense_hits = parse_rrf_hits_arg(
            arg_values
                .first()
                .ok_or_else(|| DbError::internal("hybrid_fuse_rrf_hits() missing dense hits"))?,
            "hybrid_fuse_rrf_hits() dense hits",
        )?;
        let sparse_hits = parse_rrf_hits_arg(
            arg_values
                .get(1)
                .ok_or_else(|| DbError::internal("hybrid_fuse_rrf_hits() missing sparse hits"))?,
            "hybrid_fuse_rrf_hits() sparse hits",
        )?;
        let k = non_negative_usize_arg(
            arg_values
                .get(2)
                .ok_or_else(|| DbError::internal("hybrid_fuse_rrf_hits() missing k"))?,
            "hybrid_fuse_rrf_hits() k",
        )?;
        if k == 0 {
            return Ok(Value::Array(Vec::new()));
        }
        let dense_weight =
            parse_rrf_weight_arg(arg_values.get(3), "hybrid_fuse_rrf_hits() dense_weight")?;
        let sparse_weight =
            parse_rrf_weight_arg(arg_values.get(4), "hybrid_fuse_rrf_hits() sparse_weight")?;
        let rrf_k = arg_values
            .get(5)
            .map(|value| non_negative_usize_arg(value, "hybrid_fuse_rrf_hits() rrf_k"))
            .transpose()?
            .unwrap_or(60);
        let rrf_k = if rrf_k == 0 { 1 } else { rrf_k };

        let mut fused = std::collections::BTreeMap::<i64, HybridRrfFusionEntry>::new();
        let mut seen_dense = std::collections::HashSet::new();
        for (rank, hit) in dense_hits.iter().enumerate() {
            context.check_deadline()?;
            let id = read_hit_id(hit, "hybrid_fuse_rrf_hits() dense hits")?;
            if !seen_dense.insert(id) {
                continue;
            }
            let rank_1 = rank.saturating_add(1);
            let denom = usize_to_f64(rrf_k.saturating_add(rank_1));
            let contribution = if denom == 0.0 {
                0.0
            } else {
                dense_weight / denom
            };
            let entry = fused.entry(id).or_default();
            entry.fused_score += contribution;
            entry.dense_rank = Some(rank_1);
            entry.dense_score = hit.get("score").and_then(serde_json::Value::as_f64);
            entry.dense_distance = hit.get("distance").and_then(serde_json::Value::as_f64);
            if entry.payload.is_none() {
                entry.payload = hit.get("payload").cloned();
            }
        }

        let mut seen_sparse = std::collections::HashSet::new();
        for (rank, hit) in sparse_hits.iter().enumerate() {
            context.check_deadline()?;
            let id = read_hit_id(hit, "hybrid_fuse_rrf_hits() sparse hits")?;
            if !seen_sparse.insert(id) {
                continue;
            }
            let rank_1 = rank.saturating_add(1);
            let denom = usize_to_f64(rrf_k.saturating_add(rank_1));
            let contribution = if denom == 0.0 {
                0.0
            } else {
                sparse_weight / denom
            };
            let entry = fused.entry(id).or_default();
            entry.fused_score += contribution;
            entry.sparse_rank = Some(rank_1);
            entry.sparse_score = hit.get("score").and_then(serde_json::Value::as_f64);
            entry.sparse_distance = hit.get("distance").and_then(serde_json::Value::as_f64);
            if entry.payload.is_none() {
                entry.payload = hit.get("payload").cloned();
            }
        }

        let mut ordered: Vec<(i64, HybridRrfFusionEntry)> = fused.into_iter().collect();
        ordered.sort_by(|left, right| {
            right
                .1
                .fused_score
                .total_cmp(&left.1.fused_score)
                .then_with(|| left.0.cmp(&right.0))
        });

        let mut hits = Vec::new();
        for (id, entry) in ordered.into_iter().take(k) {
            context.check_deadline()?;
            let mut object = serde_json::Map::new();
            object.insert("id".to_owned(), serde_json::Value::Number(id.into()));
            object.insert(
                "fused_score".to_owned(),
                vector_hit_json_number(entry.fused_score),
            );
            if let Some(rank) = entry.dense_rank {
                let mut dense = serde_json::Map::new();
                dense.insert(
                    "rank".to_owned(),
                    serde_json::Value::Number(usize_to_i64(rank).into()),
                );
                if let Some(score) = entry.dense_score {
                    dense.insert("score".to_owned(), vector_hit_json_number(score));
                }
                if let Some(distance) = entry.dense_distance {
                    dense.insert("distance".to_owned(), vector_hit_json_number(distance));
                }
                object.insert("dense".to_owned(), serde_json::Value::Object(dense));
            }
            if let Some(rank) = entry.sparse_rank {
                let mut sparse = serde_json::Map::new();
                sparse.insert(
                    "rank".to_owned(),
                    serde_json::Value::Number(usize_to_i64(rank).into()),
                );
                if let Some(score) = entry.sparse_score {
                    sparse.insert("score".to_owned(), vector_hit_json_number(score));
                }
                if let Some(distance) = entry.sparse_distance {
                    sparse.insert("distance".to_owned(), vector_hit_json_number(distance));
                }
                object.insert("sparse".to_owned(), serde_json::Value::Object(sparse));
            }
            if let Some(payload) = entry.payload {
                object.insert("payload".to_owned(), payload);
            }
            hits.push(Value::Jsonb(serde_json::Value::Object(object)));
        }
        Ok(Value::Array(hits))
    }

    pub(super) fn resolve_vector_recommend_top_k_hits(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if !(5..=11).contains(&arg_values.len()) {
            return Err(DbError::internal(
                "vector_recommend_top_k_hits() expects between 5 and 11 arguments",
            ));
        }
        if arg_values
            .iter()
            .enumerate()
            .any(|(index, value)| matches!(index, 0 | 1 | 2 | 4) && value.is_null())
        {
            return Ok(Value::Array(Vec::new()));
        }

        let table_name =
            expect_text_arg(&arg_values[0], "vector_recommend_top_k_hits() table name")?;
        let vector_column =
            expect_text_arg(&arg_values[1], "vector_recommend_top_k_hits() column name")?;
        let k = non_negative_usize_arg(&arg_values[4], "vector_recommend_top_k_hits() k")?;
        if k == 0 {
            return Ok(Value::Array(Vec::new()));
        }
        let optional_arg = |index: usize| arg_values.get(index).filter(|value| !value.is_null());
        let metric = parse_vector_metric_arg(optional_arg(5))?;
        let ef_search_override = parse_vector_ef_search_arg(optional_arg(6))?;
        let distance_threshold = parse_vector_distance_threshold_arg(optional_arg(7))?;
        let exact = parse_vector_exact_arg(optional_arg(8))?;
        let score_threshold = parse_vector_score_threshold_arg(optional_arg(9))?;
        let option_overrides = parse_vector_top_k_options_arg(optional_arg(10))?;
        let metric = option_overrides.metric.unwrap_or(metric);
        let ef_search_override = option_overrides.ef_search.or(ef_search_override);
        let ef_search_override = vector_ef_search_or_session_default(context, ef_search_override)?;
        let distance_threshold = option_overrides.distance_threshold.or(distance_threshold);
        let exact = option_overrides.exact.unwrap_or(exact);
        let score_threshold = option_overrides.score_threshold.or(score_threshold);
        let offset = option_overrides.offset.unwrap_or(0);
        let requested_result_count = k.checked_add(offset).ok_or_else(|| {
            DbError::bind_error(
                SqlState::NumericValueOutOfRange,
                "vector_recommend_top_k_hits() k + offset is out of range",
            )
        })?;

        let Some(relation) = self.find_relation_by_name(table_name, context)? else {
            return Err(DbError::bind_error(
                SqlState::UndefinedTable,
                format!("relation \"{table_name}\" does not exist"),
            ));
        };
        let ResolvedRelation::Table(table) = relation else {
            return Err(DbError::bind_error(
                SqlState::WrongObjectType,
                format!("relation \"{table_name}\" is not a table"),
            ));
        };
        if table.columns.is_empty() {
            return Err(DbError::internal("table has no identifier column"));
        }
        let vector_ordinal = table
            .columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(vector_column))
            .ok_or_else(|| {
                DbError::bind_error(
                    SqlState::UndefinedColumn,
                    format!(
                        "column \"{vector_column}\" does not exist on relation \"{table_name}\""
                    ),
                )
            })?;
        if !matches!(
            table
                .columns
                .get(vector_ordinal)
                .map(|column| &column.data_type),
            Some(DataType::Vector { .. })
        ) {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!(
                    "column \"{vector_column}\" on relation \"{table_name}\" is not a vector column"
                ),
            ));
        }
        let vector_type = table
            .columns
            .get(vector_ordinal)
            .map(|column| column.data_type.clone())
            .ok_or_else(|| DbError::internal("vector column descriptor not found"))?;
        let vector_dims = vector_dims_from_type(
            &vector_type,
            "vector_recommend_top_k_hits() positive examples",
        )?;
        let positive_specs = parse_recommend_example_specs(
            &arg_values[2],
            &vector_type,
            vector_dims,
            "vector_recommend_top_k_hits() positive examples",
        )?;
        let negative_specs = parse_recommend_example_specs(
            arg_values.get(3).unwrap_or(&Value::Null),
            &vector_type,
            vector_dims,
            "vector_recommend_top_k_hits() negative examples",
        )?;

        let mut id_examples = collect_recommend_example_ids(&positive_specs);
        id_examples.extend(collect_recommend_example_ids(&negative_specs));
        let mut id_vectors =
            std::collections::HashMap::<i64, aiondb_core::VectorValue>::with_capacity(
                id_examples.len(),
            );
        if !id_examples.is_empty() {
            let example_id_list = id_examples.iter().copied().collect::<Vec<_>>();
            let rows_by_id = self.load_rows_by_bigint_ids(context, &table, &example_id_list)?;
            for id in &example_id_list {
                context.check_deadline()?;
                let Some(row) = rows_by_id.get(id) else {
                    continue;
                };
                if id_vectors.contains_key(id) {
                    continue;
                }
                let Some(Value::Vector(vector)) = row.values.get(vector_ordinal) else {
                    continue;
                };
                id_vectors.insert(*id, vector.clone());
            }
        }

        let positive_vectors = materialize_recommend_vectors(
            &positive_specs,
            &id_vectors,
            vector_dims,
            "vector_recommend_top_k_hits() positive examples",
        )?;
        let negative_vectors = materialize_recommend_vectors(
            &negative_specs,
            &id_vectors,
            vector_dims,
            "vector_recommend_top_k_hits() negative examples",
        )?;
        let positive_centroid = centroid_vector(
            &positive_vectors,
            vector_dims,
            "vector_recommend_top_k_hits() positive examples",
        )?;
        let negative_centroid = if negative_vectors.is_empty() {
            None
        } else {
            Some(centroid_vector(
                &negative_vectors,
                vector_dims,
                "vector_recommend_top_k_hits() negative examples",
            )?)
        };

        let mut query_values = positive_centroid.values.clone();
        if let Some(negative) = negative_centroid.as_ref() {
            for (index, value) in query_values.iter_mut().enumerate() {
                *value -= negative.values.get(index).copied().unwrap_or(0.0);
            }
        }
        let query_vector = aiondb_core::VectorValue::new(positive_centroid.dims, query_values);

        let requested_metric = hybrid_vector_metric_to_distance_metric(metric);
        let payload_filter =
            self.compile_vector_top_k_filter(&table, option_overrides.filter.as_ref())?;
        let ids = if exact {
            self.collect_vector_top_k_ids_exact(
                context,
                &table,
                vector_ordinal,
                &query_vector,
                metric,
                requested_result_count,
                offset,
                distance_threshold,
                score_threshold,
                payload_filter.as_ref(),
            )?
        } else {
            match self.find_hnsw_index_for_column(
                context,
                table.table_id,
                vector_ordinal,
                requested_metric,
            )? {
                Some(index_id) => {
                    let ef_search = ef_search_override
                        .unwrap_or_else(|| bounded_hnsw_ef_search(k))
                        .min(HNSW_MAX_EF_SEARCH);
                    self.collect_vector_top_k_ids_hnsw(
                        context,
                        table.table_id,
                        index_id,
                        vector_ordinal,
                        &query_vector,
                        metric,
                        requested_result_count,
                        offset,
                        ef_search,
                        distance_threshold,
                        score_threshold,
                        payload_filter.as_ref(),
                    )?
                }
                None => self.collect_vector_top_k_ids_exact(
                    context,
                    &table,
                    vector_ordinal,
                    &query_vector,
                    metric,
                    requested_result_count,
                    offset,
                    distance_threshold,
                    score_threshold,
                    payload_filter.as_ref(),
                )?,
            }
        };

        if ids.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }
        let mut ordered_ids = Vec::with_capacity(ids.len());
        let mut seen_ids = std::collections::HashSet::with_capacity(ids.len());
        for value in ids {
            let coerced = aiondb_eval::coerce_value(value, &DataType::BigInt)?;
            let Value::BigInt(id) = coerced else {
                continue;
            };
            if seen_ids.insert(id) {
                ordered_ids.push(id);
            }
        }
        if ordered_ids.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }

        let rows_by_id = self.load_rows_by_bigint_ids(context, &table, &ordered_ids)?;

        // Resolve payload column list once outside the per-id loop (see
        // `resolve_vector_top_k_hits`).
        let payload_columns: Vec<(usize, String)> = table
            .columns
            .iter()
            .enumerate()
            .filter(|(ord, _)| *ord != 0 && *ord != vector_ordinal)
            .map(|(ord, col)| (ord, col.name.clone()))
            .collect();

        // Parallel scoring + payload assembly, identical pattern to
        // `resolve_vector_top_k_hits`. `with_min_len(32)` guards small
        // recommend result sets.
        let hit_opts: Vec<Option<Value>> = ordered_ids
            .par_iter()
            .with_min_len(32)
            .map(|id| -> DbResult<Option<Value>> {
                context.check_deadline()?;
                let Some(row) = rows_by_id.get(id) else {
                    return Ok(None);
                };
                let Some(Value::Vector(candidate_vector)) = row.values.get(vector_ordinal) else {
                    return Ok(None);
                };
                let distance = compute_vector_distance(metric, candidate_vector, &query_vector)?;
                let score = vector_similarity_score(metric, distance);
                let mut payload = serde_json::Map::with_capacity(payload_columns.len().min(1024));
                for (ordinal, name) in &payload_columns {
                    let Some(value) = row.values.get(*ordinal) else {
                        continue;
                    };
                    if value.is_null() {
                        continue;
                    }
                    payload.insert(name.clone(), vector_hit_value_to_json(value));
                }
                let mut hit = serde_json::Map::with_capacity(4);
                hit.insert("id".to_owned(), serde_json::Value::Number((*id).into()));
                hit.insert("distance".to_owned(), vector_hit_json_number(distance));
                hit.insert("score".to_owned(), vector_hit_json_number(score));
                hit.insert("payload".to_owned(), serde_json::Value::Object(payload));
                Ok(Some(Value::Jsonb(serde_json::Value::Object(hit))))
            })
            .collect::<DbResult<Vec<_>>>()?;
        let hits: Vec<Value> = hit_opts.into_iter().flatten().collect();
        Ok(Value::Array(hits))
    }

    pub(super) fn resolve_full_text_top_k_hits(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().take(4).any(Value::is_null) {
            return Ok(Value::Array(Vec::new()));
        }
        if !(4..=8).contains(&arg_values.len()) {
            return Err(DbError::internal(
                "full_text_top_k_hits() expects between 4 and 8 arguments",
            ));
        }

        let table_name = expect_text_arg(&arg_values[0], "full_text_top_k_hits() table name")?;
        let text_column = expect_text_arg(&arg_values[1], "full_text_top_k_hits() column name")?;
        let query_text = expect_text_arg(&arg_values[2], "full_text_top_k_hits() query text")?;
        let k = non_negative_usize_arg(&arg_values[3], "full_text_top_k_hits() k")?;
        if k == 0 {
            return Ok(Value::Array(Vec::new()));
        }
        let optional_arg = |index: usize| arg_values.get(index).filter(|value| !value.is_null());
        let query_mode = parse_full_text_query_mode_arg(optional_arg(4))?;
        let config = parse_full_text_config_arg(optional_arg(5))?;
        let score_threshold = parse_full_text_score_threshold_arg(optional_arg(6))?;
        let option_overrides = parse_full_text_top_k_options_arg(optional_arg(7))?;
        let query_mode = option_overrides.query_mode.unwrap_or(query_mode);
        let config = option_overrides.config.unwrap_or(config);
        let score_threshold = option_overrides.score_threshold.or(score_threshold);
        let offset = option_overrides.offset.unwrap_or(0);
        let payload_filter_spec = option_overrides.filter.as_ref();
        let requested_result_count = k.checked_add(offset).ok_or_else(|| {
            DbError::bind_error(
                SqlState::NumericValueOutOfRange,
                "full_text_top_k_hits() k + offset is out of range",
            )
        })?;

        let Some(relation) = self.find_relation_by_name(table_name, context)? else {
            return Err(DbError::bind_error(
                SqlState::UndefinedTable,
                format!("relation \"{table_name}\" does not exist"),
            ));
        };
        let ResolvedRelation::Table(table) = relation else {
            return Err(DbError::bind_error(
                SqlState::WrongObjectType,
                format!("relation \"{table_name}\" is not a table"),
            ));
        };
        if table.columns.is_empty() {
            return Err(DbError::internal("table has no identifier column"));
        }
        let text_ordinal = table
            .columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(text_column))
            .ok_or_else(|| {
                DbError::bind_error(
                    SqlState::UndefinedColumn,
                    format!("column \"{text_column}\" does not exist on relation \"{table_name}\""),
                )
            })?;
        if !matches!(
            table
                .columns
                .get(text_ordinal)
                .map(|column| &column.data_type),
            Some(DataType::Text)
        ) {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!(
                    "column \"{text_column}\" on relation \"{table_name}\" is not a text column"
                ),
            ));
        }
        let payload_filter = self.compile_vector_top_k_filter(&table, payload_filter_spec)?;

        let config_expr = TypedExpr::literal(Value::Text(config.clone()), DataType::Text, false);
        let query_expr =
            TypedExpr::literal(Value::Text(query_text.to_owned()), DataType::Text, false);
        let tsquery_expr = match query_mode {
            FullTextQueryMode::Plain => TypedExpr::scalar_function(
                ScalarFunction::PlaintoTsquery,
                vec![config_expr.clone(), query_expr],
                DataType::Text,
                true,
            ),
            FullTextQueryMode::Phrase => TypedExpr::scalar_function(
                ScalarFunction::PhrasetoTsquery,
                vec![config_expr.clone(), query_expr],
                DataType::Text,
                true,
            ),
            FullTextQueryMode::Websearch => TypedExpr::scalar_function(
                ScalarFunction::WebsearchToTsquery,
                vec![config_expr.clone(), query_expr],
                DataType::Text,
                true,
            ),
            FullTextQueryMode::Raw => TypedExpr::scalar_function(
                ScalarFunction::ToTsquery,
                vec![config_expr.clone(), query_expr],
                DataType::Text,
                true,
            ),
        };
        let resolved_tsquery = self.evaluate_expr(&tsquery_expr, context)?;
        let tsquery_text = match &resolved_tsquery {
            Value::Text(text) if !text.trim().is_empty() => text.clone(),
            _ => return Ok(Value::Array(Vec::new())),
        };
        let can_use_conjunctive_gin_prefilter = match query_mode {
            FullTextQueryMode::Plain | FullTextQueryMode::Phrase => true,
            FullTextQueryMode::Websearch | FullTextQueryMode::Raw => {
                !tsquery_has_disjunction_or_negation(&tsquery_text)
            }
        };
        let mut gin_prefilter = None;
        let stream = if can_use_conjunctive_gin_prefilter {
            let prefilter_terms = extract_quoted_tsquery_terms(&tsquery_text);
            if prefilter_terms.is_empty() {
                self.scan_table_locked(context, table.table_id, None)?
            } else if let Some(index_id) =
                self.find_gin_index_for_column(context, table.table_id, text_ordinal)?
            {
                let mut pattern_object = serde_json::Map::new();
                for term in prefilter_terms {
                    pattern_object.insert(term, serde_json::Value::Object(serde_json::Map::new()));
                }
                let pattern = serde_json::Value::Object(pattern_object);
                let use_limited_probe = payload_filter.is_none()
                    && score_threshold.map_or(true, |threshold| threshold <= FULL_TEXT_MAX_RANK);
                gin_prefilter = Some((index_id, pattern.clone(), use_limited_probe));
                self.gin_containment_search_locked(
                    context,
                    table.table_id,
                    index_id,
                    &pattern,
                    use_limited_probe.then_some(requested_result_count),
                )?
            } else {
                self.scan_table_locked(context, table.table_id, None)?
            }
        } else {
            self.scan_table_locked(context, table.table_id, None)?
        };
        // Resolve payload columns ONCE. The closure below runs the stream
        // per (possibly retried) scan path; capturing the precomputed list
        // avoids walking every column for every retained candidate row.
        let payload_columns: Vec<(usize, String)> = table
            .columns
            .iter()
            .enumerate()
            .filter(|(ord, _)| *ord != 0 && *ord != text_ordinal)
            .map(|(ord, col)| (ord, col.name.clone()))
            .collect();
        let payload_columns_ref = &payload_columns;
        let process_stream =
            |mut stream: Box<dyn TupleStream>,
             stream_is_tuple_id_ascending: bool|
             -> DbResult<(std::collections::BinaryHeap<FullTextTopHit>, bool)> {
                let mut top_hits = std::collections::BinaryHeap::<FullTextTopHit>::new();
                let mut id_matches_tuple_order = stream_is_tuple_id_ascending;
                let mut last_ordered_id: Option<i64> = None;
                while let Some(record) = stream.next()? {
                    context.check_deadline()?;
                    let id = aiondb_eval::coerce_value(
                        record.row.values.first().cloned().unwrap_or(Value::Null),
                        &DataType::BigInt,
                    )?;
                    let Value::BigInt(id) = id else {
                        continue;
                    };
                    if id_matches_tuple_order {
                        match last_ordered_id {
                            Some(previous) if id < previous => {
                                id_matches_tuple_order = false;
                            }
                            Some(_) => {}
                            None => {
                                last_ordered_id = Some(id);
                            }
                        }
                        if id_matches_tuple_order {
                            last_ordered_id = Some(id);
                        }
                    }
                    if payload_filter
                        .as_ref()
                        .is_some_and(|filter| !filter.matches(&record.row))
                    {
                        continue;
                    }

                    let Some(Value::Text(document)) = record.row.values.get(text_ordinal) else {
                        continue;
                    };
                    let Some(score) =
                        aiondb_eval::eval_full_text_match_rank(&config, document, &tsquery_text)?
                    else {
                        continue;
                    };
                    let score = f64::from(score);
                    if score_threshold.is_some_and(|threshold| score < threshold) {
                        continue;
                    }
                    let keep_candidate = if top_hits.len() < requested_result_count {
                        true
                    } else {
                        top_hits
                            .peek()
                            .is_some_and(|worst| full_text_hit_is_better(score, id, worst))
                    };
                    if !keep_candidate {
                        continue;
                    }

                    let mut payload =
                        serde_json::Map::with_capacity(payload_columns_ref.len().min(1024));
                    for (ordinal, name) in payload_columns_ref {
                        let Some(value) = record.row.values.get(*ordinal) else {
                            continue;
                        };
                        if value.is_null() {
                            continue;
                        }
                        payload.insert(name.clone(), vector_hit_value_to_json(value));
                    }
                    let hit = FullTextTopHit { score, id, payload };
                    if top_hits.len() < requested_result_count {
                        top_hits.push(hit);
                    } else if let Some(mut worst) = top_hits.peek_mut() {
                        if full_text_hit_is_better(hit.score, hit.id, &worst) {
                            *worst = hit;
                        }
                    }
                    if id_matches_tuple_order
                        && top_hits.len() >= requested_result_count
                        && top_hits
                            .peek()
                            .is_some_and(|worst| worst.score >= FULL_TEXT_MAX_RANK)
                    {
                        return Ok((top_hits, true));
                    }
                }
                Ok((top_hits, false))
            };
        let (mut top_hits, early_satisfied) = process_stream(stream, gin_prefilter.is_some())?;
        if let Some((index_id, pattern, true)) = gin_prefilter {
            if !early_satisfied {
                let full_stream = self.gin_containment_search_locked(
                    context,
                    table.table_id,
                    index_id,
                    &pattern,
                    None,
                )?;
                (top_hits, _) = process_stream(full_stream, true)?;
            }
        }

        let mut scored = top_hits.into_vec();
        scored.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.id.cmp(&right.id))
        });

        let final_count = requested_result_count.saturating_sub(offset);
        let mut hits = Vec::with_capacity(final_count);
        for top_hit in scored.into_iter().skip(offset).take(final_count) {
            context.check_deadline()?;
            let mut hit = serde_json::Map::new();
            hit.insert(
                "id".to_owned(),
                serde_json::Value::Number(top_hit.id.into()),
            );
            hit.insert("score".to_owned(), vector_hit_json_number(top_hit.score));
            hit.insert(
                "payload".to_owned(),
                serde_json::Value::Object(top_hit.payload),
            );
            hits.push(Value::Jsonb(serde_json::Value::Object(hit)));
        }
        Ok(Value::Array(hits))
    }

    pub(super) fn resolve_hybrid_search_top_k_hits(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().take(6).any(Value::is_null) {
            return Ok(Value::Array(Vec::new()));
        }
        if !(6..=7).contains(&arg_values.len()) {
            return Err(DbError::internal(
                "hybrid_search_top_k_hits() expects 6 or 7 arguments",
            ));
        }

        let table_name = expect_text_arg(&arg_values[0], "hybrid_search_top_k_hits() table name")?;
        let vector_column =
            expect_text_arg(&arg_values[1], "hybrid_search_top_k_hits() vector column")?;
        let text_column =
            expect_text_arg(&arg_values[2], "hybrid_search_top_k_hits() text column")?;
        let vector_query = arg_values
            .get(3)
            .cloned()
            .ok_or_else(|| DbError::internal("hybrid_search_top_k_hits() missing vector query"))?;
        let text_query = expect_text_arg(&arg_values[4], "hybrid_search_top_k_hits() text query")?;
        let k = non_negative_usize_arg(&arg_values[5], "hybrid_search_top_k_hits() k")?;
        if k == 0 {
            return Ok(Value::Array(Vec::new()));
        }

        let options = parse_hybrid_search_top_k_options_arg(
            arg_values.get(6).filter(|value| !value.is_null()),
        )?;
        let fusion = options.fusion.unwrap_or(HybridSearchFusionMethod::Rrf);
        let dense_weight = options.dense_weight.unwrap_or(1.0);
        let sparse_weight = options.sparse_weight.unwrap_or(1.0);
        let rrf_k = options.rrf_k.unwrap_or(60).max(1);
        let offset = options.offset.unwrap_or(0);
        let requested_result_count = k.checked_add(offset).ok_or_else(|| {
            DbError::bind_error(
                SqlState::NumericValueOutOfRange,
                "hybrid_search_top_k_hits() k + offset is out of range",
            )
        })?;
        let default_source_k = requested_result_count.max(10).saturating_mul(4);
        let source_k = options
            .source_k
            .unwrap_or(default_source_k)
            .max(requested_result_count)
            .max(1);
        let vector_source_k = source_k.min(VECTOR_MAX_K).max(1);

        let usize_to_bigint = |value: usize, arg_name: &str| -> DbResult<Value> {
            let value = i64::try_from(value).map_err(|_| {
                DbError::bind_error(
                    SqlState::NumericValueOutOfRange,
                    format!("{arg_name} is out of range"),
                )
            })?;
            Ok(Value::BigInt(value))
        };

        let filter_json = options
            .filter
            .as_ref()
            .map(vector_top_k_filter_spec_to_json);
        let dense_options_json = filter_json.as_ref().map(|filter| {
            let mut object = serde_json::Map::new();
            object.insert("filter".to_owned(), filter.clone());
            serde_json::Value::Object(object)
        });
        let sparse_options_json = dense_options_json.clone();

        let dense_args = vec![
            literal_expr_from_value(Value::Text(table_name.to_owned())),
            literal_expr_from_value(Value::Text(vector_column.to_owned())),
            literal_expr_from_value(vector_query),
            literal_expr_from_value(usize_to_bigint(
                vector_source_k,
                "hybrid_search_top_k_hits() source_k",
            )?),
            literal_expr_from_value(
                options
                    .metric
                    .map(|metric| Value::Text(hybrid_vector_metric_name(metric).to_owned()))
                    .unwrap_or(Value::Null),
            ),
            literal_expr_from_value(
                options
                    .vector_ef_search
                    .map(|ef| usize_to_bigint(ef, "hybrid_search_top_k_hits() vector_ef_search"))
                    .transpose()?
                    .unwrap_or(Value::Null),
            ),
            literal_expr_from_value(
                options
                    .vector_distance_threshold
                    .map(Value::Double)
                    .unwrap_or(Value::Null),
            ),
            literal_expr_from_value(
                options
                    .vector_exact
                    .map(Value::Boolean)
                    .unwrap_or(Value::Null),
            ),
            literal_expr_from_value(
                options
                    .vector_score_threshold
                    .map(Value::Double)
                    .unwrap_or(Value::Null),
            ),
            literal_expr_from_value(dense_options_json.map(Value::Jsonb).unwrap_or(Value::Null)),
        ];

        let sparse_args = vec![
            literal_expr_from_value(Value::Text(table_name.to_owned())),
            literal_expr_from_value(Value::Text(text_column.to_owned())),
            literal_expr_from_value(Value::Text(text_query.to_owned())),
            literal_expr_from_value(usize_to_bigint(
                source_k,
                "hybrid_search_top_k_hits() source_k",
            )?),
            literal_expr_from_value(Value::Text(
                full_text_query_mode_name(options.query_mode.unwrap_or(FullTextQueryMode::Plain))
                    .to_owned(),
            )),
            literal_expr_from_value(Value::Text(
                options.config.unwrap_or_else(|| "english".to_owned()),
            )),
            literal_expr_from_value(
                options
                    .text_score_threshold
                    .map(Value::Double)
                    .unwrap_or(Value::Null),
            ),
            literal_expr_from_value(sparse_options_json.map(Value::Jsonb).unwrap_or(Value::Null)),
        ];

        // Dense vector search and sparse full-text search are independent —
        // they touch different indexes and only their fused output depends
        // on both. Run them in parallel via `rayon::join`. The executor and
        // its catalog/storage handles are `Send + Sync` (built on
        // `Arc<dyn …>` + `Mutex`/`RwLock`), and the executor already runs
        // concurrent calls under a single `ExecutionContext` for the
        // parallel-query Gather path.
        let (dense_result, sparse_result) = rayon::join(
            || self.resolve_vector_top_k_hits(&dense_args, outer_row, context),
            || self.resolve_full_text_top_k_hits(&sparse_args, outer_row, context),
        );
        let dense_hits = dense_result?;
        let sparse_hits = sparse_result?;

        let requested_i64 = i64::try_from(requested_result_count).map_err(|_| {
            DbError::bind_error(
                SqlState::NumericValueOutOfRange,
                "hybrid_search_top_k_hits() k + offset is out of range",
            )
        })?;
        let mut fuse_args = vec![
            literal_expr_from_value(dense_hits),
            literal_expr_from_value(sparse_hits),
            literal_expr_from_value(Value::BigInt(requested_i64)),
            literal_expr_from_value(Value::Double(dense_weight)),
            literal_expr_from_value(Value::Double(sparse_weight)),
        ];
        let fused = match fusion {
            HybridSearchFusionMethod::Rrf => {
                let rrf_k_i64 = i64::try_from(rrf_k).map_err(|_| {
                    DbError::bind_error(
                        SqlState::NumericValueOutOfRange,
                        "hybrid_search_top_k_hits() options.rrf_k is out of range",
                    )
                })?;
                fuse_args.push(literal_expr_from_value(Value::BigInt(rrf_k_i64)));
                self.resolve_hybrid_fuse_rrf_hits(&fuse_args, outer_row, context)?
            }
            HybridSearchFusionMethod::Dbsf => {
                self.resolve_hybrid_fuse_dbsf_hits(&fuse_args, outer_row, context)?
            }
        };

        if offset == 0 {
            return Ok(fused);
        }
        let fused_hits = parse_rrf_hits_arg(&fused, "hybrid_search_top_k_hits() fused hits")?;
        let final_hits = fused_hits
            .into_iter()
            .skip(offset)
            .take(k)
            .map(|hit| Value::Jsonb(serde_json::Value::Object(hit)))
            .collect();
        Ok(Value::Array(final_hits))
    }

    pub(super) fn resolve_hybrid_fuse_dbsf_hits(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if !(3..=5).contains(&arg_values.len()) {
            return Err(DbError::internal(
                "hybrid_fuse_dbsf_hits() expects between 3 and 5 arguments",
            ));
        }

        let dense_hits = parse_rrf_hits_arg(
            arg_values
                .first()
                .ok_or_else(|| DbError::internal("hybrid_fuse_dbsf_hits() missing dense hits"))?,
            "hybrid_fuse_dbsf_hits() dense hits",
        )?;
        let sparse_hits = parse_rrf_hits_arg(
            arg_values
                .get(1)
                .ok_or_else(|| DbError::internal("hybrid_fuse_dbsf_hits() missing sparse hits"))?,
            "hybrid_fuse_dbsf_hits() sparse hits",
        )?;
        let k = non_negative_usize_arg(
            arg_values
                .get(2)
                .ok_or_else(|| DbError::internal("hybrid_fuse_dbsf_hits() missing k"))?,
            "hybrid_fuse_dbsf_hits() k",
        )?;
        if k == 0 {
            return Ok(Value::Array(Vec::new()));
        }
        let dense_weight =
            parse_rrf_weight_arg(arg_values.get(3), "hybrid_fuse_dbsf_hits() dense_weight")?;
        let sparse_weight =
            parse_rrf_weight_arg(arg_values.get(4), "hybrid_fuse_dbsf_hits() sparse_weight")?;

        let dense_source_hits =
            collect_dbsf_source_hits(&dense_hits, "hybrid_fuse_dbsf_hits() dense hits", context)?;
        let sparse_source_hits =
            collect_dbsf_source_hits(&sparse_hits, "hybrid_fuse_dbsf_hits() sparse hits", context)?;
        let dense_normalized = compute_dbsf_normalized_scores(&dense_source_hits);
        let sparse_normalized = compute_dbsf_normalized_scores(&sparse_source_hits);

        let mut fused = std::collections::BTreeMap::<i64, HybridDbsfFusionEntry>::new();

        for (hit, normalized_score) in dense_source_hits.iter().zip(dense_normalized.iter()) {
            context.check_deadline()?;
            let entry = fused.entry(hit.id).or_default();
            entry.fused_score += dense_weight * *normalized_score;
            entry.dense_rank = Some(hit.rank);
            entry.dense_score = hit.score;
            entry.dense_distance = hit.distance;
            entry.dense_normalized_score = Some(*normalized_score);
            if entry.payload.is_none() {
                entry.payload = hit.payload.clone();
            }
        }

        for (hit, normalized_score) in sparse_source_hits.iter().zip(sparse_normalized.iter()) {
            context.check_deadline()?;
            let entry = fused.entry(hit.id).or_default();
            entry.fused_score += sparse_weight * *normalized_score;
            entry.sparse_rank = Some(hit.rank);
            entry.sparse_score = hit.score;
            entry.sparse_distance = hit.distance;
            entry.sparse_normalized_score = Some(*normalized_score);
            if entry.payload.is_none() {
                entry.payload = hit.payload.clone();
            }
        }

        let mut ordered: Vec<(i64, HybridDbsfFusionEntry)> = fused.into_iter().collect();
        ordered.sort_by(|left, right| {
            right
                .1
                .fused_score
                .total_cmp(&left.1.fused_score)
                .then_with(|| left.0.cmp(&right.0))
        });

        let mut hits = Vec::new();
        for (id, entry) in ordered.into_iter().take(k) {
            context.check_deadline()?;
            let mut object = serde_json::Map::new();
            object.insert("id".to_owned(), serde_json::Value::Number(id.into()));
            object.insert(
                "fused_score".to_owned(),
                vector_hit_json_number(entry.fused_score),
            );
            if let Some(rank) = entry.dense_rank {
                let mut dense = serde_json::Map::new();
                dense.insert(
                    "rank".to_owned(),
                    serde_json::Value::Number(usize_to_i64(rank).into()),
                );
                if let Some(normalized_score) = entry.dense_normalized_score {
                    dense.insert(
                        "normalized_score".to_owned(),
                        vector_hit_json_number(normalized_score),
                    );
                }
                if let Some(score) = entry.dense_score {
                    dense.insert("score".to_owned(), vector_hit_json_number(score));
                }
                if let Some(distance) = entry.dense_distance {
                    dense.insert("distance".to_owned(), vector_hit_json_number(distance));
                }
                object.insert("dense".to_owned(), serde_json::Value::Object(dense));
            }
            if let Some(rank) = entry.sparse_rank {
                let mut sparse = serde_json::Map::new();
                sparse.insert(
                    "rank".to_owned(),
                    serde_json::Value::Number(usize_to_i64(rank).into()),
                );
                if let Some(normalized_score) = entry.sparse_normalized_score {
                    sparse.insert(
                        "normalized_score".to_owned(),
                        vector_hit_json_number(normalized_score),
                    );
                }
                if let Some(score) = entry.sparse_score {
                    sparse.insert("score".to_owned(), vector_hit_json_number(score));
                }
                if let Some(distance) = entry.sparse_distance {
                    sparse.insert("distance".to_owned(), vector_hit_json_number(distance));
                }
                object.insert("sparse".to_owned(), serde_json::Value::Object(sparse));
            }
            if let Some(payload) = entry.payload {
                object.insert("payload".to_owned(), payload);
            }
            hits.push(Value::Jsonb(serde_json::Value::Object(object)));
        }
        Ok(Value::Array(hits))
    }

    pub(super) fn resolve_hybrid_group_hits_by(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if !(3..=4).contains(&arg_values.len()) {
            return Err(DbError::internal(
                "hybrid_group_hits_by() expects 3 or 4 arguments",
            ));
        }

        let hits = parse_rrf_hits_arg(
            arg_values
                .first()
                .ok_or_else(|| DbError::internal("hybrid_group_hits_by() missing hits"))?,
            "hybrid_group_hits_by() hits",
        )?;
        if hits.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }
        let payload_field = expect_text_arg(
            arg_values
                .get(1)
                .ok_or_else(|| DbError::internal("hybrid_group_hits_by() missing payload field"))?,
            "hybrid_group_hits_by() payload field",
        )?;
        let group_limit = non_negative_usize_arg(
            arg_values
                .get(2)
                .ok_or_else(|| DbError::internal("hybrid_group_hits_by() missing group limit"))?,
            "hybrid_group_hits_by() group limit",
        )?;
        if group_limit == 0 {
            return Ok(Value::Array(Vec::new()));
        }
        let group_size = arg_values
            .get(3)
            .map(|value| non_negative_usize_arg(value, "hybrid_group_hits_by() group size"))
            .transpose()?
            .unwrap_or(usize::MAX);

        #[derive(Clone, Debug)]
        struct GroupBucket {
            group: serde_json::Value,
            hits: Vec<serde_json::Map<String, serde_json::Value>>,
            count: usize,
            best_score: f64,
            first_hit_ordinal: usize,
            stable_key: String,
        }

        let mut grouped = std::collections::BTreeMap::<String, GroupBucket>::new();
        for (ordinal, hit) in hits.into_iter().enumerate() {
            context.check_deadline()?;
            let group_value = hit
                .get("payload")
                .and_then(serde_json::Value::as_object)
                .and_then(|payload| payload.get(payload_field))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let stable_key =
                serde_json::to_string(&group_value).unwrap_or_else(|_| "null".to_owned());
            let rank_score = read_hit_score_for_dbsf(&hit).unwrap_or(f64::NEG_INFINITY);
            let bucket = grouped
                .entry(stable_key.clone())
                .or_insert_with(|| GroupBucket {
                    group: group_value,
                    hits: Vec::new(),
                    count: 0,
                    best_score: rank_score,
                    first_hit_ordinal: ordinal,
                    stable_key,
                });
            bucket.count = bucket.count.saturating_add(1);
            if rank_score.is_finite() {
                bucket.best_score = bucket.best_score.max(rank_score);
            }
            if bucket.hits.len() < group_size {
                bucket.hits.push(hit);
            }
        }

        let mut ordered: Vec<GroupBucket> = grouped.into_values().collect();
        ordered.sort_by(|left, right| {
            right
                .best_score
                .total_cmp(&left.best_score)
                .then_with(|| left.first_hit_ordinal.cmp(&right.first_hit_ordinal))
                .then_with(|| left.stable_key.cmp(&right.stable_key))
        });

        let mut grouped_hits = Vec::new();
        for bucket in ordered.into_iter().take(group_limit) {
            context.check_deadline()?;
            let mut object = serde_json::Map::new();
            object.insert("group".to_owned(), bucket.group);
            object.insert(
                "count".to_owned(),
                serde_json::Value::Number(usize_to_i64(bucket.count).into()),
            );
            object.insert(
                "hits".to_owned(),
                serde_json::Value::Array(
                    bucket
                        .hits
                        .into_iter()
                        .map(serde_json::Value::Object)
                        .collect(),
                ),
            );
            grouped_hits.push(Value::Jsonb(serde_json::Value::Object(object)));
        }
        Ok(Value::Array(grouped_hits))
    }

    fn find_hnsw_index_for_column(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        vector_ordinal: usize,
        metric: VectorDistanceMetric,
    ) -> DbResult<Option<IndexId>> {
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
        else {
            return Ok(None);
        };
        let Some(vector_column) = table.columns.get(vector_ordinal) else {
            return Ok(None);
        };
        Ok(self
            .catalog_reader
            .list_indexes(context.txn_id, table_id)?
            .into_iter()
            .find(|index| {
                index.kind == IndexKind::Hnsw
                    && index.key_columns.len() == 1
                    && index.key_columns[0].column_id == vector_column.column_id
                    && index.hnsw_distance_metric() == Some(metric)
            })
            .map(|index| index.index_id))
    }

    fn find_gin_index_for_column(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        column_ordinal: usize,
    ) -> DbResult<Option<IndexId>> {
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
        else {
            return Ok(None);
        };
        let Some(target_column) = table.columns.get(column_ordinal) else {
            return Ok(None);
        };
        Ok(self
            .catalog_reader
            .list_indexes(context.txn_id, table_id)?
            .into_iter()
            .find(|index| {
                index.kind == IndexKind::Gin
                    && index.key_columns.len() == 1
                    && index.key_columns[0].column_id == target_column.column_id
            })
            .map(|index| index.index_id))
    }

    fn compile_vector_top_k_filter(
        &self,
        table: &TableDescriptor,
        filter_spec: Option<&VectorTopKFilterSpec>,
    ) -> DbResult<Option<CompiledVectorTopKFilter>> {
        let Some(filter_spec) = filter_spec else {
            return Ok(None);
        };

        let compile_clause =
            |raw_conditions: &[VectorTopKFilterCondition]| -> DbResult<Vec<CompiledVectorTopKFilterCondition>> {
                let mut compiled = Vec::with_capacity(raw_conditions.len());
                for condition in raw_conditions {
                    let Some((ordinal, column)) = table
                        .columns
                        .iter()
                        .enumerate()
                        .find(|(_, column)| column.name.eq_ignore_ascii_case(&condition.key))
                    else {
                        return Err(DbError::bind_error(
                            SqlState::UndefinedColumn,
                            format!(
                                "column \"{}\" does not exist on relation \"{}\"",
                                condition.key, table.name
                            ),
                        ));
                    };
                    let predicate = match &condition.predicate {
                        VectorTopKFilterPredicateSpec::Match(raw_match) => {
                            let raw_value = if matches!(column.data_type, DataType::Jsonb) {
                                Value::Jsonb(raw_match.clone())
                            } else {
                                vector_filter_json_literal_to_value(raw_match)
                            };
                            let expected = if matches!(raw_value, Value::Null) {
                                Value::Null
                            } else {
                                aiondb_eval::coerce_value(raw_value, &column.data_type)?
                            };
                            CompiledVectorTopKFilterPredicate::Match(expected)
                        }
                        VectorTopKFilterPredicateSpec::Range(range) => {
                            if !vector_filter_supports_numeric_range(&column.data_type) {
                                return Err(DbError::bind_error(
                                    SqlState::DatatypeMismatch,
                                    format!(
                                        "vector_top_k_ids() options.filter range on column \"{}\" requires a numeric column",
                                        condition.key
                                    ),
                                ));
                            }
                            CompiledVectorTopKFilterPredicate::Range {
                                gt: range.gt,
                                gte: range.gte,
                                lt: range.lt,
                                lte: range.lte,
                            }
                        }
                    };
                    compiled.push(CompiledVectorTopKFilterCondition {
                        ordinal,
                        column_id: column.column_id,
                        predicate,
                    });
                }
                Ok(compiled)
            };

        let filter = CompiledVectorTopKFilter {
            must: compile_clause(&filter_spec.must)?,
            should: compile_clause(&filter_spec.should)?,
            must_not: compile_clause(&filter_spec.must_not)?,
        };
        if filter.must.is_empty() && filter.should.is_empty() && filter.must_not.is_empty() {
            Ok(None)
        } else {
            Ok(Some(filter))
        }
    }

    fn collect_vector_filter_matching_tuple_ids(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        payload_filter: &CompiledVectorTopKFilter,
    ) -> DbResult<std::collections::HashSet<aiondb_core::TupleId>> {
        let btree_indexes_by_column = self
            .catalog_reader
            .list_indexes(context.txn_id, table_id)?
            .into_iter()
            .filter(|index| index.kind == IndexKind::BTree && index.key_columns.len() == 1)
            .fold(
                std::collections::HashMap::<ColumnId, IndexId>::new(),
                |mut map, index| {
                    map.entry(index.key_columns[0].column_id)
                        .or_insert(index.index_id);
                    map
                },
            );

        let is_indexable = |condition: &CompiledVectorTopKFilterCondition| {
            matches!(
                &condition.predicate,
                CompiledVectorTopKFilterPredicate::Match(expected) if !expected.is_null()
            ) && btree_indexes_by_column.contains_key(&condition.column_id)
        };

        let collect_condition_matches = |condition: &CompiledVectorTopKFilterCondition| {
            let Some(index_id) = btree_indexes_by_column.get(&condition.column_id).copied() else {
                return Ok(std::collections::HashSet::new());
            };
            let CompiledVectorTopKFilterPredicate::Match(expected) = &condition.predicate else {
                return Ok(std::collections::HashSet::new());
            };
            let mut stream = self.scan_index_locked(
                context,
                table_id,
                index_id,
                exact_lookup_key_range(expected),
                None,
            )?;
            let mut matches = std::collections::HashSet::new();
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                matches.insert(record.tuple_id);
            }
            Ok(matches)
        };

        let indexed_must = payload_filter
            .must
            .iter()
            .filter(|condition| is_indexable(condition))
            .collect::<Vec<_>>();
        let indexed_should = payload_filter
            .should
            .iter()
            .filter(|condition| is_indexable(condition))
            .collect::<Vec<_>>();
        let indexed_must_not = payload_filter
            .must_not
            .iter()
            .filter(|condition| is_indexable(condition))
            .collect::<Vec<_>>();

        let mut required_ids = if let Some(first_must) = indexed_must.first() {
            collect_condition_matches(first_must)?
        } else {
            std::collections::HashSet::new()
        };
        for condition in indexed_must.iter().skip(1) {
            let matches = collect_condition_matches(condition)?;
            required_ids.retain(|tuple_id| matches.contains(tuple_id));
            if required_ids.is_empty() {
                return Ok(required_ids);
            }
        }

        let mut should_ids = std::collections::HashSet::new();
        for condition in &indexed_should {
            let matches = collect_condition_matches(condition)?;
            should_ids.extend(matches);
        }

        let mut excluded_ids = std::collections::HashSet::new();
        for condition in &indexed_must_not {
            let matches = collect_condition_matches(condition)?;
            excluded_ids.extend(matches);
        }

        let anchored = !indexed_must.is_empty() || !indexed_should.is_empty();
        if !anchored {
            let mut stream = self.scan_table_locked(context, table_id, None)?;
            let mut tuple_ids = std::collections::HashSet::new();
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                if payload_filter.matches(&record.row) {
                    tuple_ids.insert(record.tuple_id);
                }
            }
            return Ok(tuple_ids);
        }

        let mut candidate_ids = if indexed_must.is_empty() {
            should_ids
        } else if payload_filter.should.is_empty() {
            required_ids
        } else if indexed_should.is_empty() {
            return Ok(std::collections::HashSet::new());
        } else {
            required_ids
                .into_iter()
                .filter(|tuple_id| should_ids.contains(tuple_id))
                .collect()
        };
        if candidate_ids.is_empty() {
            return Ok(candidate_ids);
        }
        if !excluded_ids.is_empty() {
            candidate_ids.retain(|tuple_id| !excluded_ids.contains(tuple_id));
        }
        if candidate_ids.is_empty() {
            return Ok(candidate_ids);
        }

        let should_validate_candidates = if let Some(stats) = self
            .catalog_reader
            .get_statistics(context.txn_id, table_id)?
        {
            let row_count = usize::try_from(stats.row_count).unwrap_or(usize::MAX);
            candidate_ids.len() <= row_count.saturating_div(2).max(1)
        } else {
            candidate_ids.len() <= VECTOR_FILTER_TUPLE_FETCH_VALIDATION_THRESHOLD
        };
        if !should_validate_candidates {
            let mut stream = self.scan_table_locked(context, table_id, None)?;
            let mut tuple_ids = std::collections::HashSet::new();
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                if payload_filter.matches(&record.row) {
                    tuple_ids.insert(record.tuple_id);
                }
            }
            return Ok(tuple_ids);
        }

        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;

        let mut result = std::collections::HashSet::with_capacity(candidate_ids.len());
        for tuple_id in candidate_ids {
            context.check_deadline()?;
            let Some(row) = self.storage_dml.fetch(
                context.txn_id,
                &context.snapshot,
                table_id,
                tuple_id,
                None,
            )?
            else {
                continue;
            };
            if payload_filter.matches(&row) {
                result.insert(tuple_id);
            }
        }
        Ok(result)
    }

    fn collect_vector_top_k_ids_hnsw(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        index_id: IndexId,
        vector_ordinal: usize,
        query_vector: &aiondb_core::VectorValue,
        metric: HybridVectorMetric,
        requested_result_count: usize,
        offset: usize,
        ef_search: usize,
        distance_threshold: Option<f64>,
        score_threshold: Option<f64>,
        payload_filter: Option<&CompiledVectorTopKFilter>,
    ) -> DbResult<Vec<Value>> {
        let needs_adaptive_widening =
            payload_filter.is_some() || distance_threshold.is_some() || score_threshold.is_some();
        let tuple_id_filter = payload_filter
            .map(|filter| self.collect_vector_filter_matching_tuple_ids(context, table_id, filter))
            .transpose()?;
        if tuple_id_filter
            .as_ref()
            .is_some_and(std::collections::HashSet::is_empty)
        {
            return Ok(Vec::new());
        }
        let final_count = requested_result_count.saturating_sub(offset);
        if !needs_adaptive_widening {
            let (ids, _) = self.collect_vector_top_k_ids_hnsw_once(
                context,
                table_id,
                index_id,
                vector_ordinal,
                query_vector,
                metric,
                requested_result_count,
                ef_search,
                distance_threshold,
                score_threshold,
                payload_filter,
                tuple_id_filter.as_ref(),
            )?;
            return Ok(ids.into_iter().skip(offset).take(final_count).collect());
        }

        let scan_limit_cap = pgvector_hnsw_max_scan_tuples_setting(context)?
            .unwrap_or(VECTOR_MAX_K)
            .clamp(1, VECTOR_MAX_K);
        let mut scan_limit = requested_result_count.max(1).min(scan_limit_cap);
        let mut scan_ef_search = ef_search
            .max(bounded_hnsw_ef_search(scan_limit))
            .min(HNSW_MAX_EF_SEARCH);
        loop {
            let (ids, fetched_rows) = self.collect_vector_top_k_ids_hnsw_once(
                context,
                table_id,
                index_id,
                vector_ordinal,
                query_vector,
                metric,
                scan_limit,
                scan_ef_search,
                distance_threshold,
                score_threshold,
                payload_filter,
                tuple_id_filter.as_ref(),
            )?;
            if ids.len() >= requested_result_count
                || scan_limit >= scan_limit_cap
                || fetched_rows < scan_limit
            {
                return Ok(ids.into_iter().skip(offset).take(final_count).collect());
            }
            let next_limit =
                next_vector_top_k_hnsw_limit(scan_limit, ids.len(), requested_result_count)
                    .min(scan_limit_cap);
            if next_limit <= scan_limit {
                return Ok(ids.into_iter().skip(offset).take(final_count).collect());
            }
            scan_limit = next_limit;
            scan_ef_search = scan_ef_search
                .max(bounded_hnsw_ef_search(scan_limit))
                .min(HNSW_MAX_EF_SEARCH);
        }
    }

    fn collect_vector_top_k_ids_hnsw_once(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        index_id: IndexId,
        vector_ordinal: usize,
        query_vector: &aiondb_core::VectorValue,
        metric: HybridVectorMetric,
        search_limit: usize,
        ef_search: usize,
        distance_threshold: Option<f64>,
        score_threshold: Option<f64>,
        payload_filter: Option<&CompiledVectorTopKFilter>,
        tuple_id_filter: Option<&std::collections::HashSet<aiondb_core::TupleId>>,
    ) -> DbResult<(Vec<Value>, usize)> {
        let max_search_duration = context
            .statement_deadline
            .and_then(|deadline| deadline.checked_duration_since(std::time::Instant::now()));
        let mut stream = self.vector_search_locked(
            context,
            table_id,
            index_id,
            &query_vector.values,
            search_limit,
            ef_search,
            tuple_id_filter,
            max_search_duration,
        )?;
        let mut ids = Vec::new();
        let mut seen_ids = std::collections::HashSet::<i64>::new();
        let mut fetched_rows = 0usize;
        while let Some(record) = stream.next()? {
            fetched_rows = fetched_rows.saturating_add(1);
            context.check_deadline()?;
            if tuple_id_filter.is_none()
                && payload_filter
                    .as_ref()
                    .is_some_and(|filter| !filter.matches(&record.row))
            {
                continue;
            }
            if distance_threshold.is_some() || score_threshold.is_some() {
                let Some(Value::Vector(candidate_vector)) = record.row.values.get(vector_ordinal)
                else {
                    continue;
                };
                let distance = compute_vector_distance(metric, candidate_vector, query_vector)?;
                if !vector_candidate_passes_thresholds(
                    metric,
                    distance,
                    distance_threshold,
                    score_threshold,
                ) {
                    continue;
                }
            }
            let id = aiondb_eval::coerce_value(
                record.row.values.first().cloned().unwrap_or(Value::Null),
                &DataType::BigInt,
            )?;
            let Value::BigInt(id) = id else {
                continue;
            };
            if seen_ids.insert(id) {
                ids.push(Value::BigInt(id));
            }
        }
        Ok((ids, fetched_rows))
    }

    fn collect_vector_top_k_ids_exact(
        &self,
        context: &ExecutionContext,
        table: &TableDescriptor,
        vector_ordinal: usize,
        query_vector: &aiondb_core::VectorValue,
        metric: HybridVectorMetric,
        requested_result_count: usize,
        offset: usize,
        distance_threshold: Option<f64>,
        score_threshold: Option<f64>,
        payload_filter: Option<&CompiledVectorTopKFilter>,
    ) -> DbResult<Vec<Value>> {
        let (projected_columns, candidate_vector_ordinal) = if payload_filter.is_some() {
            (None, vector_ordinal)
        } else {
            (
                self.table_column_ids_for_ordinals(context, table.table_id, &[0, vector_ordinal])?,
                1,
            )
        };
        let mut scored = Vec::new();
        let mut used_tuple_fetch = false;
        if let Some(payload_filter) = payload_filter {
            let tuple_id_filter = self.collect_vector_filter_matching_tuple_ids(
                context,
                table.table_id,
                payload_filter,
            )?;
            if tuple_id_filter.is_empty() {
                return Ok(Vec::new());
            }
            if tuple_id_filter.len() <= VECTOR_TOP_K_EXACT_TUPLE_FETCH_THRESHOLD {
                let projected_columns = self.table_column_ids_for_ordinals(
                    context,
                    table.table_id,
                    &[0, vector_ordinal],
                )?;
                let tuple_id_list = tuple_id_filter.into_iter().collect::<Vec<_>>();
                let rows = self.load_rows_by_tuple_ids(
                    context,
                    table.table_id,
                    &tuple_id_list,
                    projected_columns,
                )?;
                // Each row's distance computation is independent (SIMD on
                // disjoint vectors). Score them in parallel; ordering does
                // not matter here because the caller sorts `scored` by
                // distance afterwards.
                let scored_opts: Vec<Option<(f64, i64)>> = rows
                    .par_iter()
                    .with_min_len(32)
                    .map(|row| -> DbResult<Option<(f64, i64)>> {
                        context.check_deadline()?;
                        let id = aiondb_eval::coerce_value(
                            row.values.first().cloned().unwrap_or(Value::Null),
                            &DataType::BigInt,
                        )?;
                        let Value::BigInt(id_value) = id else {
                            return Ok(None);
                        };
                        let Some(Value::Vector(candidate_vector)) = row.values.get(1) else {
                            return Ok(None);
                        };
                        let distance =
                            compute_vector_distance(metric, candidate_vector, query_vector)?;
                        if !vector_candidate_passes_thresholds(
                            metric,
                            distance,
                            distance_threshold,
                            score_threshold,
                        ) {
                            return Ok(None);
                        }
                        let sortable_distance = if distance.is_nan() {
                            f64::INFINITY
                        } else {
                            distance
                        };
                        Ok(Some((sortable_distance, id_value)))
                    })
                    .collect::<DbResult<Vec<_>>>()?;
                scored.extend(scored_opts.into_iter().flatten());
                used_tuple_fetch = true;
            }
        }
        if !used_tuple_fetch {
            let mut stream = self.scan_table_locked(context, table.table_id, projected_columns)?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                if payload_filter
                    .as_ref()
                    .is_some_and(|filter| !filter.matches(&record.row))
                {
                    continue;
                }
                let id = aiondb_eval::coerce_value(
                    record.row.values.first().cloned().unwrap_or(Value::Null),
                    &DataType::BigInt,
                )?;
                let Value::BigInt(id_value) = id else {
                    continue;
                };
                let Some(Value::Vector(candidate_vector)) =
                    record.row.values.get(candidate_vector_ordinal)
                else {
                    continue;
                };
                let distance = compute_vector_distance(metric, candidate_vector, query_vector)?;
                if !vector_candidate_passes_thresholds(
                    metric,
                    distance,
                    distance_threshold,
                    score_threshold,
                ) {
                    continue;
                }
                let sortable_distance = if distance.is_nan() {
                    f64::INFINITY
                } else {
                    distance
                };
                scored.push((sortable_distance, id_value));
            }
        }
        let failed = std::cell::Cell::new(false);
        let error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
        scored.sort_by(|left, right| {
            if failed.get() {
                return Ordering::Equal;
            }
            if let Err(e) = context.check_deadline() {
                failed.set(true);
                *error.borrow_mut() = Some(e);
                return Ordering::Equal;
            }
            left.0
                .total_cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
        });
        if let Some(e) = error.into_inner() {
            return Err(e);
        }
        scored.truncate(requested_result_count);
        let final_count = requested_result_count.saturating_sub(offset);
        Ok(scored
            .into_iter()
            .skip(offset)
            .take(final_count)
            .map(|(_, id)| Value::BigInt(id))
            .collect())
    }

    pub(crate) fn find_sequence_descriptor(
        &self,
        sequence_name: &str,
        context: &ExecutionContext,
    ) -> DbResult<SequenceDescriptor> {
        let candidate = parse_qualified_name(sequence_name);
        if let Some(sequence) = self
            .catalog_reader
            .get_sequence(context.txn_id, &candidate)?
        {
            return Ok(sequence);
        }

        if candidate.schema_name().is_none() {
            for schema_name in super::session_search_path_schemas(context) {
                let qualified = QualifiedName::qualified(&schema_name, candidate.object_name());
                if let Some(sequence) = self
                    .catalog_reader
                    .get_sequence(context.txn_id, &qualified)?
                {
                    return Ok(sequence);
                }
            }
        }

        Err(DbError::bind_error(
            SqlState::UndefinedObject,
            format!("sequence \"{sequence_name}\" does not exist"),
        ))
    }

    pub(super) fn find_view_by_oid(
        &self,
        oid: i32,
        context: &ExecutionContext,
    ) -> DbResult<Option<ViewDescriptor>> {
        let relation_raw = i64::from(oid) - 16384;
        if relation_raw <= 0 {
            return Ok(None);
        }

        let Some(relation_id) = u64::try_from(relation_raw).ok().map(RelationId::new) else {
            return Ok(None);
        };
        self.find_view_in_known_schemas(context, |view| view.view_id == relation_id)
    }

    pub(super) fn find_view_by_name(
        &self,
        name: &str,
        context: &ExecutionContext,
    ) -> DbResult<Option<ViewDescriptor>> {
        let candidate = QualifiedName::parse(name);
        if let Some(view) = self.catalog_reader.get_view(context.txn_id, &candidate)? {
            return Ok(Some(view));
        }

        if candidate.schema_name().is_none() {
            for schema_name in super::session_search_path_schemas(context) {
                let qualified = QualifiedName::qualified(&schema_name, candidate.object_name());
                if let Some(view) = self.catalog_reader.get_view(context.txn_id, &qualified)? {
                    return Ok(Some(view));
                }
            }
        }

        self.find_view_in_known_schemas(context, |view| {
            view.name
                .object_name()
                .eq_ignore_ascii_case(candidate.object_name())
        })
    }

    fn find_view_in_known_schemas<F>(
        &self,
        context: &ExecutionContext,
        predicate: F,
    ) -> DbResult<Option<ViewDescriptor>>
    where
        F: Fn(&ViewDescriptor) -> bool,
    {
        for schema in self.catalog_reader.list_schemas(context.txn_id)? {
            if let Some(view) = self
                .catalog_reader
                .list_views(context.txn_id, schema.schema_id)?
                .into_iter()
                .find(&predicate)
            {
                return Ok(Some(view));
            }
        }

        Ok(None)
    }

    pub(super) fn find_index_by_oid(
        &self,
        oid: i32,
        context: &ExecutionContext,
    ) -> DbResult<Option<IndexDescriptor>> {
        let index_raw = i64::from(oid) - 32768;
        if index_raw <= 0 {
            return Ok(None);
        }
        let Some(index_id) = u64::try_from(index_raw).ok().map(IndexId::new) else {
            return Ok(None);
        };
        self.catalog_reader.get_index(context.txn_id, index_id)
    }

    pub(super) fn find_index_by_name(
        &self,
        name: &str,
        context: &ExecutionContext,
    ) -> DbResult<Option<IndexDescriptor>> {
        let candidate = parse_text_qualified_name(name);
        for schema_name in index_lookup_schemas(&candidate, context) {
            let schema_name = QualifiedName::unqualified(schema_name);
            let Some(schema) = self
                .catalog_reader
                .get_schema(context.txn_id, &schema_name)?
            else {
                continue;
            };
            for table in self
                .catalog_reader
                .list_tables(context.txn_id, schema.schema_id)?
            {
                if let Some(index) = self
                    .catalog_reader
                    .list_indexes(context.txn_id, table.table_id)?
                    .into_iter()
                    .find(|index| {
                        index
                            .name
                            .object_name()
                            .eq_ignore_ascii_case(candidate.object_name())
                    })
                {
                    return Ok(Some(index));
                }
            }
        }
        Ok(None)
    }

    pub(super) fn find_table_by_oid(
        &self,
        oid: i32,
        context: &ExecutionContext,
    ) -> DbResult<Option<TableDescriptor>> {
        let relation_raw = i64::from(oid) - 16_384;
        if relation_raw <= 0 {
            return Ok(None);
        }
        let Some(relation_id) = u64::try_from(relation_raw).ok().map(RelationId::new) else {
            return Ok(None);
        };
        self.catalog_reader
            .get_table_by_id(context.txn_id, relation_id)
    }

    pub(super) fn find_relation_by_oid(
        &self,
        oid: i32,
        context: &ExecutionContext,
    ) -> DbResult<Option<ResolvedRelation>> {
        if let Some(table_name) = builtin_relation_name_for_oid(oid) {
            return Ok(Some(ResolvedRelation::Synthetic {
                oid,
                display_name: format!("pg_catalog.{table_name}"),
            }));
        }
        if let Some(table) = self.find_table_by_oid(oid, context)? {
            return Ok(Some(ResolvedRelation::Table(table)));
        }
        if let Some(view) = self.find_view_by_oid(oid, context)? {
            return Ok(Some(ResolvedRelation::View(view)));
        }
        if let Some(index) = self.find_index_by_oid(oid, context)? {
            return Ok(Some(ResolvedRelation::Index(index)));
        }
        Ok(None)
    }

    pub(super) fn find_relation_by_name(
        &self,
        name: &str,
        context: &ExecutionContext,
    ) -> DbResult<Option<ResolvedRelation>> {
        if let Some(oid) = resolve_builtin_relation_oid(name) {
            let display_name = builtin_relation_name_for_oid(oid).map_or_else(
                || name.to_owned(),
                |table_name| format!("pg_catalog.{table_name}"),
            );
            return Ok(Some(ResolvedRelation::Synthetic { oid, display_name }));
        }

        let parts = parse_identifier_components(name, '.');
        let candidate = match parts.as_slice() {
            [_, schema, object] => QualifiedName::qualified(schema.clone(), object.clone()),
            [.., schema, object] => QualifiedName::qualified(schema.clone(), object.clone()),
            _ => parse_text_qualified_name(name),
        };
        for schema_name in index_lookup_schemas(&candidate, context) {
            let qualified =
                QualifiedName::qualified(schema_name.clone(), candidate.object_name().to_owned());
            if let Some(table) = self.catalog_reader.get_table(context.txn_id, &qualified)? {
                return Ok(Some(ResolvedRelation::Table(table)));
            }
            if let Some(view) = self.catalog_reader.get_view(context.txn_id, &qualified)? {
                return Ok(Some(ResolvedRelation::View(view)));
            }

            let schema_name = QualifiedName::unqualified(schema_name);
            let Some(schema) = self
                .catalog_reader
                .get_schema(context.txn_id, &schema_name)?
            else {
                continue;
            };
            for table in self
                .catalog_reader
                .list_tables(context.txn_id, schema.schema_id)?
            {
                if let Some(index) = self
                    .catalog_reader
                    .list_indexes(context.txn_id, table.table_id)?
                    .into_iter()
                    .find(|index| {
                        index
                            .name
                            .object_name()
                            .eq_ignore_ascii_case(candidate.object_name())
                    })
                {
                    return Ok(Some(ResolvedRelation::Index(index)));
                }
            }
        }

        Ok(None)
    }

    pub(super) fn estimate_relation_size(
        &self,
        relation: &ResolvedRelation,
        context: &ExecutionContext,
    ) -> DbResult<i64> {
        match relation {
            ResolvedRelation::Synthetic { .. } | ResolvedRelation::View(_) => Ok(0),
            ResolvedRelation::Table(table) => self.estimate_table_size(table, context),
            ResolvedRelation::Index(index) => self.estimate_index_size(index, context),
        }
    }

    pub(super) fn estimate_table_size(
        &self,
        table: &TableDescriptor,
        context: &ExecutionContext,
    ) -> DbResult<i64> {
        if let Some(stats) = self
            .catalog_reader
            .get_statistics(context.txn_id, table.table_id)?
            .filter(|stats| stats.total_bytes > 0)
        {
            return Ok(i64::try_from(stats.total_bytes).unwrap_or(i64::MAX));
        }

        let mut stream = self.scan_table_locked(context, table.table_id, None)?;
        let mut total = 8_192_i64;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            total = total
                .saturating_add(24)
                .saturating_add(u64_to_i64(estimate_row_bytes(&record.row)));
        }
        Ok(total)
    }

    pub(super) fn estimate_table_indexes_size(
        &self,
        table: &TableDescriptor,
        context: &ExecutionContext,
    ) -> DbResult<i64> {
        self.catalog_reader
            .list_indexes(context.txn_id, table.table_id)?
            .into_iter()
            .try_fold(0_i64, |acc, index| {
                Ok(acc.saturating_add(self.estimate_index_size(&index, context)?))
            })
    }

    pub(super) fn estimate_index_size(
        &self,
        index: &IndexDescriptor,
        context: &ExecutionContext,
    ) -> DbResult<i64> {
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, index.table_id)?
        else {
            return Ok(0);
        };

        let key_ordinals = index
            .key_columns
            .iter()
            .filter_map(|key| {
                table
                    .columns
                    .iter()
                    .position(|column| column.column_id == key.column_id)
            })
            .collect::<Vec<_>>();

        let mut stream = self.scan_table_locked(context, table.table_id, None)?;
        let mut row_count = 0_i64;
        let mut payload_bytes = 0_i64;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            row_count = row_count.saturating_add(1);
            if key_ordinals.is_empty() {
                payload_bytes = payload_bytes.saturating_add(4);
                continue;
            }
            let key_bytes = key_ordinals.iter().fold(0_i64, |acc, ordinal| {
                let value = record.row.values.get(*ordinal).unwrap_or(&Value::Null);
                acc.saturating_add(u64_to_i64(estimate_value_bytes(value)))
            });
            payload_bytes = payload_bytes
                .saturating_add(16)
                .saturating_add(key_bytes.max(4));
        }

        let base_bytes: i64 = if key_ordinals.is_empty() {
            16 * 1024
        } else {
            32 * 1024
        };

        Ok(base_bytes
            .saturating_add(payload_bytes)
            .saturating_add(row_count.saturating_mul(2)))
    }
}
