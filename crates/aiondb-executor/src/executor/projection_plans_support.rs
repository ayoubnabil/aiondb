use super::*;

#[derive(Clone, Debug, Default)]
#[allow(clippy::option_option)]
pub(crate) struct InSubqueryCacheEntry {
    pub(super) values: Vec<Value>,
    pub(super) hash_index: std::collections::HashMap<ValueHashKey, Vec<usize>>,
    pub(super) first_value_type: Option<Option<DataType>>,
    pub(super) homogeneous_type: bool,
    pub(super) all_hashable: bool,
    pub(super) has_null: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ProjectionCollectBounds {
    pub scan_offset: u64,
    pub stream_limit: Option<u64>,
    pub final_limit: Option<u64>,
}

const COLLECT_CAPACITY_HINT_MAX: usize = 16_384;

#[inline]
pub(crate) fn collect_capacity_hint(
    bounds: &ProjectionCollectBounds,
    context: &ExecutionContext,
) -> usize {
    let hinted_limit = bounds
        .stream_limit
        .or(bounds.final_limit)
        .unwrap_or(0)
        .min(context.max_result_rows);
    if hinted_limit == 0 {
        return 0;
    }
    clamp_u64_to_usize(hinted_limit, COLLECT_CAPACITY_HINT_MAX)
}

pub(crate) fn projection_collect_bounds_internal(
    plan_limit: Option<u64>,
    plan_offset: u64,
    context: &ExecutionContext,
    can_apply_offsets_during_scan: bool,
) -> ProjectionCollectBounds {
    let fl = final_collect_limit(plan_limit, context);
    ProjectionCollectBounds {
        scan_offset: if can_apply_offsets_during_scan {
            plan_offset.saturating_add(context.collect_row_offset)
        } else {
            0
        },
        stream_limit: if can_apply_offsets_during_scan {
            fl
        } else {
            None
        },
        final_limit: fl,
    }
}

pub(crate) fn final_collect_limit(
    plan_limit: Option<u64>,
    context: &ExecutionContext,
) -> Option<u64> {
    effective_collect_limit(
        plan_limit.map(|limit| limit.saturating_sub(context.collect_row_offset)),
        context.collect_row_limit,
    )
}

pub(crate) fn projection_apply_offset(
    rows: &mut Vec<Row>,
    evaluator: &ExpressionEvaluator,
    offset: Option<&TypedExpr>,
    context: &ExecutionContext,
) -> DbResult<bool> {
    let offset_val = offset
        .map(|expr| eval_limit_offset_expr(evaluator, expr, "OFFSET"))
        .transpose()?
        .unwrap_or(0)
        .saturating_add(context.collect_row_offset);
    if offset_val > 0 {
        let skip = clamp_u64_to_usize(offset_val, rows.len());
        if skip == rows.len() {
            rows.clear();
            return Ok(true);
        }
        rows.drain(..skip);
    }
    Ok(false)
}

pub(crate) fn projection_total_offset(
    evaluator: &ExpressionEvaluator,
    offset: Option<&TypedExpr>,
    context: &ExecutionContext,
) -> DbResult<u64> {
    let plan_offset = offset
        .map(|expr| eval_limit_offset_expr(evaluator, expr, "OFFSET"))
        .transpose()?
        .unwrap_or(0);
    Ok(plan_offset.saturating_add(context.collect_row_offset))
}

pub(crate) fn enforce_final_row_limits(
    context: &ExecutionContext,
    rows: &mut [Row],
) -> DbResult<()> {
    if usize_to_u64(rows.len()) > context.max_result_rows {
        return Err(DbError::program_limit(
            "maximum number of result rows reached",
        ));
    }
    let mut result_bytes = 0u64;
    for row in rows.iter() {
        result_bytes = ensure_result_bytes_fit_and_track_query_row(context, row, result_bytes)?;
    }
    Ok(())
}

/// Sort rows by all column values in ascending order (NULLS LAST).
/// This is used after DISTINCT deduplication when no explicit ORDER BY is present,
/// to produce deterministic output matching `PostgreSQL`'s effective behavior.
pub(crate) fn sort_distinct_rows(rows: &mut [Row], context: &ExecutionContext) -> DbResult<()> {
    let failed = std::cell::Cell::new(false);
    let error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
    rows.sort_by(|a, b| {
        if failed.get() {
            return Ordering::Equal;
        }
        if let Err(e) = context.check_deadline() {
            failed.set(true);
            *error.borrow_mut() = Some(e);
            return Ordering::Equal;
        }
        for (av, bv) in a.values.iter().zip(b.values.iter()) {
            // NULLS LAST: both null => Equal, left null => Greater, right null => Less
            let cmp = match (av, bv) {
                (Value::Null, Value::Null) => Ordering::Equal,
                (Value::Null, _) => Ordering::Greater,
                (_, Value::Null) => Ordering::Less,
                _ => match compare_runtime_values(av, bv) {
                    Ok(Some(ord)) => ord,
                    Ok(None) => Ordering::Equal,
                    Err(e) => {
                        failed.set(true);
                        *error.borrow_mut() = Some(e);
                        return Ordering::Equal;
                    }
                },
            };
            if cmp != Ordering::Equal {
                return cmp;
            }
        }
        Ordering::Equal
    });
    match error.into_inner() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

pub(crate) fn apply_distinct_on(
    executor: &Executor,
    rows: &mut Vec<Row>,
    distinct_on: &[TypedExpr],
    context: &ExecutionContext,
) -> DbResult<()> {
    apply_distinct_on_with(rows, distinct_on, context, |_, expr, row| {
        executor.evaluate_expr_with_row(expr, row, context)
    })
}

pub(crate) fn apply_distinct_on_with<F>(
    rows: &mut Vec<Row>,
    distinct_on: &[TypedExpr],
    context: &ExecutionContext,
    mut eval_key_expr: F,
) -> DbResult<()>
where
    F: FnMut(usize, &TypedExpr, &Row) -> DbResult<Value>,
{
    let mut seen = std::collections::HashSet::<Vec<ValueHashKey>>::with_capacity(rows.len());
    let mut distinct_rows = Vec::with_capacity(rows.len());
    for row in rows.drain(..) {
        context.check_deadline()?;
        let key: Vec<ValueHashKey> = distinct_on
            .iter()
            .enumerate()
            .map(|(position, expr)| {
                let value = eval_key_expr(position, expr, &row)?;
                build_hash_key(&value)
            })
            .collect::<DbResult<_>>()?;
        if seen.insert(key) {
            distinct_rows.push(row);
        }
    }
    *rows = distinct_rows;
    Ok(())
}

pub(crate) fn rebase_distinct_on_to_output_ordinals(
    outputs: &[ProjectionExpr],
    distinct_on: &[TypedExpr],
) -> Vec<TypedExpr> {
    distinct_on
        .iter()
        .map(|expr| rebind_projected_expr_to_output(outputs, expr).unwrap_or_else(|| expr.clone()))
        .collect()
}

pub(crate) fn rebase_order_by_to_output_ordinals(
    outputs: &[ProjectionExpr],
    order_by: &[SortExpr],
) -> Vec<SortExpr> {
    order_by
        .iter()
        .map(|sort| SortExpr {
            expr: rebind_projected_expr_to_output(outputs, &sort.expr)
                .unwrap_or_else(|| sort.expr.clone()),
            descending: sort.descending,
            nulls_first: sort.nulls_first,
        })
        .collect()
}

pub(crate) fn rebind_projected_expr_to_output(
    outputs: &[ProjectionExpr],
    expr: &TypedExpr,
) -> Option<TypedExpr> {
    if let Some((ordinal, output)) = outputs
        .iter()
        .enumerate()
        .find(|(_, output)| output.expr == *expr)
    {
        return Some(TypedExpr::column_ref(
            &output.field.name,
            ordinal,
            output.field.data_type.clone(),
            output.field.nullable,
        ));
    }

    let column_name = match &expr.kind {
        TypedExprKind::ColumnRef { name, .. } | TypedExprKind::OuterColumnRef { name, .. } => name,
        _ => return None,
    };

    let mut matches = outputs
        .iter()
        .enumerate()
        .filter(|(_, output)| projection_matches_distinct_on_name(output, column_name));
    let (ordinal, output) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }

    Some(TypedExpr::column_ref(
        &output.field.name,
        ordinal,
        output.field.data_type.clone(),
        output.field.nullable,
    ))
}

pub(crate) fn projection_matches_distinct_on_name(
    output: &ProjectionExpr,
    column_name: &str,
) -> bool {
    output.field.name.eq_ignore_ascii_case(column_name)
        || matches!(
            &output.expr.kind,
            TypedExprKind::ColumnRef { name, .. } | TypedExprKind::OuterColumnRef { name, .. }
                if name.eq_ignore_ascii_case(column_name)
        )
}

const COMPAT_SYSTEM_COLUMNS: [&str; 7] =
    ["ctid", "tableoid", "xmin", "xmax", "cmin", "cmax", "oid"];

pub(crate) fn is_compat_system_column_name(name: &str) -> bool {
    COMPAT_SYSTEM_COLUMNS
        .iter()
        .any(|column| name.eq_ignore_ascii_case(column))
}

pub(crate) fn project_table_needs_compat_row(
    outputs: &[aiondb_plan::ProjectionExpr],
    filter: Option<&TypedExpr>,
    order_by: &[aiondb_plan::SortExpr],
    distinct_on: &[TypedExpr],
) -> bool {
    outputs
        .iter()
        .any(|output| expr_references_compat_system_column(&output.expr))
        || filter.is_some_and(expr_references_compat_system_column)
        || order_by
            .iter()
            .any(|sort| expr_references_compat_system_column(&sort.expr))
        || distinct_on.iter().any(expr_references_compat_system_column)
}

fn push_expr_children<'a>(expr: &'a TypedExpr, stack: &mut Vec<&'a TypedExpr>) {
    match &expr.kind {
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryLe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::LogicalAnd { left, right }
        | TypedExprKind::LogicalOr { left, right }
        | TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right }
        | TypedExprKind::Concat { left, right }
        | TypedExprKind::JsonGet { left, right }
        | TypedExprKind::JsonGetText { left, right }
        | TypedExprKind::JsonPathGet { left, right }
        | TypedExprKind::JsonPathGetText { left, right }
        | TypedExprKind::JsonContains { left, right }
        | TypedExprKind::JsonContainedBy { left, right }
        | TypedExprKind::JsonKeyExists { left, right }
        | TypedExprKind::JsonAnyKeyExists { left, right }
        | TypedExprKind::JsonAllKeysExist { left, right }
        | TypedExprKind::ArrayConcat { left, right }
        | TypedExprKind::ArrayContains { left, right }
        | TypedExprKind::ArrayContainedBy { left, right }
        | TypedExprKind::ArrayOverlap { left, right }
        | TypedExprKind::IsDistinctFrom { left, right, .. }
        | TypedExprKind::Nullif { left, right } => {
            stack.push(right);
            stack.push(left);
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. }
        | TypedExprKind::InSubquery { expr, .. } => stack.push(expr),
        TypedExprKind::Like { expr, pattern, .. } => {
            stack.push(pattern);
            stack.push(expr);
        }
        TypedExprKind::InList { expr, list, .. } => {
            stack.extend(list);
            stack.push(expr);
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            stack.push(high);
            stack.push(low);
            stack.push(expr);
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            if let Some(expr) = else_result {
                stack.push(expr);
            }
            stack.extend(results);
            stack.extend(conditions);
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args }
        | TypedExprKind::UserFunction { args, .. } => stack.extend(args),
        TypedExprKind::AggCount { expr, filter, .. } => {
            if let Some(expr) = expr {
                stack.push(expr);
            }
            if let Some(filter) = filter {
                stack.push(filter);
            }
        }
        TypedExprKind::AggSum { expr, filter, .. }
        | TypedExprKind::AggAvg { expr, filter, .. }
        | TypedExprKind::AggAnyValue { expr, filter }
        | TypedExprKind::AggMin { expr, filter }
        | TypedExprKind::AggMax { expr, filter }
        | TypedExprKind::AggBoolAnd { expr, filter }
        | TypedExprKind::AggBoolOr { expr, filter }
        | TypedExprKind::AggStddevPop { expr, filter }
        | TypedExprKind::AggStddevSamp { expr, filter }
        | TypedExprKind::AggVarPop { expr, filter }
        | TypedExprKind::AggVarSamp { expr, filter }
        | TypedExprKind::AggArrayAgg { expr, filter, .. } => {
            stack.push(expr);
            if let Some(filter) = filter {
                stack.push(filter);
            }
        }
        TypedExprKind::AggStringAgg {
            expr,
            delimiter,
            filter,
            ..
        } => {
            stack.push(delimiter);
            stack.push(expr);
            if let Some(filter) = filter {
                stack.push(filter);
            }
        }
        TypedExprKind::WindowFunction {
            args,
            partition_by,
            order_by,
            ..
        } => {
            for sort in order_by {
                stack.push(&sort.expr);
            }
            stack.extend(partition_by);
            stack.extend(args);
        }
        TypedExprKind::Literal(_)
        | TypedExprKind::ColumnRef { .. }
        | TypedExprKind::OuterColumnRef { .. }
        | TypedExprKind::NextValue { .. }
        | TypedExprKind::ScalarSubquery { .. }
        | TypedExprKind::ArraySubquery { .. }
        | TypedExprKind::ExistsSubquery { .. } => {}
    }
}

pub(crate) fn expr_contains_in_subquery(expr: &TypedExpr) -> bool {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        if matches!(expr.kind, TypedExprKind::InSubquery { .. }) {
            return true;
        }
        push_expr_children(expr, &mut stack);
    }
    false
}

pub(crate) fn expr_requires_special_resolution(expr: &TypedExpr) -> bool {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::OuterColumnRef { .. }
            | TypedExprKind::NextValue { .. }
            | TypedExprKind::ScalarSubquery { .. }
            | TypedExprKind::ArraySubquery { .. }
            | TypedExprKind::InSubquery { .. }
            | TypedExprKind::ExistsSubquery { .. }
            | TypedExprKind::UserFunction { .. } => return true,
            TypedExprKind::ScalarFunction {
                func:
                    aiondb_plan::ScalarFunction::PgGetViewdef | aiondb_plan::ScalarFunction::Generic(_),
                ..
            } => {
                return true;
            }
            _ => push_expr_children(expr, &mut stack),
        }
    }
    false
}

pub(crate) fn expr_references_compat_system_column(expr: &TypedExpr) -> bool {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::ColumnRef { name, .. } | TypedExprKind::OuterColumnRef { name, .. } => {
                if is_compat_system_column_name(name) {
                    return true;
                }
            }
            // Conservative fallback: subqueries can capture outer rows.
            TypedExprKind::ScalarSubquery { .. }
            | TypedExprKind::ArraySubquery { .. }
            | TypedExprKind::ExistsSubquery { .. } => return true,
            _ => push_expr_children(expr, &mut stack),
        }
    }
    false
}

pub(crate) fn is_srf_output(expr: &TypedExpr) -> bool {
    match &expr.kind {
        TypedExprKind::Cast { expr, .. } => is_srf_output(expr),
        TypedExprKind::ScalarFunction { func, args } => {
            matches!(
                func,
                aiondb_plan::ScalarFunction::GenerateSeries
                    | aiondb_plan::ScalarFunction::RegexpSplitToTable
                    | aiondb_plan::ScalarFunction::RegexpMatches
                    | aiondb_plan::ScalarFunction::Unnest
            ) || matches!(
                func,
                aiondb_plan::ScalarFunction::Generic(name)
                    if name.eq_ignore_ascii_case("generate_subscripts")
                        || name.eq_ignore_ascii_case("graph_neighbors")
                        || name.eq_ignore_ascii_case("jsonb_each")
                        || name.eq_ignore_ascii_case("jsonb_each_text")
                        || name.eq_ignore_ascii_case("jsonb_array_elements")
                        || name.eq_ignore_ascii_case("jsonb_array_elements_text")
                        || name.eq_ignore_ascii_case("__aiondb_jsonb_to_recordset")
                        || name.eq_ignore_ascii_case("__aiondb_json_to_recordset")
                        || name.eq_ignore_ascii_case("__aiondb_jsonb_populate_recordset")
                        || name.eq_ignore_ascii_case("__aiondb_json_populate_recordset")
                        || name.eq_ignore_ascii_case("jsonb_to_recordset")
                        || name.eq_ignore_ascii_case("json_to_recordset")
                        || name.eq_ignore_ascii_case("jsonb_populate_recordset")
                        || name.eq_ignore_ascii_case("json_populate_recordset")
                        || name.eq_ignore_ascii_case("jsonb_path_query")
                        || name.eq_ignore_ascii_case("string_to_table")
                        || name.eq_ignore_ascii_case("vector_top_k_ids")
                        || name.eq_ignore_ascii_case("vector_top_k_hits")
                        || name.eq_ignore_ascii_case("vector_prefetch_top_k_hits")
                        || name.eq_ignore_ascii_case("vector_recommend_top_k_hits")
                        || name.eq_ignore_ascii_case("full_text_top_k_hits")
                        || name.eq_ignore_ascii_case("hybrid_search_top_k_hits")
                        || name.eq_ignore_ascii_case("hybrid_fuse_rrf_hits")
                        || name.eq_ignore_ascii_case("hybrid_fuse_dbsf_hits")
                        || name.eq_ignore_ascii_case("hybrid_group_hits_by")
                        || name.eq_ignore_ascii_case("pg_ls_dir")
                        || name.eq_ignore_ascii_case("pg_ls_archive_statusdir")
                        || name.eq_ignore_ascii_case("pg_ls_logdir")
                        || name.eq_ignore_ascii_case("pg_ls_tmpdir")
            ) || args.iter().any(is_srf_output)
        }
        TypedExprKind::UserFunction { body, args, .. } => {
            user_function_body_is_set_returning(body) || args.iter().any(is_srf_output)
        }
        _ => false,
    }
}

fn user_function_body_is_set_returning(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("return next") || lower.contains("return query")
}

pub(crate) fn expand_srf_rows(values: &[Value], srf_indices: &[usize]) -> Vec<Row> {
    let row_count = srf_indices
        .iter()
        .filter_map(|index| match values.get(*index) {
            Some(Value::Array(elements)) => Some(elements.len()),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let mut srf_mask = vec![false; values.len()];
    for index in srf_indices {
        if *index < srf_mask.len() {
            srf_mask[*index] = true;
        }
    }
    let mut rows = Vec::with_capacity(row_count);
    for row_index in 0..row_count {
        let mut expanded = Vec::with_capacity(values.len());
        for (value_index, value) in values.iter().enumerate() {
            if srf_mask[value_index] {
                match value {
                    Value::Array(elements) => {
                        expanded.push(elements.get(row_index).cloned().unwrap_or(Value::Null));
                    }
                    other => expanded.push(other.clone()),
                }
            } else {
                expanded.push(value.clone());
            }
        }
        rows.push(Row::new(expanded));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_plan::ResultField;

    #[test]
    fn rebase_distinct_on_to_output_ordinals_matches_unique_output_name_after_remap() {
        let outputs = vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        }];

        let rebased = rebase_distinct_on_to_output_ordinals(
            &outputs,
            &[TypedExpr::column_ref("id", 4, DataType::Int, false)],
        );

        assert_eq!(
            rebased,
            vec![TypedExpr::column_ref("id", 0, DataType::Int, false)]
        );
    }

    #[test]
    fn rebase_distinct_on_to_output_ordinals_keeps_ambiguous_name_unmodified() {
        let outputs = vec![
            ProjectionExpr {
                field: ResultField {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("left_id", 0, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("right_id", 1, DataType::Int, false),
            },
        ];
        let distinct_on = vec![TypedExpr::column_ref("id", 7, DataType::Int, false)];

        let rebased = rebase_distinct_on_to_output_ordinals(&outputs, &distinct_on);

        assert_eq!(rebased, distinct_on);
    }

    #[test]
    fn rebase_order_by_to_output_ordinals_matches_alias_name_after_projection() {
        let outputs = vec![ProjectionExpr {
            field: ResultField {
                name: "neighbor_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 3, DataType::BigInt, false),
        }];

        let rebased = rebase_order_by_to_output_ordinals(
            &outputs,
            &[SortExpr {
                expr: TypedExpr::column_ref("neighbor_id", 9, DataType::BigInt, false),
                descending: true,
                nulls_first: Some(false),
            }],
        );

        assert_eq!(
            rebased,
            vec![SortExpr {
                expr: TypedExpr::column_ref("neighbor_id", 0, DataType::BigInt, false),
                descending: true,
                nulls_first: Some(false),
            }]
        );
    }

    #[test]
    fn rebase_order_by_to_output_ordinals_keeps_ambiguous_alias_unmodified() {
        let outputs = vec![
            ProjectionExpr {
                field: ResultField {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("left_id", 0, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("right_id", 1, DataType::Int, false),
            },
        ];
        let order_by = vec![SortExpr {
            expr: TypedExpr::column_ref("id", 7, DataType::Int, false),
            descending: false,
            nulls_first: None,
        }];

        let rebased = rebase_order_by_to_output_ordinals(&outputs, &order_by);

        assert_eq!(rebased, order_by);
    }
}
