use super::*;
use aiondb_core::{bounded_hnsw_ef_search, HNSW_MAX_EF_SEARCH, VECTOR_MAX_K};
use std::hash::{Hash, Hasher};

fn plan_has_data_modifying_side_effects(plan: &PhysicalPlan) -> bool {
    match plan {
        PhysicalPlan::InsertValues { .. }
        | PhysicalPlan::DeleteFromTable { .. }
        | PhysicalPlan::UpdateTable { .. }
        | PhysicalPlan::MergeTable(_) => true,
        PhysicalPlan::InsertSelect { source, .. }
        | PhysicalPlan::CreateTableAs { source, .. }
        | PhysicalPlan::ProjectSource { source, .. }
        | PhysicalPlan::AggregateSource { source, .. }
        | PhysicalPlan::PartialAggregate { source, .. } => {
            plan_has_data_modifying_side_effects(source)
        }
        PhysicalPlan::SetOperation { left, right, .. }
        | PhysicalPlan::NestedLoopJoin { left, right, .. }
        | PhysicalPlan::HashJoin { left, right, .. }
        | PhysicalPlan::MergeJoin { left, right, .. } => {
            plan_has_data_modifying_side_effects(left)
                || plan_has_data_modifying_side_effects(right)
        }
        PhysicalPlan::NestedLoopIndexJoin { left, .. } => {
            plan_has_data_modifying_side_effects(left)
        }
        PhysicalPlan::BroadcastHashJoin {
            broadcast, local, ..
        } => {
            plan_has_data_modifying_side_effects(broadcast)
                || plan_has_data_modifying_side_effects(local)
        }
        PhysicalPlan::RecursiveCte {
            base, recursive, ..
        } => {
            plan_has_data_modifying_side_effects(base)
                || plan_has_data_modifying_side_effects(recursive)
        }
        PhysicalPlan::DistributedAppend { fragments, .. }
        | PhysicalPlan::FinalAggregate {
            partials: fragments,
            ..
        } => fragments.iter().any(plan_has_data_modifying_side_effects),
        _ => false,
    }
}

fn side_effect_project_cache_key(plan: &PhysicalPlan) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{plan:?}").hash(&mut hasher);
    hasher.finish()
}

fn is_single_column_passthrough(outputs: &[ProjectionExpr]) -> bool {
    matches!(
        outputs,
        [ProjectionExpr {
            expr: TypedExpr {
                kind: TypedExprKind::ColumnRef { ordinal: 0, .. },
                ..
            },
            ..
        }]
    )
}

fn fast_unnest_string_to_array_column(expr: &TypedExpr) -> Option<(usize, String)> {
    let TypedExprKind::ScalarFunction { func, args } = &expr.kind else {
        return None;
    };
    if !matches!(func, ScalarFunction::Unnest) || args.len() != 1 {
        return None;
    }
    let TypedExprKind::ScalarFunction {
        func: inner_func,
        args: inner_args,
    } = &args[0].kind
    else {
        return None;
    };
    if !matches!(inner_func, ScalarFunction::StringToArray) || inner_args.len() != 2 {
        return None;
    }
    let column_ordinal = derived_simple_column_ordinal(&inner_args[0])?;
    let Value::Text(delimiter) = derived_simple_literal(&inner_args[1])? else {
        return None;
    };
    Some((column_ordinal, delimiter))
}

fn fast_string_split_projection(outputs: &[ProjectionExpr]) -> Option<(usize, usize, String)> {
    let [id_output, split_output] = outputs else {
        return None;
    };
    let id_ordinal = derived_simple_column_ordinal(&id_output.expr)?;
    let (split_ordinal, delimiter) = fast_unnest_string_to_array_column(&split_output.expr)?;
    Some((id_ordinal, split_ordinal, delimiter))
}

#[derive(Clone, Copy)]
enum DerivedSimpleFilterOp {
    Eq,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Clone)]
struct DerivedSimpleFilter {
    column_ordinal: usize,
    op: DerivedSimpleFilterOp,
    literal: Value,
}

#[derive(Clone, Copy)]
enum DerivedSimpleAggOutput {
    GroupKey { group_index: usize },
    CountStar,
    Sum { projected_pos: usize },
    Avg { projected_pos: usize },
}

struct DerivedSimpleAggState {
    group_values: Vec<Value>,
    counts: Vec<i64>,
    sums: Vec<Option<Value>>,
}

impl DerivedSimpleAggState {
    fn new(group_values: Vec<Value>, output_count: usize) -> Self {
        Self {
            group_values,
            counts: vec![0; output_count],
            sums: vec![None; output_count],
        }
    }
}

fn derived_simple_column_ordinal(expr: &TypedExpr) -> Option<usize> {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => Some(*ordinal),
        TypedExprKind::Cast { expr, .. } => derived_simple_column_ordinal(expr),
        _ => None,
    }
}

fn derived_simple_literal(expr: &TypedExpr) -> Option<Value> {
    match &expr.kind {
        TypedExprKind::Literal(value) => Some(value.clone()),
        TypedExprKind::Cast { expr, .. } => derived_simple_literal(expr),
        _ => None,
    }
}

fn invert_derived_simple_filter_op(op: DerivedSimpleFilterOp) -> DerivedSimpleFilterOp {
    match op {
        DerivedSimpleFilterOp::Eq => DerivedSimpleFilterOp::Eq,
        DerivedSimpleFilterOp::Gt => DerivedSimpleFilterOp::Lt,
        DerivedSimpleFilterOp::Ge => DerivedSimpleFilterOp::Le,
        DerivedSimpleFilterOp::Lt => DerivedSimpleFilterOp::Gt,
        DerivedSimpleFilterOp::Le => DerivedSimpleFilterOp::Ge,
    }
}

#[allow(clippy::option_option)]
fn extract_derived_simple_filter(
    filter: Option<&TypedExpr>,
) -> Option<Option<DerivedSimpleFilter>> {
    let Some(filter) = filter else {
        return Some(None);
    };
    let (left, right, op) = match &filter.kind {
        TypedExprKind::BinaryEq { left, right } => {
            (left.as_ref(), right.as_ref(), DerivedSimpleFilterOp::Eq)
        }
        TypedExprKind::BinaryGt { left, right } => {
            (left.as_ref(), right.as_ref(), DerivedSimpleFilterOp::Gt)
        }
        TypedExprKind::BinaryGe { left, right } => {
            (left.as_ref(), right.as_ref(), DerivedSimpleFilterOp::Ge)
        }
        TypedExprKind::BinaryLt { left, right } => {
            (left.as_ref(), right.as_ref(), DerivedSimpleFilterOp::Lt)
        }
        TypedExprKind::BinaryLe { left, right } => {
            (left.as_ref(), right.as_ref(), DerivedSimpleFilterOp::Le)
        }
        TypedExprKind::Cast { expr, .. } => return extract_derived_simple_filter(Some(expr)),
        _ => return None,
    };

    if let (Some(column_ordinal), Some(literal)) = (
        derived_simple_column_ordinal(left),
        derived_simple_literal(right),
    ) {
        return Some(Some(DerivedSimpleFilter {
            column_ordinal,
            op,
            literal,
        }));
    }
    if let (Some(literal), Some(column_ordinal)) = (
        derived_simple_literal(left),
        derived_simple_column_ordinal(right),
    ) {
        return Some(Some(DerivedSimpleFilter {
            column_ordinal,
            op: invert_derived_simple_filter_op(op),
            literal,
        }));
    }
    None
}

fn derived_simple_filter_matches(value: &Value, filter: &DerivedSimpleFilter) -> DbResult<bool> {
    if matches!(value, Value::Null) || matches!(filter.literal, Value::Null) {
        return Ok(false);
    }
    let Some(ordering) = compare_runtime_values(value, &filter.literal)? else {
        return Ok(false);
    };
    Ok(match filter.op {
        DerivedSimpleFilterOp::Eq => ordering == Ordering::Equal,
        DerivedSimpleFilterOp::Gt => ordering == Ordering::Greater,
        DerivedSimpleFilterOp::Ge => ordering != Ordering::Less,
        DerivedSimpleFilterOp::Lt => ordering == Ordering::Less,
        DerivedSimpleFilterOp::Le => ordering != Ordering::Greater,
    })
}

fn derived_projected_position(required_ordinals: &[usize], ordinal: usize) -> Option<usize> {
    required_ordinals
        .iter()
        .position(|candidate| *candidate == ordinal)
}

fn project_table_outputs_are_identity(outputs: &[ProjectionExpr]) -> bool {
    !outputs.is_empty()
        && outputs
            .iter()
            .enumerate()
            .all(|(index, output)| derived_simple_column_ordinal(&output.expr) == Some(index))
}

fn aggregate_source_count_star_outputs(aggregates: &[ProjectionExpr]) -> bool {
    !aggregates.is_empty()
        && aggregates.iter().all(|projection| {
            matches!(
                &projection.expr.kind,
                TypedExprKind::AggCount {
                    expr: None,
                    distinct: false,
                    filter: None,
                }
            )
        })
}

fn remap_project_source_order_by_for_project_table_pushdown(
    order_by: &[SortExpr],
    child_outputs: &[ProjectionExpr],
) -> Option<Vec<SortExpr>> {
    order_by
        .iter()
        .map(|sort| {
            let TypedExprKind::ColumnRef { ordinal, .. } = sort.expr.kind else {
                return None;
            };
            let child_expr = child_outputs.get(ordinal)?.expr.clone();
            Some(SortExpr {
                expr: child_expr,
                descending: sort.descending,
                nulls_first: sort.nulls_first,
            })
        })
        .collect()
}

fn hnsw_project_source_expr_cache_key(expr: &TypedExpr) -> Option<HnswProjectSourceOutputCacheKey> {
    let expr = strip_hnsw_project_source_casts(expr);
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => {
            Some(HnswProjectSourceOutputCacheKey::Column(*ordinal))
        }
        TypedExprKind::ScalarFunction { func, args }
            if is_hnsw_project_l2_distance_func(func) && args.len() == 2 =>
        {
            let left = strip_hnsw_project_source_casts(&args[0]);
            let right = strip_hnsw_project_source_casts(&args[1]);
            let (vector_ordinal, query_expr) = match (&left.kind, &right.kind) {
                (TypedExprKind::ColumnRef { ordinal, .. }, _) => (*ordinal, right),
                (_, TypedExprKind::ColumnRef { ordinal, .. }) => (*ordinal, left),
                _ => return None,
            };
            let TypedExprKind::Literal(Value::Vector(query)) = &query_expr.kind else {
                return None;
            };
            Some(HnswProjectSourceOutputCacheKey::L2Distance {
                vector_ordinal,
                query_bits: query.values.iter().map(|value| value.to_bits()).collect(),
            })
        }
        _ => None,
    }
}

fn is_hnsw_project_l2_distance_func(func: &ScalarFunction) -> bool {
    matches!(func, ScalarFunction::L2Distance)
        || matches!(func, ScalarFunction::Generic(name) if name.eq_ignore_ascii_case("l2_distance"))
}

#[derive(Clone)]
struct NestedIndexJoinView<'a> {
    right_table_id: RelationId,
    right_index_id: IndexId,
    outer_key_ordinal: usize,
    join_type: JoinType,
    right_filter: Option<&'a TypedExpr>,
    filter: Option<&'a TypedExpr>,
}

fn strip_hnsw_project_source_casts(expr: &TypedExpr) -> &TypedExpr {
    let mut current = expr;
    while let TypedExprKind::Cast { expr, .. } = &current.kind {
        current = expr;
    }
    current
}

fn hnsw_project_source_cache_key(
    source: &PhysicalPlan,
    outputs: &[ProjectionExpr],
    filter: Option<&TypedExpr>,
    order_by: &[SortExpr],
    distinct: bool,
    distinct_on: &[TypedExpr],
    effective_limit: Option<u64>,
    plan_offset: u64,
    context: &ExecutionContext,
) -> Option<HnswProjectSourceResultCacheKey> {
    if filter.is_some()
        || !order_by.is_empty()
        || distinct
        || !distinct_on.is_empty()
        || plan_offset != 0
        || context.collect_row_offset != 0
    {
        return None;
    }
    let PhysicalPlan::HnswScan {
        table_id,
        index_id,
        query_vector,
        limit,
        ef_search,
        projected_ordinals,
        ..
    } = source
    else {
        return None;
    };
    let outputs = outputs
        .iter()
        .map(|output| hnsw_project_source_expr_cache_key(&output.expr))
        .collect::<Option<Vec<_>>>()?;
    Some(HnswProjectSourceResultCacheKey {
        table_id: *table_id,
        index_id: *index_id,
        query_bits: query_vector.iter().map(|value| value.to_bits()).collect(),
        hnsw_limit: *limit,
        ef_search: *ef_search,
        projected_ordinals: projected_ordinals.clone(),
        outputs,
        effective_limit,
        max_result_rows: context.max_result_rows,
    })
}

const ADAPTIVE_HNSW_SOURCE_LIMIT_CAP: usize = VECTOR_MAX_K;
const ADAPTIVE_HNSW_SAFETY_NUMERATOR: usize = 5;
const ADAPTIVE_HNSW_SAFETY_DENOMINATOR: usize = 4;
const ADAPTIVE_HNSW_MAX_GROWTH_FACTOR: usize = 6;

fn ceil_div_u128(value: u128, divisor: u128) -> u128 {
    if divisor == 0 {
        return value;
    }
    value.saturating_add(divisor.saturating_sub(1)) / divisor
}

fn next_adaptive_hnsw_limit(
    current_limit: usize,
    matched_rows: usize,
    target_rows: usize,
) -> usize {
    if current_limit == 0 {
        return 1;
    }
    if matched_rows >= target_rows {
        return current_limit;
    }
    let current_limit_u128 = current_limit as u128;
    let matched_rows_u128 = matched_rows.max(1) as u128;
    let target_rows_u128 = target_rows.max(1) as u128;
    let estimated_required = ceil_div_u128(
        target_rows_u128.saturating_mul(current_limit_u128),
        matched_rows_u128,
    );
    let safety_adjusted = ceil_div_u128(
        estimated_required.saturating_mul(ADAPTIVE_HNSW_SAFETY_NUMERATOR as u128),
        ADAPTIVE_HNSW_SAFETY_DENOMINATOR as u128,
    );
    let max_growth_limit =
        current_limit_u128.saturating_mul(ADAPTIVE_HNSW_MAX_GROWTH_FACTOR as u128);
    let cap_u128 = ADAPTIVE_HNSW_SOURCE_LIMIT_CAP as u128;
    let next_limit = safety_adjusted
        .max(current_limit_u128.saturating_add(1))
        .min(max_growth_limit)
        .min(cap_u128);
    usize::try_from(next_limit).unwrap_or(ADAPTIVE_HNSW_SOURCE_LIMIT_CAP)
}

#[cfg(test)]
mod adaptive_hnsw_growth_tests {
    use super::*;

    #[test]
    fn hnsw_project_cache_key_accepts_generic_l2_distance() {
        let vector_type = DataType::Vector {
            dims: 4,
            element_type: aiondb_core::VectorElementType::Float32,
        };
        let expr = TypedExpr::scalar_function(
            ScalarFunction::Generic("l2_distance".to_owned()),
            vec![
                TypedExpr::column_ref("embedding", 2, vector_type.clone(), true),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(4, vec![0.1, 0.2, 0.3, 0.4])),
                    vector_type,
                    false,
                ),
            ],
            DataType::Double,
            true,
        );

        assert!(matches!(
            hnsw_project_source_expr_cache_key(&expr),
            Some(HnswProjectSourceOutputCacheKey::L2Distance {
                vector_ordinal: 2,
                ..
            })
        ));
    }

    #[test]
    fn next_adaptive_hnsw_limit_is_monotonic_and_bounded() {
        let next = next_adaptive_hnsw_limit(512, 256, 512);
        assert!(next > 512, "limit should grow when target is not met");
        assert!(
            next <= 512 * ADAPTIVE_HNSW_MAX_GROWTH_FACTOR,
            "growth should stay under the configured per-step bound"
        );
    }

    #[test]
    fn next_adaptive_hnsw_limit_handles_zero_matches_with_max_growth_step() {
        let next = next_adaptive_hnsw_limit(256, 0, 64);
        assert_eq!(next, 256 * ADAPTIVE_HNSW_MAX_GROWTH_FACTOR);
    }

    #[test]
    fn next_adaptive_hnsw_limit_respects_global_cap() {
        let next = next_adaptive_hnsw_limit(9_000, 1, 10_000);
        assert_eq!(next, ADAPTIVE_HNSW_SOURCE_LIMIT_CAP);
    }
}

impl Executor {
    fn collect_nested_index_join_chain<'a>(
        plan: &'a PhysicalPlan,
        joins: &mut Vec<NestedIndexJoinView<'a>>,
    ) -> &'a PhysicalPlan {
        match plan {
            PhysicalPlan::NestedLoopIndexJoin {
                left,
                right_table_id,
                right_index_id,
                outer_key_ordinal,
                join_type,
                right_filter,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
                ..
            } if order_by.is_empty()
                && limit.is_none()
                && offset.is_none()
                && !*distinct
                && distinct_on.is_empty() =>
            {
                let base = Self::collect_nested_index_join_chain(left, joins);
                joins.push(NestedIndexJoinView {
                    right_table_id: *right_table_id,
                    right_index_id: *right_index_id,
                    outer_key_ordinal: *outer_key_ordinal,
                    join_type: *join_type,
                    right_filter: right_filter.as_ref(),
                    filter: filter.as_ref(),
                });
                base
            }
            _ => plan,
        }
    }

    fn plan_base_table_id(plan: &PhysicalPlan) -> Option<RelationId> {
        match plan {
            PhysicalPlan::ProjectTable { table_id, .. }
            | PhysicalPlan::LockingProjectTable { table_id, .. }
            | PhysicalPlan::SeqScan { table_id } => Some(*table_id),
            _ => None,
        }
    }

    fn table_name_is(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        expected: &str,
    ) -> DbResult<bool> {
        Ok(self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .is_some_and(|table| table.name.name.eq_ignore_ascii_case(expected)))
    }

    fn table_column_index(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        column_name: &str,
    ) -> DbResult<Option<usize>> {
        Ok(self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .and_then(|table| {
                table
                    .columns
                    .iter()
                    .position(|column| column.name.eq_ignore_ascii_case(column_name))
            }))
    }

    fn output_field_index(fields: &[aiondb_plan::ResultField], name: &str) -> Option<usize> {
        fields.iter().position(|field| {
            field.name.eq_ignore_ascii_case(name)
                || field
                    .name
                    .rsplit(['.', '\0'])
                    .next()
                    .is_some_and(|last| last.eq_ignore_ascii_case(name))
        })
    }

    fn literal_vector_from_min_l2_aggregate(
        &self,
        aggregates: &[ProjectionExpr],
    ) -> DbResult<Option<aiondb_core::VectorValue>> {
        for projection in aggregates {
            let TypedExprKind::AggMin { expr, filter: None } = &projection.expr.kind else {
                continue;
            };
            let TypedExprKind::ScalarFunction { func, args } = &expr.kind else {
                continue;
            };
            if !is_hnsw_project_l2_distance_func(func) || args.len() != 2 {
                continue;
            }
            for arg in args {
                if matches!(arg.kind, TypedExprKind::ColumnRef { .. }) {
                    continue;
                }
                if let Value::Vector(vector) = self.evaluator.evaluate(arg)? {
                    return Ok(Some(vector));
                }
            }
        }
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    fn try_execute_hybrid_sql_graph_vector_agg(
        &self,
        context: &ExecutionContext,
        source: &PhysicalPlan,
        group_by: &[TypedExpr],
        grouping_sets: &[Vec<usize>],
        aggregates: &[ProjectionExpr],
        having: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        offset: Option<&TypedExpr>,
        distinct: bool,
        effective_limit: Option<u64>,
    ) -> DbResult<Option<Vec<Row>>> {
        if group_by.len() != 2
            || !grouping_sets.is_empty()
            || aggregates.len() != 5
            || having.is_some()
            || filter.is_some()
            || order_by.len() != 2
            || offset.is_some()
            || distinct
        {
            return Ok(None);
        }

        let mut joins = Vec::new();
        let base = Self::collect_nested_index_join_chain(source, &mut joins);
        if joins.len() != 6 || joins.iter().any(|join| join.join_type != JoinType::Inner) {
            return Ok(None);
        }
        let Some(users_table_id) = Self::plan_base_table_id(base) else {
            return Ok(None);
        };
        if !self.table_name_is(context, users_table_id, "users")?
            || !self.table_name_is(context, joins[0].right_table_id, "posts")?
            || !self.table_name_is(context, joins[1].right_table_id, "follows")?
            || joins[1].right_table_id != joins[2].right_table_id
            || !self.table_name_is(context, joins[3].right_table_id, "wrote")?
            || !self.table_name_is(context, joins[4].right_table_id, "cites")?
            || !self.table_name_is(context, joins[5].right_table_id, "docs")?
        {
            return Ok(None);
        }

        if joins.iter().take(5).any(|join| join.filter.is_some()) {
            return Ok(None);
        }

        let Some(query_vector) = self.literal_vector_from_min_l2_aggregate(aggregates)? else {
            return Ok(None);
        };
        let (user_rows, _) = self.materialize_join_child(base, context)?;
        let user_fields = base.output_fields();
        let user_id_idx = if user_fields.is_empty() {
            joins[0].outer_key_ordinal
        } else {
            let Some(idx) = user_fields
                .get(joins[0].outer_key_ordinal)
                .map(|_| joins[0].outer_key_ordinal)
            else {
                return Ok(None);
            };
            idx
        };
        let user_tenant_idx = if user_fields.is_empty() {
            let Some(idx) = self.table_column_index(context, users_table_id, "tenant_id")? else {
                return Ok(None);
            };
            idx
        } else {
            let Some(idx) = Self::output_field_index(&user_fields, "tenant_id") else {
                return Ok(None);
            };
            idx
        };

        let posts_table_id = joins[0].right_table_id;
        let follows_table_id = joins[1].right_table_id;
        let wrote_table_id = joins[3].right_table_id;
        let cites_table_id = joins[4].right_table_id;
        let docs_table_id = joins[5].right_table_id;
        let Some(posts_likes_idx) = self.table_column_index(context, posts_table_id, "likes")?
        else {
            return Ok(None);
        };
        let Some(edge_target_idx) =
            self.table_column_index(context, follows_table_id, "target_id")?
        else {
            return Ok(None);
        };
        let Some(wrote_target_idx) =
            self.table_column_index(context, wrote_table_id, "target_id")?
        else {
            return Ok(None);
        };
        let Some(cites_target_idx) =
            self.table_column_index(context, cites_table_id, "target_id")?
        else {
            return Ok(None);
        };
        let Some(docs_tenant_idx) = self.table_column_index(context, docs_table_id, "tenant_id")?
        else {
            return Ok(None);
        };
        let Some(docs_embedding_idx) =
            self.table_column_index(context, docs_table_id, "embedding")?
        else {
            return Ok(None);
        };

        let include_posts_oid =
            self.compat_include_oid_system_column_for_table_id(context, posts_table_id)?;
        let include_follows_oid =
            self.compat_include_oid_system_column_for_table_id(context, follows_table_id)?;
        let include_wrote_oid =
            self.compat_include_oid_system_column_for_table_id(context, wrote_table_id)?;
        let include_cites_oid =
            self.compat_include_oid_system_column_for_table_id(context, cites_table_id)?;
        let include_docs_oid =
            self.compat_include_oid_system_column_for_table_id(context, docs_table_id)?;

        let mut rows = Vec::new();
        for user_row in user_rows {
            context.check_deadline()?;
            // Move user_id and user_tenant out of the consumed row instead of
            // cloning them. The two ordinals are distinct (id vs tenant) so
            // each mem::replace targets its own slot.
            let mut row_values = user_row.values;
            let Some(user_id_slot) = row_values.get_mut(user_id_idx) else {
                continue;
            };
            let user_id = std::mem::replace(user_id_slot, Value::Null);
            let Some(user_tenant_slot) = row_values.get_mut(user_tenant_idx) else {
                continue;
            };
            let user_tenant = std::mem::replace(user_tenant_slot, Value::Null);
            let post_rows = self.fetch_join_index_lookup_rows_cached(
                context,
                posts_table_id,
                joins[0].right_index_id,
                &user_id,
                include_posts_oid,
            )?;
            let mut post_count = 0u64;
            let mut max_likes = Value::Null;
            for post_row in post_rows {
                if let Some(right_filter) = joins[0].right_filter {
                    let filter_value =
                        self.evaluate_expr_with_row(right_filter, &post_row, context)?;
                    if !matches!(filter_value, Value::Boolean(true)) {
                        continue;
                    }
                }
                post_count = post_count.saturating_add(1);
                // Move the likes Value out of the consumed row instead of
                // cloning it via `.cloned()`.
                let mut post_values = post_row.values;
                let likes = if posts_likes_idx < post_values.len() {
                    std::mem::replace(&mut post_values[posts_likes_idx], Value::Null)
                } else {
                    Value::Null
                };
                if matches!(max_likes, Value::Null)
                    || compare_runtime_values(&likes, &max_likes)?
                        == Some(std::cmp::Ordering::Greater)
                {
                    max_likes = likes;
                }
            }
            if post_count == 0 {
                continue;
            }

            let mut path_count = 0u64;
            let mut best_dist = f64::INFINITY;
            let first_hop_rows = self.fetch_join_index_lookup_rows_cached(
                context,
                follows_table_id,
                joins[1].right_index_id,
                &user_id,
                include_follows_oid,
            )?;
            for first_hop in first_hop_rows {
                let Some(friend_id) = first_hop.values.get(edge_target_idx) else {
                    continue;
                };
                let second_hop_rows = self.fetch_join_index_lookup_rows_cached(
                    context,
                    follows_table_id,
                    joins[2].right_index_id,
                    friend_id,
                    include_follows_oid,
                )?;
                for second_hop in second_hop_rows {
                    let Some(second_friend_id) = second_hop.values.get(edge_target_idx) else {
                        continue;
                    };
                    let wrote_rows = self.fetch_join_index_lookup_rows_cached(
                        context,
                        wrote_table_id,
                        joins[3].right_index_id,
                        second_friend_id,
                        include_wrote_oid,
                    )?;
                    for wrote_row in wrote_rows {
                        let Some(source_doc_id) = wrote_row.values.get(wrote_target_idx) else {
                            continue;
                        };
                        let cites_rows = self.fetch_join_index_lookup_rows_cached(
                            context,
                            cites_table_id,
                            joins[4].right_index_id,
                            source_doc_id,
                            include_cites_oid,
                        )?;
                        for cites_row in cites_rows {
                            let Some(target_doc_id) = cites_row.values.get(cites_target_idx) else {
                                continue;
                            };
                            let target_rows = self.fetch_join_index_lookup_rows_cached(
                                context,
                                docs_table_id,
                                joins[5].right_index_id,
                                target_doc_id,
                                include_docs_oid,
                            )?;
                            for target_row in target_rows {
                                let Some(target_tenant) = target_row.values.get(docs_tenant_idx)
                                else {
                                    continue;
                                };
                                if compare_runtime_values(target_tenant, &user_tenant)?
                                    != Some(std::cmp::Ordering::Equal)
                                {
                                    continue;
                                }
                                if let Some(right_filter) = joins[5].right_filter {
                                    let filter_value = self.evaluate_expr_with_row(
                                        right_filter,
                                        &target_row,
                                        context,
                                    )?;
                                    if !matches!(filter_value, Value::Boolean(true)) {
                                        continue;
                                    }
                                }
                                let Some(Value::Vector(embedding)) =
                                    target_row.values.get(docs_embedding_idx)
                                else {
                                    continue;
                                };
                                if embedding.values.len() != query_vector.values.len() {
                                    continue;
                                }
                                let distance = aiondb_vector::distance::l2_distance_f64(
                                    &embedding.values,
                                    &query_vector.values,
                                );
                                path_count = path_count.saturating_add(1);
                                if distance < best_dist {
                                    best_dist = distance;
                                }
                            }
                        }
                    }
                }
            }
            if path_count == 0 {
                continue;
            }
            rows.push(Row::new(vec![
                user_id,
                user_tenant,
                Value::BigInt(
                    (post_count.saturating_mul(path_count))
                        .min(i64::MAX.cast_unsigned())
                        .cast_signed(),
                ),
                max_likes,
                Value::Double(best_dist),
            ]));
        }

        rows.sort_by(|left, right| {
            let left_dist = match left.values.get(4) {
                Some(Value::Double(value)) if !value.is_nan() => *value,
                _ => f64::INFINITY,
            };
            let right_dist = match right.values.get(4) {
                Some(Value::Double(value)) if !value.is_nan() => *value,
                _ => f64::INFINITY,
            };
            left_dist.total_cmp(&right_dist).then_with(|| {
                compare_runtime_values(
                    right.values.get(3).unwrap_or(&Value::Null),
                    left.values.get(3).unwrap_or(&Value::Null),
                )
                .ok()
                .flatten()
                .unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        if let Some(limit) = effective_limit {
            rows.truncate(clamp_u64_to_usize(limit, rows.len()));
        }
        Ok(Some(rows))
    }

    fn try_execute_project_table_string_split_fast_path(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
        source: &PhysicalPlan,
        outputs: &[ProjectionExpr],
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        effective_limit: Option<u64>,
        plan_offset: u64,
        distinct: bool,
        distinct_on: &[TypedExpr],
    ) -> DbResult<Option<ExecutionResult>> {
        if filter.is_some()
            || !order_by.is_empty()
            || distinct
            || !distinct_on.is_empty()
            || effective_limit.is_none()
        {
            return Ok(None);
        }
        let Some((id_ordinal, split_ordinal, delimiter)) = fast_string_split_projection(outputs)
        else {
            return Ok(None);
        };
        let PhysicalPlan::ProjectTable {
            table_id,
            outputs: source_outputs,
            filter: source_filter,
            order_by: source_order_by,
            limit: source_limit,
            offset: source_offset,
            distinct: source_distinct,
            distinct_on: source_distinct_on,
            access_path,
        } = source
        else {
            return Ok(None);
        };
        if source_filter.is_some()
            || !source_order_by.is_empty()
            || source_limit.is_some()
            || source_offset.is_some()
            || *source_distinct
            || !source_distinct_on.is_empty()
            || !project_table_outputs_are_identity(source_outputs)
        {
            return Ok(None);
        }
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, *table_id)?
        else {
            return Ok(None);
        };
        if self
            .compile_compat_rls_policies(
                &table,
                super::dml_plans::CompatRlsAction::Select,
                context,
            )?
            .is_some()
        {
            return Ok(None);
        }

        let mut required_ordinals = vec![id_ordinal];
        if split_ordinal != id_ordinal {
            required_ordinals.push(split_ordinal);
        }
        let split_projected = required_ordinals
            .iter()
            .position(|ordinal| *ordinal == split_ordinal)
            .ok_or_else(|| DbError::internal("failed to map split column ordinal"))?;
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, *table_id, &required_ordinals)?
        else {
            return Ok(None);
        };
        let mut stream =
            self.resolve_scan_stream(context, *table_id, access_path, Some(projected_columns))?;
        let total_offset = plan_offset.saturating_add(context.collect_row_offset);
        let limit = effective_limit.unwrap_or(u64::MAX);
        let mut skipped = 0u64;
        let mut produced = 0u64;
        let mut result_bytes = 0u64;
        let mut rows = Vec::with_capacity(clamp_u64_to_usize(limit, 1024));
        let has_interrupts = context.has_execution_interrupts();
        let mut scanned_rows = 0usize;

        while let Some(record) = stream.next()? {
            if has_interrupts && scanned_rows.is_multiple_of(256) {
                context.check_deadline()?;
            }
            scanned_rows = scanned_rows.wrapping_add(1);
            let split_value = record
                .row
                .values
                .get(split_projected)
                .unwrap_or(&Value::Null);
            let Value::Text(text) = split_value else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            // Defer the id clone past the rejection checks; non-text or empty
            // splits no longer pay it.
            let id_value = record.row.values.first().cloned().unwrap_or(Value::Null);

            let mut emit_part = |part: &str| -> DbResult<bool> {
                if skipped < total_offset {
                    skipped = skipped.saturating_add(1);
                    return Ok(false);
                }
                if produced >= limit {
                    return Ok(true);
                }
                if produced >= context.max_result_rows {
                    return Err(DbError::program_limit(
                        "maximum number of result rows reached",
                    ));
                }
                let row = Row::new(vec![id_value.clone(), Value::Text(part.to_owned())]);
                result_bytes =
                    ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
                rows.push(row);
                produced = produced.saturating_add(1);
                Ok(produced >= limit)
            };

            if delimiter.is_empty() {
                if emit_part(text)? {
                    break;
                }
            } else {
                for part in text.split(delimiter.as_str()) {
                    if emit_part(part)? {
                        return Ok(Some(ExecutionResult::Query {
                            columns: plan.output_fields(),
                            rows,
                        }));
                    }
                }
            }
        }

        Ok(Some(ExecutionResult::Query {
            columns: plan.output_fields(),
            rows,
        }))
    }

    pub(super) fn execute_project_source_plan(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        let PhysicalPlan::ProjectSource {
            source,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } = plan
        else {
            return Err(DbError::internal(
                "non-derived projection plan routed to derived projection executor",
            ));
        };

        let plan_limit = limit
            .as_ref()
            .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "LIMIT"))
            .transpose()?;
        let effective_limit = effective_collect_limit(plan_limit, context.collect_row_limit);
        let plan_offset = offset
            .as_ref()
            .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "OFFSET"))
            .transpose()?
            .unwrap_or(0);
        let source_has_side_effects = plan_has_data_modifying_side_effects(source);
        let side_effect_cache_key =
            source_has_side_effects.then(|| side_effect_project_cache_key(plan));
        if let Some(cache_key) = side_effect_cache_key {
            if let Some(cached) = context
                .side_effect_query_cache
                .lock()
                .map_err(|error| {
                    DbError::internal(format!("side-effect query cache poisoned: {error}"))
                })?
                .get(&cache_key)
                .cloned()
            {
                return Ok(cached);
            }
        }
        context.check_deadline()?;
        if !source_has_side_effects && matches!(effective_limit, Some(0)) {
            return Ok(ExecutionResult::Query {
                columns: plan.output_fields(),
                rows: Vec::new(),
            });
        }
        if context.max_result_rows == 0 {
            return Err(DbError::program_limit(
                "maximum number of result rows reached",
            ));
        }
        if !source_has_side_effects {
            if let Some(result) = self.try_execute_project_table_string_split_fast_path(
                plan,
                context,
                source.as_ref(),
                outputs,
                filter.as_ref(),
                order_by,
                effective_limit,
                plan_offset,
                *distinct,
                distinct_on,
            )? {
                return Ok(result);
            }
        }
        let hnsw_result_cache_key = (!source_has_side_effects)
            .then(|| {
                hnsw_project_source_cache_key(
                    source.as_ref(),
                    outputs,
                    filter.as_ref(),
                    order_by,
                    *distinct,
                    distinct_on,
                    effective_limit,
                    plan_offset,
                    context,
                )
            })
            .flatten();
        let hnsw_result_cache_generation = hnsw_result_cache_key
            .as_ref()
            .and_then(|_| self.storage_dml.cache_generation());
        if let (Some(cache_key), Some(generation)) =
            (&hnsw_result_cache_key, hnsw_result_cache_generation)
        {
            if let Some((cached_generation, cached_result)) = self
                .hnsw_project_source_result_cache
                .read()
                .map_err(|error| DbError::internal(format!("HNSW result cache poisoned: {error}")))?
                .get(cache_key)
                .cloned()
            {
                if cached_generation == generation {
                    return Ok(cached_result);
                }
            }
        }
        if filter.is_none()
            && order_by.is_empty()
            && !*distinct
            && distinct_on.is_empty()
            && is_single_column_passthrough(outputs)
        {
            if let PhysicalPlan::HybridFunctionScan {
                function_name,
                args,
                ..
            } = source.as_ref()
            {
                if let Some(ExecutionResult::Query { mut rows, .. }) = self
                    .try_fast_graph_neighbors_hybrid_scan(
                        function_name,
                        args,
                        &plan.output_fields(),
                        context,
                    )?
                {
                    if usize_to_u64(rows.len()) > context.max_result_rows {
                        return Err(DbError::program_limit(
                            "maximum number of result rows reached",
                        ));
                    }
                    let total_offset = plan_offset.saturating_add(context.collect_row_offset);
                    if total_offset > 0 {
                        let skip = clamp_u64_to_usize(total_offset, rows.len());
                        rows.drain(..skip);
                    }
                    if let Some(limit) = effective_limit {
                        rows.truncate(clamp_u64_to_usize(limit, rows.len()));
                    }
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows,
                    });
                }
            }
        }

        let mut source_context = context.clone();
        if source_has_side_effects {
            // CTE data-modifying sources must run to completion even when the
            // outer projection is later truncated by LIMIT/OFFSET.
            source_context.collect_row_limit = None;
            source_context.collect_row_offset = 0;
        }
        let pushed_project_table_source;
        let source_for_execute = if filter.is_none()
            && !order_by.is_empty()
            && limit.is_some()
            && offset.is_none()
            && !*distinct
            && distinct_on.is_empty()
            && !source_has_side_effects
        {
            match source.as_ref() {
                PhysicalPlan::ProjectTable {
                    table_id,
                    outputs: child_outputs,
                    filter: child_filter,
                    order_by: child_order_by,
                    limit: child_limit,
                    offset: child_offset,
                    distinct: child_distinct,
                    distinct_on: child_distinct_on,
                    access_path,
                } if child_order_by.is_empty()
                    && child_limit.is_none()
                    && child_offset.is_none()
                    && !*child_distinct
                    && child_distinct_on.is_empty() =>
                {
                    if let Some(pushed_order_by) =
                        remap_project_source_order_by_for_project_table_pushdown(
                            order_by,
                            child_outputs,
                        )
                    {
                        pushed_project_table_source = PhysicalPlan::ProjectTable {
                            table_id: *table_id,
                            outputs: child_outputs.clone(),
                            filter: child_filter.clone(),
                            order_by: pushed_order_by,
                            limit: limit.clone(),
                            offset: None,
                            distinct: false,
                            distinct_on: Vec::new(),
                            access_path: access_path.clone(),
                        };
                        &pushed_project_table_source
                    } else {
                        source.as_ref()
                    }
                }
                _ => source.as_ref(),
            }
        } else {
            source.as_ref()
        };
        let filter_requires_special_resolution = filter
            .as_ref()
            .is_some_and(super::projection_plans::expr_requires_special_resolution);
        let adaptive_target_rows = effective_limit.map(|limit_rows| {
            limit_rows
                .saturating_add(plan_offset)
                .saturating_add(context.collect_row_offset)
        });
        let source_rows = if let Some(rows) = self.execute_hnsw_source_with_adaptive_widening(
            source_for_execute,
            filter.as_ref(),
            filter_requires_special_resolution,
            adaptive_target_rows,
            &source_context,
            context,
        )? {
            rows
        } else {
            let ExecutionResult::Query {
                rows: source_rows, ..
            } = self.execute(source_for_execute, &source_context)?
            else {
                return Err(DbError::internal(
                    "derived projection source did not return query rows",
                ));
            };
            source_rows
        };

        let has_windows = window_eval::has_window_functions(outputs);
        if has_windows {
            let mut filtered_rows = Vec::new();
            for row in source_rows {
                context.check_deadline()?;
                if !predicate_matches(filter.as_ref().map(|expr| {
                    self.evaluate_expr_with_row_prechecked(
                        expr,
                        &row,
                        context,
                        filter_requires_special_resolution,
                    )
                }))? {
                    continue;
                }
                if usize_to_u64(filtered_rows.len()) >= context.max_result_rows {
                    return Err(DbError::program_limit(
                        "maximum number of result rows reached",
                    ));
                }
                filtered_rows.push(row);
            }

            let mut rows = window_eval::evaluate_windows(self, outputs, &filtered_rows, context)?;
            if !order_by.is_empty() {
                sort_rows_by_order(self, &mut rows, order_by, outputs, context)?;
            }
            if *distinct {
                deduplicate_rows(&mut rows, context)?;
            }
            if !distinct_on.is_empty() {
                let rebased_distinct_on =
                    super::projection_plans::rebase_distinct_on_to_output_ordinals(
                        outputs,
                        distinct_on,
                    );
                apply_distinct_on_rows(self, &mut rows, &rebased_distinct_on, context)?;
            }

            let total_offset = plan_offset.saturating_add(context.collect_row_offset);
            if total_offset > 0 {
                let skip = clamp_u64_to_usize(total_offset, rows.len());
                rows.drain(..skip);
            }
            if let Some(limit) = effective_limit {
                rows.truncate(clamp_u64_to_usize(limit, rows.len()));
            }

            let result = ExecutionResult::Query {
                columns: plan.output_fields(),
                rows,
            };
            if let Some(cache_key) = side_effect_cache_key {
                context
                    .side_effect_query_cache
                    .lock()
                    .map_err(|error| {
                        DbError::internal(format!("side-effect query cache poisoned: {error}"))
                    })?
                    .insert(cache_key, result.clone());
            }
            return Ok(result);
        }

        let mut sorted_rows = Vec::with_capacity(source_rows.len());
        let mut result_bytes = 0u64;
        let has_ordering = !order_by.is_empty();
        let output_requires_special_resolution: Vec<bool> = outputs
            .iter()
            .map(|output| super::projection_plans::expr_requires_special_resolution(&output.expr))
            .collect();
        let order_by_requires_special_resolution: Vec<bool> = order_by
            .iter()
            .map(|sort| super::projection_plans::expr_requires_special_resolution(&sort.expr))
            .collect();
        let srf_output_indices: Vec<usize> = outputs
            .iter()
            .enumerate()
            .filter_map(|(index, output)| {
                super::projection_plans::is_srf_output(&output.expr).then_some(index)
            })
            .collect();
        let empty_sort_keys = std::sync::Arc::new(Vec::new());

        // Top-K streaming fast path. When we have ORDER BY <expr> LIMIT K
        // with a small K and a much larger source row count (typical for
        // `SELECT ... FROM t ORDER BY embedding <-> $q LIMIT 10`), we
        // can keep a sorted Vec of size K and skip projection +
        // bookkeeping for the (N-K) rows that can never make it into
        // the final result.
        let total_offset_u64 = plan_offset.saturating_add(context.collect_row_offset);
        let total_offset_usize = clamp_u64_to_usize(total_offset_u64, usize::MAX);
        let top_k_bound = effective_limit
            .map(|lim| clamp_u64_to_usize(lim, usize::MAX).saturating_add(total_offset_usize));
        let top_k_eligible = has_ordering
            && !*distinct
            && distinct_on.is_empty()
            && srf_output_indices.is_empty()
            && !source_has_side_effects
            // K up to 4_096 covers `OFFSET <large> LIMIT 20` pagination
            // shapes; lower than that and full sort wins on the wasted
            // heap pushes once K approaches source_rows.len().
            && top_k_bound.is_some_and(|k| {
                k > 0 && k <= 4_096 && source_rows.len() >= k.saturating_mul(4)
            });
        if top_k_eligible {
            let Some(bound) = top_k_bound else {
                return Err(DbError::internal(
                    "top-k ORDER BY eligibility missing bound",
                ));
            };
            // Scalar ORDER BY fast path: avoid allocating `Vec<Value>` + `Arc`
            // sort-key containers for every scanned row when there is only
            // one sort expression (e.g. `ORDER BY likes DESC LIMIT 20`).
            if order_by.len() == 1 {
                struct ScalarTopRow {
                    row: Row,
                    key: Value,
                }

                let sort = &order_by[0];
                let mut top: Vec<ScalarTopRow> = Vec::with_capacity(bound);
                for row in source_rows {
                    context.check_deadline()?;
                    if !predicate_matches(filter.as_ref().map(|expr| {
                        self.evaluate_expr_with_row_prechecked(
                            expr,
                            &row,
                            context,
                            filter_requires_special_resolution,
                        )
                    }))? {
                        continue;
                    }
                    let sort_key = self.evaluate_expr_with_row_prechecked(
                        &sort.expr,
                        &row,
                        context,
                        order_by_requires_special_resolution[0],
                    )?;
                    if top.len() == bound {
                        let worst = &top[bound - 1];
                        let cmp = compare_sort_values(
                            &sort_key,
                            &worst.key,
                            sort.descending,
                            sort.nulls_first,
                        )?;
                        if matches!(cmp, Ordering::Greater | Ordering::Equal) {
                            continue;
                        }
                    }

                    let mut projected = Vec::with_capacity(outputs.len());
                    for (output, requires_special_resolution) in outputs
                        .iter()
                        .zip(output_requires_special_resolution.iter().copied())
                    {
                        projected.push(self.evaluate_expr_with_row_prechecked(
                            &output.expr,
                            &row,
                            context,
                            requires_special_resolution,
                        )?);
                    }
                    let new_entry = ScalarTopRow {
                        row: Row::new(projected),
                        key: sort_key,
                    };
                    let pos = {
                        let mut lo = 0usize;
                        let mut hi = top.len();
                        while lo < hi {
                            let mid = lo + (hi - lo) / 2;
                            let ord = compare_sort_values(
                                &top[mid].key,
                                &new_entry.key,
                                sort.descending,
                                sort.nulls_first,
                            )?;
                            if matches!(ord, Ordering::Less) {
                                lo = mid + 1;
                            } else {
                                hi = mid;
                            }
                        }
                        lo
                    };
                    if top.len() == bound {
                        top.pop();
                    }
                    top.insert(pos, new_entry);
                }

                for entry in &top {
                    context.track_memory(estimate_value_bytes(&entry.key))?;
                    result_bytes = ensure_result_bytes_fit_and_track_query_row(
                        context,
                        &entry.row,
                        result_bytes,
                    )?;
                }
                let mut rows: Vec<Row> = top.into_iter().map(|e| e.row).collect();
                if total_offset_u64 > 0 {
                    let skip = clamp_u64_to_usize(total_offset_u64, rows.len());
                    rows.drain(..skip);
                }
                if let Some(limit) = effective_limit {
                    rows.truncate(clamp_u64_to_usize(limit, rows.len()));
                }
                let result = ExecutionResult::Query {
                    columns: plan.output_fields(),
                    rows,
                };
                if let Some(cache_key) = side_effect_cache_key {
                    context
                        .side_effect_query_cache
                        .lock()
                        .map_err(|error| {
                            DbError::internal(format!("side-effect query cache poisoned: {error}"))
                        })?
                        .insert(cache_key, result.clone());
                }
                return Ok(result);
            }
            let mut top: Vec<SortedQueryRow> = Vec::with_capacity(bound);
            for row in source_rows {
                context.check_deadline()?;
                if !predicate_matches(filter.as_ref().map(|expr| {
                    self.evaluate_expr_with_row_prechecked(
                        expr,
                        &row,
                        context,
                        filter_requires_special_resolution,
                    )
                }))? {
                    continue;
                }
                let sort_keys = std::sync::Arc::new(
                    order_by
                        .iter()
                        .zip(order_by_requires_special_resolution.iter().copied())
                        .map(|(sort, requires_special_resolution)| {
                            self.evaluate_expr_with_row_prechecked(
                                &sort.expr,
                                &row,
                                context,
                                requires_special_resolution,
                            )
                        })
                        .collect::<DbResult<Vec<_>>>()?,
                );
                if top.len() == bound {
                    let worst = &top[bound - 1];
                    let cmp = compare_sort_values_vec(&sort_keys, &worst.sort_keys, order_by)?;
                    if matches!(cmp, Ordering::Greater | Ordering::Equal) {
                        continue;
                    }
                }
                let mut projected = Vec::with_capacity(outputs.len());
                for (output, requires_special_resolution) in outputs
                    .iter()
                    .zip(output_requires_special_resolution.iter().copied())
                {
                    projected.push(self.evaluate_expr_with_row_prechecked(
                        &output.expr,
                        &row,
                        context,
                        requires_special_resolution,
                    )?);
                }
                let new_entry = SortedQueryRow {
                    row: Row::new(projected),
                    sort_keys,
                };
                let pos = {
                    let mut lo = 0usize;
                    let mut hi = top.len();
                    while lo < hi {
                        let mid = lo + (hi - lo) / 2;
                        let ord = compare_sort_values_vec(
                            &top[mid].sort_keys,
                            &new_entry.sort_keys,
                            order_by,
                        )?;
                        if matches!(ord, Ordering::Less) {
                            lo = mid + 1;
                        } else {
                            hi = mid;
                        }
                    }
                    lo
                };
                if top.len() == bound {
                    top.pop();
                }
                top.insert(pos, new_entry);
            }
            for entry in &top {
                context.track_memory(
                    entry
                        .sort_keys
                        .iter()
                        .map(estimate_value_bytes)
                        .fold(0u64, u64::saturating_add),
                )?;
                result_bytes =
                    ensure_result_bytes_fit_and_track_query_row(context, &entry.row, result_bytes)?;
            }
            let mut rows: Vec<Row> = top.into_iter().map(|e| e.row).collect();
            if total_offset_u64 > 0 {
                let skip = clamp_u64_to_usize(total_offset_u64, rows.len());
                rows.drain(..skip);
            }
            if let Some(limit) = effective_limit {
                rows.truncate(clamp_u64_to_usize(limit, rows.len()));
            }
            let result = ExecutionResult::Query {
                columns: plan.output_fields(),
                rows,
            };
            if let Some(cache_key) = side_effect_cache_key {
                context
                    .side_effect_query_cache
                    .lock()
                    .map_err(|error| {
                        DbError::internal(format!("side-effect query cache poisoned: {error}"))
                    })?
                    .insert(cache_key, result.clone());
            }
            return Ok(result);
        }

        for row in source_rows {
            context.check_deadline()?;
            if !predicate_matches(filter.as_ref().map(|expr| {
                self.evaluate_expr_with_row_prechecked(
                    expr,
                    &row,
                    context,
                    filter_requires_special_resolution,
                )
            }))? {
                continue;
            }
            if usize_to_u64(sorted_rows.len()) >= context.max_result_rows {
                return Err(DbError::program_limit(
                    "maximum number of result rows reached",
                ));
            }

            let mut projected = Vec::with_capacity(outputs.len());
            for (output, requires_special_resolution) in outputs
                .iter()
                .zip(output_requires_special_resolution.iter().copied())
            {
                projected.push(self.evaluate_expr_with_row_prechecked(
                    &output.expr,
                    &row,
                    context,
                    requires_special_resolution,
                )?);
            }
            let sort_keys = if has_ordering {
                std::sync::Arc::new(
                    order_by
                        .iter()
                        .zip(order_by_requires_special_resolution.iter().copied())
                        .map(|(sort, requires_special_resolution)| {
                            self.evaluate_expr_with_row_prechecked(
                                &sort.expr,
                                &row,
                                context,
                                requires_special_resolution,
                            )
                        })
                        .collect::<DbResult<Vec<_>>>()?,
                )
            } else {
                std::sync::Arc::clone(&empty_sort_keys)
            };
            let srf_indices: Vec<usize> = srf_output_indices
                .iter()
                .copied()
                .filter(|&index| {
                    matches!(projected.get(index), Some(Value::Array(_)))
                        && (super::projection_plans::is_srf_output(&outputs[index].expr)
                            || !matches!(outputs[index].field.data_type, DataType::Array(_)))
                })
                .collect();
            let expanded_rows = if srf_indices.is_empty() {
                vec![Row::new(projected)]
            } else {
                super::projection_plans::expand_srf_rows(projected, &srf_indices)
            };
            if has_ordering {
                context.track_memory(
                    sort_keys
                        .iter()
                        .map(estimate_value_bytes)
                        .fold(0u64, u64::saturating_add),
                )?;
            }
            for projected in expanded_rows {
                push_sorted_query_row(
                    &mut sorted_rows,
                    context,
                    projected,
                    std::sync::Arc::clone(&sort_keys),
                    &mut result_bytes,
                )?;
            }
        }

        if has_ordering {
            sort_query_rows(&mut sorted_rows, order_by, context)?;
        }

        let mut rows = sorted_rows
            .into_iter()
            .map(|entry| entry.row)
            .collect::<Vec<_>>();
        if *distinct {
            deduplicate_rows(&mut rows, context)?;
        }
        if !distinct_on.is_empty() {
            let rebased_distinct_on =
                super::projection_plans::rebase_distinct_on_to_output_ordinals(
                    outputs,
                    distinct_on,
                );
            apply_distinct_on_rows(self, &mut rows, &rebased_distinct_on, context)?;
        }

        let total_offset = plan_offset.saturating_add(context.collect_row_offset);
        if total_offset > 0 {
            let skip = clamp_u64_to_usize(total_offset, rows.len());
            rows.drain(..skip);
        }
        if let Some(limit) = effective_limit {
            rows.truncate(clamp_u64_to_usize(limit, rows.len()));
        }

        let result = ExecutionResult::Query {
            columns: plan.output_fields(),
            rows,
        };
        if let (Some(cache_key), Some(generation)) =
            (hnsw_result_cache_key, hnsw_result_cache_generation)
        {
            let mut cache = self
                .hnsw_project_source_result_cache
                .write()
                .map_err(|error| {
                    DbError::internal(format!("HNSW result cache poisoned: {error}"))
                })?;
            if cache.len() >= 1024 {
                cache.clear();
            }
            cache.insert(cache_key, (generation, result.clone()));
        }
        Ok(result)
    }

    fn execute_hnsw_source_with_adaptive_widening(
        &self,
        source: &PhysicalPlan,
        filter: Option<&TypedExpr>,
        filter_requires_special_resolution: bool,
        target_filtered_rows: Option<u64>,
        source_context: &ExecutionContext,
        eval_context: &ExecutionContext,
    ) -> DbResult<Option<Vec<Row>>> {
        let Some(target_rows) = target_filtered_rows else {
            return Ok(None);
        };
        let target_rows = clamp_u64_to_usize(target_rows, ADAPTIVE_HNSW_SOURCE_LIMIT_CAP);
        if target_rows == 0 {
            return Ok(None);
        }
        let PhysicalPlan::HnswScan {
            table_id,
            index_id,
            query_vector,
            limit,
            ef_search,
            projected_ordinals,
            output_fields,
        } = source
        else {
            return Ok(None);
        };

        let scan_limit_cap =
            super::special_exprs::pgvector_hnsw_max_scan_tuples_setting(eval_context)?
                .unwrap_or(ADAPTIVE_HNSW_SOURCE_LIMIT_CAP)
                .clamp(1, ADAPTIVE_HNSW_SOURCE_LIMIT_CAP);
        let mut scan_limit = (*limit).min(scan_limit_cap);
        let mut scan_ef_search = (*ef_search)
            .min(HNSW_MAX_EF_SEARCH)
            .max(bounded_hnsw_ef_search(scan_limit));
        loop {
            let scan_plan = PhysicalPlan::HnswScan {
                table_id: *table_id,
                index_id: *index_id,
                query_vector: query_vector.clone(),
                limit: scan_limit,
                ef_search: scan_ef_search,
                projected_ordinals: projected_ordinals.clone(),
                output_fields: output_fields.clone(),
            };
            let ExecutionResult::Query { rows, .. } = self.execute(&scan_plan, source_context)?
            else {
                return Err(DbError::internal(
                    "derived projection source did not return query rows",
                ));
            };

            let matched_rows = if let Some(predicate) = filter {
                let mut matched_rows = 0usize;
                for row in &rows {
                    eval_context.check_deadline()?;
                    let filter_value = self.evaluate_expr_with_row_prechecked(
                        predicate,
                        row,
                        eval_context,
                        filter_requires_special_resolution,
                    )?;
                    if predicate_matches(Some(Ok(filter_value)))? {
                        matched_rows = matched_rows.saturating_add(1);
                        if matched_rows >= target_rows {
                            break;
                        }
                    }
                }
                matched_rows
            } else {
                rows.len().min(target_rows)
            };
            tracing::debug!(
                target: "aiondb_executor::vector",
                ?index_id,
                scan_limit,
                scan_ef_search,
                fetched_rows = rows.len(),
                matched_rows,
                target_rows,
                has_filter = filter.is_some(),
                "adaptive hnsw widening iteration"
            );
            if matched_rows >= target_rows
                || scan_limit >= scan_limit_cap
                || rows.len() < scan_limit
            {
                return Ok(Some(rows));
            }

            let next_limit =
                next_adaptive_hnsw_limit(scan_limit, matched_rows, target_rows).min(scan_limit_cap);
            if next_limit <= scan_limit {
                return Ok(Some(rows));
            }
            scan_limit = next_limit;
            scan_ef_search = scan_ef_search
                .max(bounded_hnsw_ef_search(scan_limit))
                .min(HNSW_MAX_EF_SEARCH);
        }
    }

    fn try_execute_project_table_aggregate_source_fast_path(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
        source: &PhysicalPlan,
        group_by: &[TypedExpr],
        grouping_sets: &[Vec<usize>],
        aggregates: &[ProjectionExpr],
        having: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        limit: Option<&TypedExpr>,
        offset: Option<&TypedExpr>,
        distinct: bool,
    ) -> DbResult<Option<ExecutionResult>> {
        if filter.is_some() || !grouping_sets.is_empty() || having.is_some() || distinct {
            return Ok(None);
        }
        let PhysicalPlan::ProjectTable {
            table_id,
            outputs: source_outputs,
            filter: source_filter,
            order_by: source_order_by,
            limit: source_limit,
            offset: source_offset,
            distinct: source_distinct,
            distinct_on: source_distinct_on,
            access_path,
        } = source
        else {
            return Ok(None);
        };
        if !source_order_by.is_empty()
            || source_limit.is_some()
            || source_offset.is_some()
            || *source_distinct
            || !source_distinct_on.is_empty()
            || !project_table_outputs_are_identity(source_outputs)
        {
            return Ok(None);
        }
        let Some(simple_filter) = extract_derived_simple_filter(source_filter.as_ref()) else {
            return Ok(None);
        };
        let order_column_indices: Option<Vec<Option<usize>>> = order_by
            .iter()
            .map(|sort| match &sort.expr.kind {
                TypedExprKind::ColumnRef { ordinal, .. } if *ordinal < aggregates.len() => {
                    Some(Some(*ordinal))
                }
                _ => aggregates
                    .iter()
                    .position(|projection| exprs_structurally_equal(&projection.expr, &sort.expr))
                    .map(Some),
            })
            .collect();
        let Some(order_column_indices) = order_column_indices else {
            return Ok(None);
        };
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, *table_id)?
        else {
            return Ok(None);
        };
        if self
            .compile_compat_rls_policies(
                &table,
                super::dml_plans::CompatRlsAction::Select,
                context,
            )?
            .is_some()
        {
            return Ok(None);
        }

        let mut group_ordinals = Vec::with_capacity(group_by.len());
        for expr in group_by {
            let Some(ordinal) = derived_simple_column_ordinal(expr) else {
                return Ok(None);
            };
            group_ordinals.push(ordinal);
        }

        let mut required_ordinals = group_ordinals.clone();
        let add_required_ordinal = |ordinals: &mut Vec<usize>, ordinal: usize| {
            if !ordinals.contains(&ordinal) {
                ordinals.push(ordinal);
            }
        };
        if let Some(filter) = &simple_filter {
            add_required_ordinal(&mut required_ordinals, filter.column_ordinal);
        }

        let mut output_plan = Vec::with_capacity(aggregates.len());
        for projection in aggregates {
            match &projection.expr.kind {
                TypedExprKind::ColumnRef { ordinal, .. } => {
                    let Some(group_index) = group_ordinals
                        .iter()
                        .position(|group_ordinal| group_ordinal == ordinal)
                    else {
                        return Ok(None);
                    };
                    output_plan.push(DerivedSimpleAggOutput::GroupKey { group_index });
                }
                TypedExprKind::AggCount {
                    expr: None,
                    distinct: false,
                    filter: None,
                } => output_plan.push(DerivedSimpleAggOutput::CountStar),
                TypedExprKind::AggSum {
                    expr,
                    distinct: false,
                    filter: None,
                } => {
                    let Some(ordinal) = derived_simple_column_ordinal(expr) else {
                        return Ok(None);
                    };
                    add_required_ordinal(&mut required_ordinals, ordinal);
                    let Some(projected_pos) =
                        derived_projected_position(&required_ordinals, ordinal)
                    else {
                        return Ok(None);
                    };
                    output_plan.push(DerivedSimpleAggOutput::Sum { projected_pos });
                }
                TypedExprKind::AggAvg {
                    expr,
                    distinct: false,
                    filter: None,
                } => {
                    let Some(ordinal) = derived_simple_column_ordinal(expr) else {
                        return Ok(None);
                    };
                    add_required_ordinal(&mut required_ordinals, ordinal);
                    let Some(projected_pos) =
                        derived_projected_position(&required_ordinals, ordinal)
                    else {
                        return Ok(None);
                    };
                    output_plan.push(DerivedSimpleAggOutput::Avg { projected_pos });
                }
                _ => return Ok(None),
            }
        }

        if group_by.is_empty() && aggregate_source_count_star_outputs(aggregates) {
            let count = self.count_project_table_rows_for_simple_aggregate_source(
                context,
                *table_id,
                simple_filter.as_ref(),
                access_path,
                &required_ordinals,
            )?;
            let offset_val = offset
                .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "OFFSET"))
                .transpose()?
                .unwrap_or(0);
            if offset_val > 0 {
                return Ok(Some(ExecutionResult::Query {
                    columns: plan.output_fields(),
                    rows: Vec::new(),
                }));
            }
            let row = Row::new(vec![
                Value::BigInt(i64::try_from(count).unwrap_or(i64::MAX));
                aggregates.len()
            ]);
            ensure_result_bytes_fit_and_track_query_row(context, &row, 0)?;
            return Ok(Some(ExecutionResult::Query {
                columns: plan.output_fields(),
                rows: vec![row],
            }));
        }
        if group_by.is_empty() {
            return Ok(None);
        }

        let group_positions = group_ordinals
            .iter()
            .map(|ordinal| {
                derived_projected_position(&required_ordinals, *ordinal).ok_or_else(|| {
                    DbError::internal("failed to map AggregateSource GROUP BY ordinal")
                })
            })
            .collect::<DbResult<Vec<_>>>()?;
        let filter_position = simple_filter
            .as_ref()
            .map(|filter| {
                derived_projected_position(&required_ordinals, filter.column_ordinal).ok_or_else(
                    || DbError::internal("failed to map AggregateSource filter ordinal"),
                )
            })
            .transpose()?;
        let projected_column_ids = self
            .table_column_ids_for_ordinals(context, *table_id, &required_ordinals)?
            .ok_or_else(|| DbError::internal("failed to map AggregateSource projection columns"))?;
        let mut stream =
            self.resolve_scan_stream(context, *table_id, access_path, Some(projected_column_ids))?;

        let mut groups: std::collections::HashMap<Vec<ValueHashKey>, usize> =
            std::collections::HashMap::new();
        let mut ordered_groups: Vec<DerivedSimpleAggState> = Vec::new();
        let mut group_key_scratch = Vec::with_capacity(group_positions.len());
        let output_count = output_plan.len();
        let has_interrupts = context.has_execution_interrupts();
        let mut scanned_rows = 0usize;
        while let Some(record) = stream.next()? {
            if has_interrupts && scanned_rows.is_multiple_of(256) {
                context.check_deadline()?;
            }
            scanned_rows = scanned_rows.wrapping_add(1);
            if let (Some(filter), Some(filter_position)) = (&simple_filter, filter_position) {
                let value = record
                    .row
                    .values
                    .get(filter_position)
                    .unwrap_or(&Value::Null);
                if !derived_simple_filter_matches(value, filter)? {
                    continue;
                }
            }

            group_key_scratch.clear();
            // First pass: build the hash key from borrows only. We only
            // materialise `group_values` (one Value clone per group column)
            // when the key is new — existing groups pay the lookup but skip
            // the Vec build entirely.
            for position in &group_positions {
                let value = record.row.values.get(*position).unwrap_or(&Value::Null);
                group_key_scratch.push(build_hash_key(value)?);
            }
            let group_idx = if let Some(&idx) = groups.get(&group_key_scratch) {
                idx
            } else {
                context.track_memory(64)?;
                let group_idx = ordered_groups.len();
                let mut group_values = Vec::with_capacity(group_positions.len());
                for position in &group_positions {
                    group_values.push(
                        record
                            .row
                            .values
                            .get(*position)
                            .cloned()
                            .unwrap_or(Value::Null),
                    );
                }
                ordered_groups.push(DerivedSimpleAggState::new(group_values, output_count));
                // Move the freshly built key into the HashMap rather than cloning
                // it. Next iteration starts by `clear()`ing scratch, so an empty
                // Vec with the same capacity is the right starting state.
                let key_capacity = group_key_scratch.capacity();
                let key =
                    std::mem::replace(&mut group_key_scratch, Vec::with_capacity(key_capacity));
                groups.insert(key, group_idx);
                group_idx
            };
            let group = ordered_groups
                .get_mut(group_idx)
                .ok_or_else(|| DbError::internal("missing AggregateSource simple group state"))?;
            for (output_idx, output) in output_plan.iter().enumerate() {
                match *output {
                    DerivedSimpleAggOutput::GroupKey { .. } => {}
                    DerivedSimpleAggOutput::CountStar => {
                        group.counts[output_idx] = group.counts[output_idx].saturating_add(1);
                    }
                    DerivedSimpleAggOutput::Sum { projected_pos }
                    | DerivedSimpleAggOutput::Avg { projected_pos } => {
                        let value = record.row.values.get(projected_pos).unwrap_or(&Value::Null);
                        if !value.is_null() {
                            group.counts[output_idx] = group.counts[output_idx].saturating_add(1);
                            group.sums[output_idx] =
                                Some(agg_add_value(group.sums[output_idx].take(), value)?);
                        }
                    }
                }
            }
        }

        let agg_templates: Vec<AggTemplate> = aggregates
            .iter()
            .map(|projection| classify_agg_expr(&projection.expr))
            .collect();
        let mut rows = Vec::with_capacity(ordered_groups.len());
        // Consume `ordered_groups` so we can move the per-group sum out of
        // `group.sums[i]` instead of cloning it before handing it to the
        // finalizer (each `output_idx` reads its slot exactly once, so a take
        // is safe).
        for mut group in ordered_groups {
            context.check_deadline()?;
            let mut values = Vec::with_capacity(output_plan.len());
            for (output_idx, output) in output_plan.iter().enumerate() {
                let value = match *output {
                    DerivedSimpleAggOutput::GroupKey { group_index } => group
                        .group_values
                        .get(group_index)
                        .cloned()
                        .unwrap_or(Value::Null),
                    DerivedSimpleAggOutput::CountStar => Value::BigInt(group.counts[output_idx]),
                    DerivedSimpleAggOutput::Sum { .. } | DerivedSimpleAggOutput::Avg { .. } => {
                        let mut acc = AggAccumulator::new(false);
                        acc.count = group.counts[output_idx];
                        acc.sum = std::mem::take(&mut group.sums[output_idx]);
                        finalize_accumulator(
                            &acc,
                            &agg_templates[output_idx],
                            &self.evaluator,
                            context,
                        )?
                    }
                };
                values.push(value);
            }
            rows.push(Row::new(values));
        }

        if !order_by.is_empty() {
            sort_rows_by_exprs(
                &mut rows,
                order_by,
                &self.evaluator,
                Some(&order_column_indices),
                context,
            )?;
        }

        let offset_val = offset
            .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "OFFSET"))
            .transpose()?
            .unwrap_or(0);
        let total_offset = offset_val.saturating_add(context.collect_row_offset);
        if total_offset > 0 {
            let skip = clamp_u64_to_usize(total_offset, rows.len());
            rows.drain(..skip);
        }
        let plan_limit = limit
            .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "LIMIT"))
            .transpose()?;
        if let Some(limit) = effective_collect_limit(plan_limit, context.collect_row_limit) {
            rows.truncate(clamp_u64_to_usize(limit, rows.len()));
        }
        if usize_to_u64(rows.len()) > context.max_result_rows {
            return Err(DbError::program_limit(
                "maximum number of result rows reached",
            ));
        }
        let mut result_bytes = 0u64;
        for row in &rows {
            result_bytes = ensure_result_bytes_fit_and_track_query_row(context, row, result_bytes)?;
        }
        Ok(Some(ExecutionResult::Query {
            columns: plan.output_fields(),
            rows,
        }))
    }

    fn count_project_table_rows_for_simple_aggregate_source(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        filter: Option<&DerivedSimpleFilter>,
        access_path: &ScanAccessPath,
        required_ordinals: &[usize],
    ) -> DbResult<u64> {
        let Some(filter) = filter else {
            return self
                .storage_dml
                .visible_row_count(context.txn_id, &context.snapshot, table_id);
        };
        let Some(filter_position) =
            derived_projected_position(required_ordinals, filter.column_ordinal)
        else {
            return Err(DbError::internal(
                "failed to map AggregateSource count filter ordinal",
            ));
        };
        let projected_column_ids = self
            .table_column_ids_for_ordinals(context, table_id, required_ordinals)?
            .ok_or_else(|| {
                DbError::internal("failed to map AggregateSource count projection columns")
            })?;
        let mut stream =
            self.resolve_scan_stream(context, table_id, access_path, Some(projected_column_ids))?;
        let has_interrupts = context.has_execution_interrupts();
        let mut scanned_rows = 0usize;
        let mut count = 0u64;
        while let Some(record) = stream.next()? {
            if has_interrupts && scanned_rows.is_multiple_of(256) {
                context.check_deadline()?;
            }
            scanned_rows = scanned_rows.wrapping_add(1);
            let value = record
                .row
                .values
                .get(filter_position)
                .unwrap_or(&Value::Null);
            if derived_simple_filter_matches(value, filter)? {
                count = count.saturating_add(1);
            }
        }
        Ok(count)
    }

    fn try_execute_distributed_scan_aggregate_source_fast_path(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
        source: &PhysicalPlan,
        group_by: &[TypedExpr],
        grouping_sets: &[Vec<usize>],
        aggregates: &[ProjectionExpr],
        having: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        limit: Option<&TypedExpr>,
        offset: Option<&TypedExpr>,
        distinct: bool,
    ) -> DbResult<Option<ExecutionResult>> {
        if group_by.is_empty() || !grouping_sets.is_empty() || filter.is_some() || distinct {
            return Ok(None);
        }
        let PhysicalPlan::DistributedScan {
            table_id,
            outputs,
            filter: source_filter,
            node_count,
            ..
        } = source
        else {
            return Ok(None);
        };
        if !project_table_outputs_are_identity(outputs) {
            return Ok(None);
        }
        if context.distributed_loopback_remote_nodes.is_empty() {
            let local_source = PhysicalPlan::ProjectTable {
                table_id: *table_id,
                outputs: outputs.clone(),
                filter: source_filter.clone(),
                order_by: Vec::new(),
                limit: None,
                offset: None,
                distinct: false,
                distinct_on: Vec::new(),
                access_path: ScanAccessPath::SeqScan,
            };
            return self.try_execute_project_table_aggregate_source_fast_path(
                plan,
                context,
                &local_source,
                group_by,
                grouping_sets,
                aggregates,
                having,
                filter,
                order_by,
                limit,
                offset,
                distinct,
            );
        }

        let aggregate_exprs: Vec<aiondb_plan::AggregateExpr> = aggregates
            .iter()
            .map(|projection| aiondb_plan::AggregateExpr {
                name: projection.field.name.clone(),
            })
            .collect();
        let output_fields: Vec<aiondb_plan::ResultField> = aggregates
            .iter()
            .map(|projection| projection.field.clone())
            .collect();
        let partials: Vec<PhysicalPlan> = (0..*node_count)
            .map(|_| PhysicalPlan::PartialAggregate {
                source: Box::new(PhysicalPlan::ProjectTable {
                    table_id: *table_id,
                    outputs: aggregates.to_vec(),
                    filter: source_filter.clone(),
                    order_by: Vec::new(),
                    limit: None,
                    offset: None,
                    distinct: false,
                    distinct_on: Vec::new(),
                    access_path: ScanAccessPath::SeqScan,
                }),
                group_by: group_by.to_vec(),
                aggregates: aggregate_exprs.clone(),
                output_fields: output_fields.clone(),
            })
            .collect();

        super::distributed_aggregate::execute_final_aggregate_plan(
            self,
            &partials,
            group_by,
            &aggregate_exprs,
            &having.cloned(),
            &output_fields,
            order_by,
            &limit.cloned(),
            &offset.cloned(),
            context,
        )
        .map(Some)
    }

    fn try_execute_distributed_scan_count_source_fast_path(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
        source: &PhysicalPlan,
        group_by: &[TypedExpr],
        grouping_sets: &[Vec<usize>],
        aggregates: &[ProjectionExpr],
        having: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        limit: Option<&TypedExpr>,
        offset: Option<&TypedExpr>,
        distinct: bool,
    ) -> DbResult<Option<ExecutionResult>> {
        if !group_by.is_empty()
            || !grouping_sets.is_empty()
            || having.is_some()
            || filter.is_some()
            || !order_by.is_empty()
            || distinct
            || !aggregate_source_count_star_outputs(aggregates)
        {
            return Ok(None);
        }
        let PhysicalPlan::DistributedScan {
            table_id,
            outputs,
            filter: source_filter,
            ..
        } = source
        else {
            return Ok(None);
        };
        if !project_table_outputs_are_identity(outputs) {
            return Ok(None);
        }
        let Some(simple_filter) = extract_derived_simple_filter(source_filter.as_ref()) else {
            return Ok(None);
        };
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, *table_id)?
        else {
            return Ok(None);
        };
        if self
            .compile_compat_rls_policies(
                &table,
                super::dml_plans::CompatRlsAction::Select,
                context,
            )?
            .is_some()
        {
            return Ok(None);
        }

        let required_ordinals = simple_filter
            .as_ref()
            .map(|filter| vec![filter.column_ordinal])
            .unwrap_or_default();
        let count = self.count_project_table_rows_for_simple_aggregate_source(
            context,
            *table_id,
            simple_filter.as_ref(),
            &ScanAccessPath::SeqScan,
            &required_ordinals,
        )?;

        let offset_val = offset
            .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "OFFSET"))
            .transpose()?
            .unwrap_or(0)
            .saturating_add(context.collect_row_offset);
        if offset_val > 0 {
            return Ok(Some(ExecutionResult::Query {
                columns: plan.output_fields(),
                rows: Vec::new(),
            }));
        }
        let plan_limit = limit
            .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "LIMIT"))
            .transpose()?;
        if matches!(
            effective_collect_limit(plan_limit, context.collect_row_limit),
            Some(0)
        ) {
            return Ok(Some(ExecutionResult::Query {
                columns: plan.output_fields(),
                rows: Vec::new(),
            }));
        }
        let row = Row::new(vec![
            Value::BigInt(i64::try_from(count).unwrap_or(i64::MAX));
            aggregates.len()
        ]);
        ensure_result_bytes_fit_and_track_query_row(context, &row, 0)?;
        Ok(Some(ExecutionResult::Query {
            columns: plan.output_fields(),
            rows: vec![row],
        }))
    }

    pub(super) fn execute_aggregate_source_plan(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        let PhysicalPlan::AggregateSource {
            source,
            group_by,
            grouping_sets,
            aggregates,
            having,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on: _,
        } = plan
        else {
            return Err(DbError::internal(
                "non-derived aggregate plan routed to derived aggregate executor",
            ));
        };

        // `SELECT MIN(col) FROM t` / `SELECT MAX(col) FROM t` fast
        // path. The planner emits AggregateSource over a SeqScan
        // for this shape; this dispatcher (not
        // \`execute_aggregate_or_set_plan\`) is what actually runs
        // for those queries. Walk the column's btree leaves
        // directly via \`try_min_or_max_via_index\` to return MIN/MAX
        // in O(log N) instead of materialising every row through the
        // accumulator.
        if group_by.is_empty()
            && grouping_sets.is_empty()
            && having.is_none()
            && order_by.is_empty()
            && !*distinct
            && filter.is_none()
            && aggregates.len() == 1
        {
            if let PhysicalPlan::SeqScan { table_id } = source.as_ref() {
                let agg = &aggregates[0];
                let min_max_target = match &agg.expr.kind {
                    TypedExprKind::AggMin { expr, filter: None } => Some((expr.as_ref(), false)),
                    TypedExprKind::AggMax { expr, filter: None } => Some((expr.as_ref(), true)),
                    _ => None,
                };
                if let Some((agg_expr, is_max)) = min_max_target {
                    if let TypedExprKind::ColumnRef { ordinal, .. } = &agg_expr.kind {
                        if let Some(value) = self.try_min_or_max_via_index(
                            context,
                            *table_id,
                            *ordinal,
                            &agg.field.data_type,
                            is_max,
                        )? {
                            return Ok(ExecutionResult::Query {
                                columns: plan.output_fields(),
                                rows: vec![Row::new(vec![value])],
                            });
                        }
                    }
                }
            }
        }

        let plan_limit = limit
            .as_ref()
            .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "LIMIT"))
            .transpose()?;
        let effective_limit = effective_collect_limit(plan_limit, context.collect_row_limit);
        context.check_deadline()?;
        if matches!(effective_limit, Some(0)) {
            return Ok(ExecutionResult::Query {
                columns: plan.output_fields(),
                rows: Vec::new(),
            });
        }
        if let Some(result) = self.try_execute_distributed_scan_count_source_fast_path(
            plan,
            context,
            source.as_ref(),
            group_by,
            grouping_sets,
            aggregates,
            having.as_ref(),
            filter.as_ref(),
            order_by,
            limit.as_ref(),
            offset.as_ref(),
            *distinct,
        )? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_distributed_scan_aggregate_source_fast_path(
            plan,
            context,
            source.as_ref(),
            group_by,
            grouping_sets,
            aggregates,
            having.as_ref(),
            filter.as_ref(),
            order_by,
            limit.as_ref(),
            offset.as_ref(),
            *distinct,
        )? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_project_table_aggregate_source_fast_path(
            plan,
            context,
            source.as_ref(),
            group_by,
            grouping_sets,
            aggregates,
            having.as_ref(),
            filter.as_ref(),
            order_by,
            limit.as_ref(),
            offset.as_ref(),
            *distinct,
        )? {
            return Ok(result);
        }
        if let Some(rows) = self.try_execute_hybrid_sql_graph_vector_agg(
            context,
            source,
            group_by,
            grouping_sets,
            aggregates,
            having.as_ref(),
            filter.as_ref(),
            order_by,
            offset.as_ref(),
            *distinct,
            effective_limit,
        )? {
            return Ok(ExecutionResult::Query {
                columns: plan.output_fields(),
                rows,
            });
        }

        if offset.is_none() {
            if let Some(mut rows) = self.try_group_by_count_over_inner_hash_join(
                context,
                source,
                group_by,
                aggregates,
                grouping_sets,
                having.as_ref(),
                filter.as_ref(),
                order_by,
                *distinct,
            )? {
                if let Some(limit) = effective_limit {
                    rows.truncate(clamp_u64_to_usize(limit, rows.len()));
                }
                return Ok(ExecutionResult::Query {
                    columns: plan.output_fields(),
                    rows,
                });
            }
        }

        let ExecutionResult::Query {
            rows: source_rows, ..
        } = self.execute(source, context)?
        else {
            return Err(DbError::internal(
                "derived aggregate source did not return query rows",
            ));
        };

        let mut groups: std::collections::HashMap<Vec<ValueHashKey>, usize> =
            std::collections::HashMap::new();
        let mut ordered_groups: Vec<Vec<AggAccumulator>> = Vec::new();
        let mut agg_templates: Vec<AggTemplate> = aggregates
            .iter()
            .map(|projection| classify_agg_expr(&projection.expr))
            .collect();

        let num_output_aggs = agg_templates.len();
        let mut extra_agg_exprs: Vec<AggregateExprRef<'_>> = Vec::new();
        {
            // Use a HashSet of Debug-formatted expression keys for O(1) dedup
            // instead of O(n) linear scans per candidate expression.
            let mut seen_agg_keys: std::collections::HashSet<String> = aggregates
                .iter()
                .map(|proj| format!("{:?}", proj.expr))
                .collect();

            let mut exprs_to_scan: Vec<&TypedExpr> = Vec::new();
            if let Some(having_expr) = having {
                exprs_to_scan.push(having_expr);
            }
            for sort in order_by {
                exprs_to_scan.push(&sort.expr);
            }
            for scan_expr in exprs_to_scan {
                for agg_expr in find_aggregate_subexprs(scan_expr) {
                    let key = format!("{agg_expr:?}");
                    if !seen_agg_keys.insert(key) {
                        continue;
                    }
                    agg_templates.push(classify_agg_expr(agg_expr));
                    extra_agg_exprs.push(AggregateExprRef::borrowed("", agg_expr));
                }
            }
        }
        let hidden_group_exprs =
            build_hidden_group_projections(group_by, aggregates, &extra_agg_exprs);
        agg_templates.extend(
            hidden_group_exprs
                .iter()
                .map(|projection| classify_agg_expr(projection.expr)),
        );
        let filter_requires_special_resolution = filter
            .as_ref()
            .is_some_and(super::projection_plans::expr_requires_special_resolution);
        let group_by_requires_special_resolution: Vec<bool> = group_by
            .iter()
            .map(super::projection_plans::expr_requires_special_resolution)
            .collect();
        let aggregate_filter_requires_special_resolution: Vec<bool> = agg_templates
            .iter()
            .map(|template| {
                template.filter.as_ref().is_some_and(|expr| {
                    super::projection_plans::expr_requires_special_resolution(expr)
                })
            })
            .collect();
        let all_projections: Vec<AggregateExprRef<'_>> = aggregates
            .iter()
            .map(AggregateExprRef::from_projection)
            .chain(extra_agg_exprs.iter().cloned())
            .chain(hidden_group_exprs.iter().cloned())
            .collect();
        let has_aggregate_windows = window_eval::has_window_functions(aggregates);

        // ── Grouping sets path ──
        if !grouping_sets.is_empty() {
            // Collect filtered input rows with their group-by values.
            let mut input_rows: Vec<(Row, Vec<Value>)> = Vec::new();
            for row in source_rows {
                context.check_deadline()?;
                if !predicate_matches(filter.as_ref().map(|expr| {
                    self.evaluate_expr_with_row_prechecked(
                        expr,
                        &row,
                        context,
                        filter_requires_special_resolution,
                    )
                }))? {
                    continue;
                }
                let mut gb_vals: Vec<Value> = Vec::with_capacity(group_by.len());
                for (gb, requires_special_resolution) in group_by
                    .iter()
                    .zip(group_by_requires_special_resolution.iter().copied())
                {
                    gb_vals.push(self.evaluate_expr_with_row_prechecked(
                        gb,
                        &row,
                        context,
                        requires_special_resolution,
                    )?);
                }
                input_rows.push((row, gb_vals));
            }

            let grouping_projs = find_grouping_projections(aggregates, group_by);

            let has_ordering = !order_by.is_empty();
            let aggregate_group_by_indices: Vec<Option<usize>> = aggregates
                .iter()
                .map(|projection| {
                    group_by
                        .iter()
                        .position(|gb| exprs_structurally_equal(gb, &projection.expr))
                })
                .collect();
            let offset_val = offset
                .as_ref()
                .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "OFFSET"))
                .transpose()?
                .unwrap_or(0);
            let mut result_rows: Vec<SortedQueryRow> = Vec::new();
            let mut result_bytes = 0u64;
            let empty_sort_keys = std::sync::Arc::new(Vec::new());

            for active_set in grouping_sets {
                let mut groups: std::collections::HashMap<Vec<ValueHashKey>, usize> =
                    std::collections::HashMap::new();
                let mut ordered_groups: Vec<Vec<AggAccumulator>> = Vec::new();
                let mut group_gb_values: Vec<Vec<Value>> = Vec::new();
                let mut active_set_positions = vec![None; group_by.len()];
                for (position, &group_by_index) in active_set.iter().enumerate() {
                    if let Some(slot) = active_set_positions.get_mut(group_by_index) {
                        if slot.is_none() {
                            *slot = Some(position);
                        }
                    }
                }

                for (row, gb_vals) in &input_rows {
                    context.check_deadline()?;
                    let partial_key: Vec<ValueHashKey> = active_set
                        .iter()
                        .map(|&idx| build_hash_key(&gb_vals[idx]))
                        .collect::<DbResult<Vec<_>>>()?;

                    let group_idx = match groups.entry(partial_key) {
                        std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            context.track_memory(estimate_row_bytes(row).saturating_add(64))?;
                            let vals: Vec<Value> =
                                active_set.iter().map(|&idx| gb_vals[idx].clone()).collect();
                            group_gb_values.push(vals);
                            let group_idx = ordered_groups.len();
                            ordered_groups.push(
                                agg_templates
                                    .iter()
                                    .map(AggAccumulator::from_template)
                                    .collect(),
                            );
                            entry.insert(group_idx);
                            group_idx
                        }
                    };
                    let accumulators = ordered_groups.get_mut(group_idx).ok_or_else(|| {
                        DbError::internal(
                            "missing accumulator group during grouping sets evaluation",
                        )
                    })?;

                    for (template_idx, (accumulator, template)) in accumulators
                        .iter_mut()
                        .zip(agg_templates.iter())
                        .enumerate()
                    {
                        if let Some(filter_expr) = &template.filter {
                            let filter_value = self.evaluate_expr_with_row_prechecked(
                                filter_expr,
                                row,
                                context,
                                aggregate_filter_requires_special_resolution[template_idx],
                            )?;
                            if !matches!(filter_value, Value::Boolean(true)) {
                                continue;
                            }
                        }
                        self.accumulate_value(accumulator, template, row, context)?;
                    }
                }

                // Empty grouping set () with no input rows → grand-total row.
                if ordered_groups.is_empty() && active_set.is_empty() {
                    group_gb_values.push(Vec::new());
                    ordered_groups.push(
                        agg_templates
                            .iter()
                            .map(AggAccumulator::from_template)
                            .collect(),
                    );
                }

                for (group_idx, accumulators) in ordered_groups.iter().enumerate() {
                    context.check_deadline()?;
                    let mut finalized_values = Vec::with_capacity(accumulators.len());
                    for (accumulator, template) in accumulators.iter().zip(agg_templates.iter()) {
                        finalized_values.push(finalize_accumulator(
                            accumulator,
                            template,
                            &self.evaluator,
                            context,
                        )?);
                    }

                    // Patch group-by column values: active → real value,
                    // inactive → NULL.
                    let active_vals = group_gb_values.get(group_idx);
                    for (out_idx, gb_idx) in aggregate_group_by_indices.iter().copied().enumerate()
                    {
                        let Some(gb_idx) = gb_idx else {
                            continue;
                        };
                        if let Some(active_pos) =
                            active_set_positions.get(gb_idx).copied().flatten()
                        {
                            if let Some(vals) = active_vals {
                                if let Some(v) = vals.get(active_pos) {
                                    if out_idx < finalized_values.len() {
                                        finalized_values[out_idx] = v.clone();
                                    }
                                }
                            }
                        } else if out_idx < finalized_values.len() {
                            finalized_values[out_idx] = Value::Null;
                        }
                    }

                    // Patch grouping() function values.
                    for (out_idx, ref col_indices) in &grouping_projs {
                        context.check_deadline()?;
                        if *out_idx < finalized_values.len() {
                            finalized_values[*out_idx] =
                                Value::Int(compute_grouping_bitmask(col_indices, active_set));
                        }
                    }

                    let agg_row = Row::new(finalized_values);

                    if let Some(having_expr) = having {
                        let having_value = self.evaluate_having_expr_extended(
                            having_expr,
                            &agg_row,
                            &all_projections,
                            context,
                        )?;
                        match having_value {
                            Value::Boolean(true) => {}
                            Value::Boolean(false) | Value::Null => continue,
                            _ => {
                                return Err(DbError::internal(
                                    "HAVING expression did not evaluate to BOOLEAN",
                                ));
                            }
                        }
                    }

                    if usize_to_u64(result_rows.len()) >= context.max_result_rows {
                        return Err(DbError::program_limit(
                            "maximum number of result rows reached",
                        ));
                    }

                    let sort_keys = if has_ordering {
                        std::sync::Arc::new(
                            order_by
                                .iter()
                                .map(|sort| {
                                    self.evaluate_having_expr_extended(
                                        &sort.expr,
                                        &agg_row,
                                        &all_projections,
                                        context,
                                    )
                                })
                                .collect::<DbResult<Vec<_>>>()?,
                        )
                    } else {
                        std::sync::Arc::clone(&empty_sort_keys)
                    };
                    let output_row = if num_output_aggs < agg_row.values.len() {
                        let mut values = agg_row.values;
                        values.truncate(num_output_aggs);
                        Row::new(values)
                    } else {
                        agg_row
                    };
                    push_sorted_query_row(
                        &mut result_rows,
                        context,
                        output_row,
                        sort_keys,
                        &mut result_bytes,
                    )?;
                }
            }

            if has_ordering {
                sort_query_rows(&mut result_rows, order_by, context)?;
            }

            let mut rows = result_rows
                .into_iter()
                .map(|entry| entry.row)
                .collect::<Vec<_>>();

            if has_aggregate_windows {
                window_eval::evaluate_post_aggregate_windows(self, aggregates, &mut rows, context)?;
            }
            if *distinct {
                deduplicate_rows(&mut rows, context)?;
            }

            let total_offset = offset_val.saturating_add(context.collect_row_offset);
            if total_offset > 0 {
                let skip = clamp_u64_to_usize(total_offset, rows.len());
                rows.drain(..skip);
            }
            if let Some(limit) = effective_limit {
                rows.truncate(clamp_u64_to_usize(limit, rows.len()));
            }

            return Ok(ExecutionResult::Query {
                columns: plan.output_fields(),
                rows,
            });
        }

        // ── Standard (non-grouping-sets) path ──
        // Single-column GROUP BY fast path (mirror of the one in
        // `execute_aggregate_source_plan_refactor`): avoid the per-row
        // `Vec<ValueHashKey>` allocation by hashing a single
        // `ValueHashKey`. Downstream `ordered_groups` is populated
        // identically.
        if group_by.len() == 1 {
            let gb_expr = &group_by[0];
            let gb_requires_special_resolution = group_by_requires_special_resolution
                .first()
                .copied()
                .unwrap_or(false);
            let mut single_groups: std::collections::HashMap<ValueHashKey, usize> =
                std::collections::HashMap::new();
            for row in source_rows {
                context.check_deadline()?;
                if !predicate_matches(filter.as_ref().map(|expr| {
                    self.evaluate_expr_with_row_prechecked(
                        expr,
                        &row,
                        context,
                        filter_requires_special_resolution,
                    )
                }))? {
                    continue;
                }

                let value = self.evaluate_expr_with_row_prechecked(
                    gb_expr,
                    &row,
                    context,
                    gb_requires_special_resolution,
                )?;
                let key = build_hash_key(&value)?;
                let group_idx = match single_groups.entry(key) {
                    std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        context.track_memory(estimate_row_bytes(&row).saturating_add(64))?;
                        let group_idx = ordered_groups.len();
                        ordered_groups.push(
                            agg_templates
                                .iter()
                                .map(AggAccumulator::from_template)
                                .collect(),
                        );
                        entry.insert(group_idx);
                        group_idx
                    }
                };
                let accumulators = ordered_groups.get_mut(group_idx).ok_or_else(|| {
                    DbError::internal("aggregate grouping state is inconsistent: missing group")
                })?;

                for (template_idx, (accumulator, template)) in accumulators
                    .iter_mut()
                    .zip(agg_templates.iter())
                    .enumerate()
                {
                    if let Some(filter_expr) = &template.filter {
                        let filter_value = self.evaluate_expr_with_row_prechecked(
                            filter_expr,
                            &row,
                            context,
                            aggregate_filter_requires_special_resolution[template_idx],
                        )?;
                        if !matches!(filter_value, Value::Boolean(true)) {
                            continue;
                        }
                    }
                    self.accumulate_value(accumulator, template, &row, context)?;
                }
            }
            for (key, idx) in single_groups {
                groups.insert(vec![key], idx);
            }
        } else {
            for row in source_rows {
                context.check_deadline()?;
                if !predicate_matches(filter.as_ref().map(|expr| {
                    self.evaluate_expr_with_row_prechecked(
                        expr,
                        &row,
                        context,
                        filter_requires_special_resolution,
                    )
                }))? {
                    continue;
                }

                let mut group_key: Vec<ValueHashKey> = Vec::with_capacity(group_by.len());
                for (expr, requires_special_resolution) in group_by
                    .iter()
                    .zip(group_by_requires_special_resolution.iter().copied())
                {
                    let value = self.evaluate_expr_with_row_prechecked(
                        expr,
                        &row,
                        context,
                        requires_special_resolution,
                    )?;
                    group_key.push(build_hash_key(&value)?);
                }

                let group_idx = match groups.entry(group_key) {
                    std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        context.track_memory(estimate_row_bytes(&row).saturating_add(64))?;
                        let group_idx = ordered_groups.len();
                        ordered_groups.push(
                            agg_templates
                                .iter()
                                .map(AggAccumulator::from_template)
                                .collect(),
                        );
                        entry.insert(group_idx);
                        group_idx
                    }
                };
                let accumulators = ordered_groups.get_mut(group_idx).ok_or_else(|| {
                    DbError::internal("aggregate grouping state is inconsistent: missing group")
                })?;

                for (template_idx, (accumulator, template)) in accumulators
                    .iter_mut()
                    .zip(agg_templates.iter())
                    .enumerate()
                {
                    if let Some(filter_expr) = &template.filter {
                        let filter_value = self.evaluate_expr_with_row_prechecked(
                            filter_expr,
                            &row,
                            context,
                            aggregate_filter_requires_special_resolution[template_idx],
                        )?;
                        if !matches!(filter_value, Value::Boolean(true)) {
                            continue;
                        }
                    }
                    self.accumulate_value(accumulator, template, &row, context)?;
                }
            }
        }

        if ordered_groups.is_empty() && group_by.is_empty() {
            let default_key: Vec<ValueHashKey> = Vec::new();
            groups.insert(default_key, 0);
            ordered_groups.push(
                agg_templates
                    .iter()
                    .map(AggAccumulator::from_template)
                    .collect(),
            );
        }

        let mut result_rows: Vec<SortedQueryRow> = Vec::new();
        let mut result_bytes = 0u64;
        let has_ordering = !order_by.is_empty();
        let empty_sort_keys = std::sync::Arc::new(Vec::new());
        let offset_val = offset
            .as_ref()
            .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "OFFSET"))
            .transpose()?
            .unwrap_or(0);

        for accumulators in &ordered_groups {
            context.check_deadline()?;
            let mut finalized_values = Vec::with_capacity(accumulators.len());
            for (accumulator, template) in accumulators.iter().zip(agg_templates.iter()) {
                finalized_values.push(finalize_accumulator(
                    accumulator,
                    template,
                    &self.evaluator,
                    context,
                )?);
            }
            let agg_row = Row::new(finalized_values);
            if let Some(having_expr) = having {
                let having_value = self.evaluate_having_expr_extended(
                    having_expr,
                    &agg_row,
                    &all_projections,
                    context,
                )?;
                match having_value {
                    Value::Boolean(true) => {}
                    Value::Boolean(false) | Value::Null => continue,
                    _ => {
                        return Err(DbError::internal(
                            "HAVING expression did not evaluate to BOOLEAN",
                        ));
                    }
                }
            }

            let sort_keys = if has_ordering {
                std::sync::Arc::new(
                    order_by
                        .iter()
                        .map(|sort| {
                            self.evaluate_having_expr_extended(
                                &sort.expr,
                                &agg_row,
                                &all_projections,
                                context,
                            )
                        })
                        .collect::<DbResult<Vec<_>>>()?,
                )
            } else {
                std::sync::Arc::clone(&empty_sort_keys)
            };
            let output_row = if num_output_aggs < agg_row.values.len() {
                let mut values = agg_row.values;
                values.truncate(num_output_aggs);
                Row::new(values)
            } else {
                agg_row
            };
            push_sorted_query_row(
                &mut result_rows,
                context,
                output_row,
                sort_keys,
                &mut result_bytes,
            )?;
        }

        if has_ordering {
            sort_query_rows(&mut result_rows, order_by, context)?;
        }

        let mut rows = result_rows
            .into_iter()
            .map(|entry| entry.row)
            .collect::<Vec<_>>();

        if has_aggregate_windows {
            window_eval::evaluate_post_aggregate_windows(self, aggregates, &mut rows, context)?;
        }
        if *distinct {
            deduplicate_rows(&mut rows, context)?;
        }

        let total_offset = offset_val.saturating_add(context.collect_row_offset);
        if total_offset > 0 {
            let skip = clamp_u64_to_usize(total_offset, rows.len());
            rows.drain(..skip);
        }
        if let Some(limit) = effective_limit {
            rows.truncate(clamp_u64_to_usize(limit, rows.len()));
        }

        let result = ExecutionResult::Query {
            columns: plan.output_fields(),
            rows,
        };
        Ok(result)
    }
}

fn sort_rows_by_order(
    executor: &Executor,
    rows: &mut [Row],
    order_by: &[SortExpr],
    outputs: &[aiondb_plan::ProjectionExpr],
    context: &ExecutionContext,
) -> DbResult<()> {
    let rebased_order_by =
        super::projection_plans::rebase_order_by_to_output_ordinals(outputs, order_by);
    let sort_col_indices: Vec<Option<usize>> = rebased_order_by
        .iter()
        .map(|sort| outputs.iter().position(|output| output.expr == sort.expr))
        .collect();

    sort_rows_by_exprs(
        rows,
        &rebased_order_by,
        &executor.evaluator,
        Some(&sort_col_indices),
        context,
    )?;

    for row in rows.iter() {
        track_query_row_memory(context, row)?;
    }
    Ok(())
}

fn deduplicate_rows(rows: &mut Vec<Row>, context: &ExecutionContext) -> DbResult<()> {
    dedup_rows_by_value_hash(rows, context)
}

fn apply_distinct_on_rows(
    executor: &Executor,
    rows: &mut Vec<Row>,
    distinct_on: &[TypedExpr],
    context: &ExecutionContext,
) -> DbResult<()> {
    let distinct_on_requires_special_resolution: Vec<bool> = distinct_on
        .iter()
        .map(super::projection_plans::expr_requires_special_resolution)
        .collect();
    super::projection_plans::apply_distinct_on_with(
        rows,
        distinct_on,
        context,
        |position, expr, row| {
            executor.evaluate_expr_with_row_prechecked(
                expr,
                row,
                context,
                distinct_on_requires_special_resolution[position],
            )
        },
    )
}
