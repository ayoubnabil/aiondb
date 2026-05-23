//! Vector / hybrid-search / full-text / recommend resolution + catalog
//! lookups (`impl Executor`).
//!
//! Split out of `special_exprs/hybrid_and_relation.rs`; continuation of
//! `impl Executor`. Helper types/fns stay in the parent module and are
//! visible here as a descendant; parent scope via `use super::*`.
#![allow(clippy::too_many_lines, clippy::type_complexity)]

use super::*;

fn keep_top_k_ordered_by<T, F>(values: &mut Vec<T>, k: usize, mut compare: F)
where
    F: FnMut(&T, &T) -> std::cmp::Ordering,
{
    if k == 0 {
        values.clear();
        return;
    }
    if values.len() > k {
        values.select_nth_unstable_by(k - 1, |left, right| compare(left, right));
        values.truncate(k);
    }
    values.sort_by(compare);
}

#[derive(Clone, Copy, Debug)]
struct VectorTopKScore {
    distance: f64,
    id: i64,
}

impl PartialEq for VectorTopKScore {
    fn eq(&self, other: &Self) -> bool {
        self.distance.total_cmp(&other.distance) == Ordering::Equal && self.id == other.id
    }
}

impl Eq for VectorTopKScore {}

impl PartialOrd for VectorTopKScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for VectorTopKScore {
    fn cmp(&self, other: &Self) -> Ordering {
        self.distance
            .total_cmp(&other.distance)
            .then_with(|| self.id.cmp(&other.id))
    }
}

fn vector_top_k_score_is_better(distance: f64, id: i64, other: &VectorTopKScore) -> bool {
    match distance.total_cmp(&other.distance) {
        Ordering::Less => true,
        Ordering::Greater => false,
        Ordering::Equal => id < other.id,
    }
}

fn push_vector_top_k_score(
    top_scores: &mut std::collections::BinaryHeap<VectorTopKScore>,
    limit: usize,
    distance: f64,
    id: i64,
) {
    if limit == 0 {
        return;
    }
    let score = VectorTopKScore { distance, id };
    if top_scores.len() < limit {
        top_scores.push(score);
    } else if let Some(mut worst) = top_scores.peek_mut() {
        if vector_top_k_score_is_better(score.distance, score.id, &worst) {
            *worst = score;
        }
    }
}

fn vector_hit_payload_columns(
    table: &TableDescriptor,
    vector_ordinal: usize,
    selection: Option<&VectorTopKPayloadSelection>,
    function_name: &str,
) -> DbResult<Option<Vec<(usize, String)>>> {
    match selection.unwrap_or(&VectorTopKPayloadSelection::All) {
        VectorTopKPayloadSelection::None => Ok(None),
        VectorTopKPayloadSelection::All => Ok(Some(
            table
                .columns
                .iter()
                .enumerate()
                .filter(|(ord, _)| *ord != 0 && *ord != vector_ordinal)
                .map(|(ord, col)| (ord, col.name.clone()))
                .collect(),
        )),
        VectorTopKPayloadSelection::Include(fields) => {
            let mut columns = Vec::with_capacity(fields.len());
            let mut seen = std::collections::HashSet::with_capacity(fields.len());
            for field in fields {
                let Some((ordinal, column)) = table
                    .columns
                    .iter()
                    .enumerate()
                    .find(|(_, column)| column.name.eq_ignore_ascii_case(field))
                else {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedColumn,
                        format!("{function_name} options.with_payload column \"{field}\" does not exist"),
                    ));
                };
                if ordinal == 0 || ordinal == vector_ordinal || !seen.insert(ordinal) {
                    continue;
                }
                columns.push((ordinal, column.name.clone()));
            }
            Ok(Some(columns))
        }
        VectorTopKPayloadSelection::Exclude(fields) => {
            let mut excluded = std::collections::HashSet::with_capacity(fields.len());
            for field in fields {
                let Some((ordinal, _)) = table
                    .columns
                    .iter()
                    .enumerate()
                    .find(|(_, column)| column.name.eq_ignore_ascii_case(field))
                else {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedColumn,
                        format!("{function_name} options.with_payload column \"{field}\" does not exist"),
                    ));
                };
                excluded.insert(ordinal);
            }
            Ok(Some(
                table
                    .columns
                    .iter()
                    .enumerate()
                    .filter(|(ord, _)| {
                        *ord != 0 && *ord != vector_ordinal && !excluded.contains(ord)
                    })
                    .map(|(ord, col)| (ord, col.name.clone()))
                    .collect(),
            ))
        }
    }
}

impl Executor {
    pub(in crate::executor) fn resolve_vector_top_k_ids(
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
        let optional_arg = |index: usize| arg_values.get(index).filter(|value| !value.is_null());
        let metric = parse_vector_metric_arg(optional_arg(4))?;
        let ef_search_override = parse_vector_ef_search_arg(optional_arg(5))?;
        let distance_threshold = parse_vector_distance_threshold_arg(optional_arg(6))?;
        let exact = parse_vector_exact_arg(optional_arg(7))?;
        let score_threshold = parse_vector_score_threshold_arg(optional_arg(8))?;
        let option_overrides = parse_vector_top_k_options_arg(optional_arg(9))?;
        let k = option_overrides.limit.unwrap_or(k);
        if k == 0 {
            return Ok(Value::Array(Vec::new()));
        }
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

    pub(in crate::executor) fn resolve_vector_top_k_hits(
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
        let optional_arg = |index: usize| arg_values.get(index).filter(|value| !value.is_null());
        let metric = parse_vector_metric_arg(optional_arg(4))?;
        let ef_search_override = parse_vector_ef_search_arg(optional_arg(5))?;
        let distance_threshold = parse_vector_distance_threshold_arg(optional_arg(6))?;
        let exact = parse_vector_exact_arg(optional_arg(7))?;
        let score_threshold = parse_vector_score_threshold_arg(optional_arg(8))?;
        let option_overrides = parse_vector_top_k_options_arg(optional_arg(9))?;
        let k = option_overrides.limit.unwrap_or(k);
        if k == 0 {
            return Ok(Value::Array(Vec::new()));
        }
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
        let payload_columns = vector_hit_payload_columns(
            &table,
            vector_ordinal,
            option_overrides.with_payload.as_ref(),
            "vector_top_k_hits()",
        )?;
        let include_vector = option_overrides.with_vector.unwrap_or(false);

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
                let mut hit = serde_json::Map::with_capacity(4);
                hit.insert("id".to_owned(), serde_json::Value::Number((*id).into()));
                hit.insert("distance".to_owned(), vector_hit_json_number(distance));
                hit.insert("score".to_owned(), vector_hit_json_number(score));
                if include_vector {
                    hit.insert(
                        "vector".to_owned(),
                        vector_hit_vector_to_json(candidate_vector),
                    );
                }
                if let Some(payload_columns) = &payload_columns {
                    let mut payload =
                        serde_json::Map::with_capacity(payload_columns.len().min(1024));
                    for (ordinal, name) in payload_columns {
                        let Some(value) = row.values.get(*ordinal) else {
                            continue;
                        };
                        if value.is_null() {
                            continue;
                        }
                        payload.insert(name.clone(), vector_hit_value_to_json(value));
                    }
                    hit.insert("payload".to_owned(), serde_json::Value::Object(payload));
                }
                Ok(Some(Value::Jsonb(serde_json::Value::Object(hit))))
            })
            .collect::<DbResult<Vec<_>>>()?;
        let hits: Vec<Value> = hit_opts.into_iter().flatten().collect();
        Ok(Value::Array(hits))
    }

    pub(in crate::executor) fn resolve_vector_prefetch_top_k_hits(
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
        let k = option_overrides.limit.unwrap_or(k);
        if k == 0 {
            return Ok(Value::Array(Vec::new()));
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
        let payload_columns = vector_hit_payload_columns(
            &table,
            vector_ordinal,
            option_overrides.with_payload.as_ref(),
            "vector_prefetch_top_k_hits()",
        )?;
        let include_vector = option_overrides.with_vector.unwrap_or(false);
        let scored_opts: Vec<
            Option<(
                f64,
                i64,
                f64,
                f64,
                Option<serde_json::Map<String, serde_json::Value>>,
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
                        Option<serde_json::Map<String, serde_json::Value>>,
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
                    let payload = if let Some(payload_columns) = &payload_columns {
                        let mut payload =
                            serde_json::Map::with_capacity(payload_columns.len().min(1024));
                        for (ordinal, name) in payload_columns {
                            let Some(value) = row.values.get(*ordinal) else {
                                continue;
                            };
                            if value.is_null() {
                                continue;
                            }
                            payload.insert(name.clone(), vector_hit_value_to_json(value));
                        }
                        Some(payload)
                    } else {
                        None
                    };
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
            Option<serde_json::Map<String, serde_json::Value>>,
        )> = scored_opts.into_iter().flatten().collect();

        keep_top_k_ordered_by(&mut scored, requested_result_count, |left, right| {
            left.0
                .total_cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
        });

        let final_count = requested_result_count.saturating_sub(offset);
        let mut hits = Vec::with_capacity(final_count);
        for (_, id, distance, score, payload) in scored.into_iter().skip(offset).take(final_count) {
            context.check_deadline()?;
            let mut hit = serde_json::Map::new();
            hit.insert("id".to_owned(), serde_json::Value::Number(id.into()));
            hit.insert("distance".to_owned(), vector_hit_json_number(distance));
            hit.insert("score".to_owned(), vector_hit_json_number(score));
            if include_vector {
                let Some(row) = rows_by_id.get(&id) else {
                    continue;
                };
                let Some(Value::Vector(vector)) = row.values.get(vector_ordinal) else {
                    continue;
                };
                hit.insert("vector".to_owned(), vector_hit_vector_to_json(vector));
            }
            if let Some(payload) = payload {
                hit.insert("payload".to_owned(), serde_json::Value::Object(payload));
            }
            hits.push(Value::Jsonb(serde_json::Value::Object(hit)));
        }
        Ok(Value::Array(hits))
    }

    pub(in crate::executor) fn resolve_hybrid_fuse_rrf_hits(
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

        let fused_capacity = dense_hits.len().saturating_add(sparse_hits.len());
        let mut fused = std::collections::HashMap::<i64, HybridRrfFusionEntry<'_>>::with_capacity(
            fused_capacity,
        );
        let mut seen_dense = std::collections::HashSet::with_capacity(dense_hits.len());
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
                entry.payload = hit.get("payload");
            }
        }

        let mut seen_sparse = std::collections::HashSet::with_capacity(sparse_hits.len());
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
                entry.payload = hit.get("payload");
            }
        }

        let mut ordered: Vec<(i64, HybridRrfFusionEntry<'_>)> = fused.into_iter().collect();
        keep_top_k_ordered_by(&mut ordered, k, |left, right| {
            right
                .1
                .fused_score
                .total_cmp(&left.1.fused_score)
                .then_with(|| left.0.cmp(&right.0))
        });

        let mut hits = Vec::with_capacity(ordered.len());
        for (id, entry) in ordered {
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
                object.insert("payload".to_owned(), payload.to_owned());
            }
            hits.push(Value::Jsonb(serde_json::Value::Object(object)));
        }
        Ok(Value::Array(hits))
    }

    pub(in crate::executor) fn resolve_vector_recommend_top_k_hits(
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
        let optional_arg = |index: usize| arg_values.get(index).filter(|value| !value.is_null());
        let metric = parse_vector_metric_arg(optional_arg(5))?;
        let ef_search_override = parse_vector_ef_search_arg(optional_arg(6))?;
        let distance_threshold = parse_vector_distance_threshold_arg(optional_arg(7))?;
        let exact = parse_vector_exact_arg(optional_arg(8))?;
        let score_threshold = parse_vector_score_threshold_arg(optional_arg(9))?;
        let option_overrides = parse_vector_top_k_options_arg(optional_arg(10))?;
        let k = option_overrides.limit.unwrap_or(k);
        if k == 0 {
            return Ok(Value::Array(Vec::new()));
        }
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
            positive_specs,
            &id_vectors,
            vector_dims,
            "vector_recommend_top_k_hits() positive examples",
        )?;
        let negative_vectors = materialize_recommend_vectors(
            negative_specs,
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

        // Move the centroid out instead of cloning; we then mutate it in place
        // before wrapping it back into a `VectorValue` for the query vector.
        let positive_dims = positive_centroid.dims;
        let mut query_values = positive_centroid.values;
        if let Some(negative) = negative_centroid.as_ref() {
            for (index, value) in query_values.iter_mut().enumerate() {
                *value -= negative.values.get(index).copied().unwrap_or(0.0);
            }
        }
        let query_vector = aiondb_core::VectorValue::new(positive_dims, query_values);

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
        let payload_columns = vector_hit_payload_columns(
            &table,
            vector_ordinal,
            option_overrides.with_payload.as_ref(),
            "vector_recommend_top_k_hits()",
        )?;
        let include_vector = option_overrides.with_vector.unwrap_or(false);

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
                let mut hit = serde_json::Map::with_capacity(4);
                hit.insert("id".to_owned(), serde_json::Value::Number((*id).into()));
                hit.insert("distance".to_owned(), vector_hit_json_number(distance));
                hit.insert("score".to_owned(), vector_hit_json_number(score));
                if include_vector {
                    hit.insert(
                        "vector".to_owned(),
                        vector_hit_vector_to_json(candidate_vector),
                    );
                }
                if let Some(payload_columns) = &payload_columns {
                    let mut payload =
                        serde_json::Map::with_capacity(payload_columns.len().min(1024));
                    for (ordinal, name) in payload_columns {
                        let Some(value) = row.values.get(*ordinal) else {
                            continue;
                        };
                        if value.is_null() {
                            continue;
                        }
                        payload.insert(name.clone(), vector_hit_value_to_json(value));
                    }
                    hit.insert("payload".to_owned(), serde_json::Value::Object(payload));
                }
                Ok(Some(Value::Jsonb(serde_json::Value::Object(hit))))
            })
            .collect::<DbResult<Vec<_>>>()?;
        let hits: Vec<Value> = hit_opts.into_iter().flatten().collect();
        Ok(Value::Array(hits))
    }

    pub(in crate::executor) fn resolve_full_text_top_k_hits(
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
                while let Some(mut record) = stream.next()? {
                    context.check_deadline()?;
                    // Move column 0 out of the row (replacing it with Null)
                    // instead of cloning. text_ordinal is always > 0 in this
                    // path, so leaving Null at index 0 is harmless.
                    let first_val = if record.row.values.is_empty() {
                        Value::Null
                    } else {
                        std::mem::replace(&mut record.row.values[0], Value::Null)
                    };
                    let Some(id) = read_bigint_value(first_val)? else {
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

    pub(in crate::executor) fn resolve_hybrid_search_top_k_hits(
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
            .map(|hit| Value::Jsonb(serde_json::Value::Object(hit.clone())))
            .collect();
        Ok(Value::Array(final_hits))
    }

    pub(in crate::executor) fn resolve_hybrid_fuse_dbsf_hits(
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
        let dense_normalizer = compute_dbsf_score_normalizer(&dense_source_hits);
        let sparse_normalizer = compute_dbsf_score_normalizer(&sparse_source_hits);

        let fused_capacity = dense_source_hits
            .len()
            .saturating_add(sparse_source_hits.len());
        let mut fused = std::collections::HashMap::<i64, HybridDbsfFusionEntry<'_>>::with_capacity(
            fused_capacity,
        );

        for hit in &dense_source_hits {
            context.check_deadline()?;
            let normalized_score = dense_normalizer.normalize(hit.raw_score);
            let entry = fused.entry(hit.id).or_default();
            entry.fused_score += dense_weight * normalized_score;
            entry.dense_rank = Some(hit.rank);
            entry.dense_score = hit.score;
            entry.dense_distance = hit.distance;
            entry.dense_normalized_score = Some(normalized_score);
            if entry.payload.is_none() {
                entry.payload = hit.payload;
            }
        }

        for hit in &sparse_source_hits {
            context.check_deadline()?;
            let normalized_score = sparse_normalizer.normalize(hit.raw_score);
            let entry = fused.entry(hit.id).or_default();
            entry.fused_score += sparse_weight * normalized_score;
            entry.sparse_rank = Some(hit.rank);
            entry.sparse_score = hit.score;
            entry.sparse_distance = hit.distance;
            entry.sparse_normalized_score = Some(normalized_score);
            if entry.payload.is_none() {
                entry.payload = hit.payload;
            }
        }

        let mut ordered: Vec<(i64, HybridDbsfFusionEntry<'_>)> = fused.into_iter().collect();
        keep_top_k_ordered_by(&mut ordered, k, |left, right| {
            right
                .1
                .fused_score
                .total_cmp(&left.1.fused_score)
                .then_with(|| left.0.cmp(&right.0))
        });

        let mut hits = Vec::with_capacity(ordered.len());
        for (id, entry) in ordered {
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
                object.insert("payload".to_owned(), payload.to_owned());
            }
            hits.push(Value::Jsonb(serde_json::Value::Object(object)));
        }
        Ok(Value::Array(hits))
    }

    pub(in crate::executor) fn resolve_hybrid_group_hits_by(
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
        struct GroupBucket<'a> {
            group: serde_json::Value,
            hits: Vec<&'a serde_json::Map<String, serde_json::Value>>,
            count: usize,
            best_score: f64,
            first_hit_ordinal: usize,
            stable_key: String,
        }

        let mut grouped =
            std::collections::HashMap::<String, GroupBucket>::with_capacity(hits.len());
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
            let rank_score = read_hit_score_for_dbsf(hit).unwrap_or(f64::NEG_INFINITY);
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
        keep_top_k_ordered_by(&mut ordered, group_limit, |left, right| {
            right
                .best_score
                .total_cmp(&left.best_score)
                .then_with(|| left.first_hit_ordinal.cmp(&right.first_hit_ordinal))
                .then_with(|| left.stable_key.cmp(&right.stable_key))
        });

        let mut grouped_hits = Vec::with_capacity(ordered.len());
        for bucket in ordered {
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
                        .map(|hit| serde_json::Value::Object(hit.clone()))
                        .collect(),
                ),
            );
            grouped_hits.push(Value::Jsonb(serde_json::Value::Object(object)));
        }
        Ok(Value::Array(grouped_hits))
    }

    pub(in crate::executor) fn find_hnsw_index_for_column(
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

    pub(in crate::executor) fn find_gin_index_for_column(
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

    pub(in crate::executor) fn compile_vector_top_k_filter(
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
                    let (ordinal, column, json_path) = if matches!(
                        condition.predicate,
                        VectorTopKFilterPredicateSpec::HasId(_)
                    ) {
                        table.columns.first().map_or_else(
                            || {
                                Err(DbError::bind_error(
                                    SqlState::UndefinedColumn,
                                    format!(
                                        "relation \"{}\" has no id column for vector_top_k_ids() has_id filter",
                                        table.name
                                    ),
                                ))
                            },
                            |column| Ok((0, column, Vec::new())),
                        )?
                    } else {
                        resolve_vector_filter_key(table, &condition.key)?
                    };
                    let predicate = match &condition.predicate {
                        VectorTopKFilterPredicateSpec::Match(raw_match) => {
                            let expected =
                                coerce_vector_filter_match_value(raw_match, &column.data_type)?;
                            CompiledVectorTopKFilterPredicate::Match(expected)
                        }
                        VectorTopKFilterPredicateSpec::MatchAny(raw_matches) => {
                            let expected_values = raw_matches
                                .iter()
                                .map(|raw_match| {
                                    coerce_vector_filter_match_value(
                                        raw_match,
                                        &column.data_type,
                                    )
                                })
                                .collect::<DbResult<Vec<_>>>()?;
                            CompiledVectorTopKFilterPredicate::MatchAny(expected_values)
                        }
                        VectorTopKFilterPredicateSpec::MatchExcept(raw_matches) => {
                            let excluded_values = raw_matches
                                .iter()
                                .map(|raw_match| {
                                    coerce_vector_filter_match_value(
                                        raw_match,
                                        &column.data_type,
                                    )
                                })
                                .collect::<DbResult<Vec<_>>>()?;
                            CompiledVectorTopKFilterPredicate::MatchExcept(excluded_values)
                        }
                        VectorTopKFilterPredicateSpec::MatchText(text) => {
                            CompiledVectorTopKFilterPredicate::MatchText(text.to_lowercase())
                        }
                        VectorTopKFilterPredicateSpec::IsNull => {
                            CompiledVectorTopKFilterPredicate::IsNull
                        }
                        VectorTopKFilterPredicateSpec::IsEmpty => {
                            CompiledVectorTopKFilterPredicate::IsEmpty
                        }
                        VectorTopKFilterPredicateSpec::HasId(raw_ids) => {
                            let expected_values = raw_ids
                                .iter()
                                .map(|raw_id| {
                                    coerce_vector_filter_match_value(raw_id, &column.data_type)
                                })
                                .collect::<DbResult<Vec<_>>>()?;
                            CompiledVectorTopKFilterPredicate::MatchAny(expected_values)
                        }
                        VectorTopKFilterPredicateSpec::ValuesCount(values_count) => {
                            CompiledVectorTopKFilterPredicate::ValuesCount {
                                gt: values_count.gt,
                                gte: values_count.gte,
                                lt: values_count.lt,
                                lte: values_count.lte,
                            }
                        }
                        VectorTopKFilterPredicateSpec::Range(range) => {
                            if json_path.is_empty()
                                && !vector_filter_supports_numeric_range(&column.data_type)
                            {
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
                        json_path,
                        predicate,
                    });
                }
                Ok(compiled)
            };

        let mut filter = CompiledVectorTopKFilter {
            must: compile_clause(&filter_spec.must)?,
            should: compile_clause(&filter_spec.should)?,
            must_not: compile_clause(&filter_spec.must_not)?,
            min_should: filter_spec
                .min_should
                .as_ref()
                .map(|min_should| {
                    Ok(CompiledVectorTopKMinShould {
                        conditions: compile_clause(&min_should.conditions)?,
                        min_count: min_should.min_count,
                    })
                })
                .transpose()?,
        };
        if filter
            .min_should
            .as_ref()
            .is_some_and(|min_should| min_should.min_count == 0)
        {
            filter.min_should = None;
        }
        if filter.must.is_empty()
            && filter.should.is_empty()
            && filter.must_not.is_empty()
            && filter.min_should.is_none()
        {
            Ok(None)
        } else {
            Ok(Some(filter))
        }
    }

    pub(in crate::executor) fn collect_vector_filter_matching_tuple_ids(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        payload_filter: &CompiledVectorTopKFilter,
    ) -> DbResult<std::collections::HashSet<aiondb_core::TupleId>> {
        if payload_filter.is_impossible() {
            return Ok(std::collections::HashSet::new());
        }

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
            if !condition.json_path.is_empty() {
                return false;
            }
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
        let indexed_min_should = payload_filter
            .min_should
            .as_ref()
            .map(|min_should| {
                min_should
                    .conditions
                    .iter()
                    .filter(|condition| is_indexable(condition))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let all_must_indexed = indexed_must.len() == payload_filter.must.len();
        let all_should_indexed = !payload_filter.should.is_empty()
            && indexed_should.len() == payload_filter.should.len();
        let all_should_covered = payload_filter.should.is_empty() || all_should_indexed;
        let all_must_not_indexed = indexed_must_not.len() == payload_filter.must_not.len();

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
        if all_should_indexed {
            for condition in &indexed_should {
                let matches = collect_condition_matches(condition)?;
                should_ids.extend(matches);
            }
        }

        let all_min_should_indexed = payload_filter.min_should.as_ref().is_some_and(|min_should| {
            min_should.min_count > 0 && indexed_min_should.len() == min_should.conditions.len()
        });
        let mut min_should_ids = std::collections::HashSet::new();
        if all_min_should_indexed {
            let min_count = payload_filter
                .min_should
                .as_ref()
                .map_or(usize::MAX, |min_should| min_should.min_count);
            if min_count == 1 {
                for condition in &indexed_min_should {
                    let matches = collect_condition_matches(condition)?;
                    min_should_ids.extend(matches);
                }
            } else {
                let mut counts = std::collections::HashMap::new();
                for condition in &indexed_min_should {
                    let matches = collect_condition_matches(condition)?;
                    for tuple_id in matches {
                        let count = counts.entry(tuple_id).or_insert(0usize);
                        *count = count.saturating_add(1);
                        if *count >= min_count {
                            min_should_ids.insert(tuple_id);
                        }
                    }
                }
            }
            if min_should_ids.is_empty() {
                return Ok(min_should_ids);
            }
        }

        let all_min_should_covered =
            payload_filter.min_should.is_none() || all_min_should_indexed;
        let intersect_candidate_ids = |
            candidate_ids: &mut Option<std::collections::HashSet<aiondb_core::TupleId>>,
            ids: std::collections::HashSet<aiondb_core::TupleId>,
        | {
            if let Some(candidate_ids) = candidate_ids {
                candidate_ids.retain(|tuple_id| ids.contains(tuple_id));
            } else {
                *candidate_ids = Some(ids);
            }
        };
        let mut candidate_ids = None;
        if !indexed_must.is_empty() {
            candidate_ids = Some(required_ids);
        }
        if all_should_indexed {
            intersect_candidate_ids(&mut candidate_ids, should_ids);
        }
        if all_min_should_indexed {
            intersect_candidate_ids(&mut candidate_ids, min_should_ids);
        }

        let Some(mut candidate_ids) = candidate_ids else {
            let mut stream = self.scan_table_locked(context, table_id, None)?;
            let mut tuple_ids = std::collections::HashSet::new();
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                if payload_filter.matches(&record.row) {
                    tuple_ids.insert(record.tuple_id);
                }
            }
            return Ok(tuple_ids);
        };
        if candidate_ids.is_empty() {
            return Ok(candidate_ids);
        }
        let mut excluded_ids = std::collections::HashSet::new();
        for condition in &indexed_must_not {
            let matches = collect_condition_matches(condition)?;
            excluded_ids.extend(matches);
        }
        if !excluded_ids.is_empty() {
            candidate_ids.retain(|tuple_id| !excluded_ids.contains(tuple_id));
        }
        if candidate_ids.is_empty() {
            return Ok(candidate_ids);
        }
        if all_must_indexed
            && all_should_covered
            && all_must_not_indexed
            && all_min_should_covered
        {
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

    pub(in crate::executor) fn collect_vector_top_k_ids_hnsw(
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

    pub(in crate::executor) fn collect_vector_top_k_ids_hnsw_once(
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
        let mut ids = Vec::with_capacity(search_limit);
        let mut seen_ids = std::collections::HashSet::<i64>::with_capacity(search_limit);
        let mut fetched_rows = 0usize;
        while let Some(mut record) = stream.next()? {
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
            let first_val = if record.row.values.is_empty() {
                Value::Null
            } else {
                std::mem::replace(&mut record.row.values[0], Value::Null)
            };
            let Some(id) = read_bigint_value(first_val)? else {
                continue;
            };
            if seen_ids.insert(id) {
                ids.push(Value::BigInt(id));
            }
        }
        Ok((ids, fetched_rows))
    }

    pub(in crate::executor) fn collect_vector_top_k_ids_exact(
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
        let mut top_scores = std::collections::BinaryHeap::<VectorTopKScore>::new();
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
                        let Some(id_value) = read_bigint_first_value(&row.values)? else {
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
                for (distance, id) in scored_opts.into_iter().flatten() {
                    push_vector_top_k_score(&mut top_scores, requested_result_count, distance, id);
                }
                used_tuple_fetch = true;
            }
        }
        if !used_tuple_fetch {
            let mut stream = self.scan_table_locked(context, table.table_id, projected_columns)?;
            while let Some(mut record) = stream.next()? {
                context.check_deadline()?;
                if payload_filter
                    .as_ref()
                    .is_some_and(|filter| !filter.matches(&record.row))
                {
                    continue;
                }
                // Move the first column out of the row (replaced by Null) so
                // `coerce_value` consumes an owned Value without cloning. The
                // candidate vector lives at a later ordinal so leaving Null at
                // index 0 does not disturb the subsequent access.
                let first_val = if record.row.values.is_empty() {
                    Value::Null
                } else {
                    std::mem::replace(&mut record.row.values[0], Value::Null)
                };
                let Some(id_value) = read_bigint_value(first_val)? else {
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
                push_vector_top_k_score(
                    &mut top_scores,
                    requested_result_count,
                    sortable_distance,
                    id_value,
                );
            }
        }
        let failed = std::cell::Cell::new(false);
        let error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
        let mut scored = top_scores.into_vec();
        scored.sort_by(|left, right| {
            if failed.get() {
                return Ordering::Equal;
            }
            if let Err(e) = context.check_deadline() {
                failed.set(true);
                *error.borrow_mut() = Some(e);
                return Ordering::Equal;
            }
            left.distance
                .total_cmp(&right.distance)
                .then_with(|| left.id.cmp(&right.id))
        });
        if let Some(e) = error.into_inner() {
            return Err(e);
        }
        let final_count = requested_result_count.saturating_sub(offset);
        Ok(scored
            .into_iter()
            .skip(offset)
            .take(final_count)
            .map(|score| Value::BigInt(score.id))
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

    pub(in crate::executor) fn find_view_by_oid(
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

    pub(in crate::executor) fn find_view_by_name(
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

    pub(in crate::executor) fn find_view_in_known_schemas<F>(
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

    pub(in crate::executor) fn find_index_by_oid(
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

    pub(in crate::executor) fn find_index_by_name(
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

    pub(in crate::executor) fn find_table_by_oid(
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

    pub(in crate::executor) fn find_relation_by_oid(
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

    pub(in crate::executor) fn find_relation_by_name(
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

    pub(in crate::executor) fn estimate_relation_size(
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

    pub(in crate::executor) fn estimate_table_size(
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

    pub(in crate::executor) fn estimate_table_indexes_size(
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

    pub(in crate::executor) fn estimate_index_size(
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

fn resolve_vector_filter_key<'a>(
    table: &'a TableDescriptor,
    key: &str,
) -> DbResult<(usize, &'a ColumnDescriptor, Vec<String>)> {
    if let Some((ordinal, column)) = table
        .columns
        .iter()
        .enumerate()
        .find(|(_, column)| column.name.eq_ignore_ascii_case(key))
    {
        return Ok((ordinal, column, Vec::new()));
    }

    if let Some(array_key) = strip_qdrant_array_marker(key) {
        if let Some((ordinal, column)) = table
            .columns
            .iter()
            .enumerate()
            .find(|(_, column)| column.name.eq_ignore_ascii_case(array_key))
        {
            return Ok((ordinal, column, Vec::new()));
        }
    }

    let Some((column_name, json_path)) = key.split_once('.') else {
        return Err(DbError::bind_error(
            SqlState::UndefinedColumn,
            format!(
                "column \"{key}\" does not exist on relation \"{}\"",
                table.name
            ),
        ));
    };
    let column_name = strip_qdrant_array_marker(column_name).unwrap_or(column_name);
    let Some((ordinal, column)) = table
        .columns
        .iter()
        .enumerate()
        .find(|(_, column)| column.name.eq_ignore_ascii_case(column_name))
    else {
        return Err(DbError::bind_error(
            SqlState::UndefinedColumn,
            format!(
                "column \"{key}\" does not exist on relation \"{}\"",
                table.name
            ),
        ));
    };
    if !matches!(column.data_type, DataType::Jsonb) {
        return Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            format!(
                "vector_top_k_ids() options.filter key \"{key}\" requires JSONB column \"{}\"",
                column.name
            ),
        ));
    }
    let json_path = json_path
        .split('.')
        .filter(|part| !part.is_empty())
        .map(|part| strip_qdrant_array_marker(part).unwrap_or(part).to_owned())
        .collect::<Vec<_>>();
    if json_path.is_empty() || json_path.iter().any(String::is_empty) {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "vector_top_k_ids() options.filter key \"{key}\" requires a JSON path after the column name"
            ),
        ));
    }
    Ok((ordinal, column, json_path))
}

fn strip_qdrant_array_marker(key: &str) -> Option<&str> {
    key.strip_suffix("[]")
}

#[cfg(test)]
mod tests {
    use super::{keep_top_k_ordered_by, push_vector_top_k_score};

    #[test]
    fn keep_top_k_ordered_by_keeps_stable_top_window() {
        let mut values = vec![(5_i64, 0.4_f64), (2, 0.9), (9, 0.7), (1, 0.9), (3, 0.2)];

        keep_top_k_ordered_by(&mut values, 3, |left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });

        assert_eq!(values, vec![(1, 0.9), (2, 0.9), (9, 0.7)]);
    }

    #[test]
    fn keep_top_k_ordered_by_zero_clears_values() {
        let mut values = vec![3_i64, 2, 1];

        keep_top_k_ordered_by(&mut values, 0, Ord::cmp);

        assert!(values.is_empty());
    }

    #[test]
    fn keep_top_k_ordered_by_matches_full_sort_across_window_sizes() {
        let input = (0_i64..257)
            .map(|id| {
                let score = ((id * 37 + 11) % 19) as f64 / 19.0;
                (id, score)
            })
            .collect::<Vec<_>>();

        for k in [1_usize, 2, 3, 5, 8, 13, 21, 34, 55, 144, 257, 300] {
            let mut expected = input.clone();
            expected.sort_by(compare_score_desc_id_asc);
            expected.truncate(k);

            let mut actual = input.clone();
            keep_top_k_ordered_by(&mut actual, k, compare_score_desc_id_asc);

            assert_eq!(actual, expected, "k={k}");
        }
    }

    fn compare_score_desc_id_asc(left: &(i64, f64), right: &(i64, f64)) -> std::cmp::Ordering {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    }

    #[test]
    fn push_vector_top_k_score_keeps_best_distance_window() {
        let mut heap = std::collections::BinaryHeap::new();
        for (distance, id) in [(0.5, 5), (0.2, 9), (0.2, 3), (0.1, 8), (0.4, 1)] {
            push_vector_top_k_score(&mut heap, 3, distance, id);
        }

        let mut scores = heap.into_vec();
        scores.sort_by(|left, right| {
            left.distance
                .total_cmp(&right.distance)
                .then_with(|| left.id.cmp(&right.id))
        });
        let actual = scores
            .into_iter()
            .map(|score| (score.distance, score.id))
            .collect::<Vec<_>>();

        assert_eq!(actual, vec![(0.1, 8), (0.2, 3), (0.2, 9)]);
    }
}
