pub(crate) fn extract_join_equi_keys(
    condition: Option<&TypedExpr>,
    filter: Option<&TypedExpr>,
    left_width: usize,
    right_width: usize,
) -> Option<(Vec<usize>, Vec<usize>)> {
    if let Some((left_keys, right_keys, _)) =
        extract_equi_join_keys(condition, left_width, right_width)
    {
        return Some((left_keys, right_keys));
    }
    if let Some((left_keys, right_keys, _)) =
        extract_equi_join_keys(filter, left_width, right_width)
    {
        return Some((left_keys, right_keys));
    }

    let combined = match (condition, filter) {
        (Some(condition), Some(filter)) => {
            Some(TypedExpr::logical_and(condition.clone(), filter.clone()))
        }
        (Some(condition), None) => Some(condition.clone()),
        (None, Some(filter)) => Some(filter.clone()),
        (None, None) => None,
    };
    extract_equi_join_keys(combined.as_ref(), left_width, right_width)
        .map(|(left_keys, right_keys, _)| (left_keys, right_keys))
}

fn best_join_orientation_cost(
    left_rows: f64,
    right_rows: f64,
    has_equi_keys: bool,
    left_sorted: bool,
    right_sorted: bool,
) -> PlanCost {
    let nlj_cost = PlanCost::nested_loop_join(left_rows, right_rows);
    if !has_equi_keys {
        return nlj_cost;
    }

    let hj_cost = PlanCost::hash_join(left_rows, right_rows);
    let mj_cost = PlanCost::merge_join(left_rows, right_rows, left_sorted, right_sorted);
    let mut best = nlj_cost;
    if hj_cost.cheaper_than(best) {
        best = hj_cost;
    }
    if mj_cost.cheaper_than(best) {
        best = mj_cost;
    }
    best
}

fn swap_join_ordinal(ordinal: usize, left_width: usize, right_width: usize) -> usize {
    if ordinal < left_width {
        ordinal.saturating_add(right_width)
    } else {
        ordinal.saturating_sub(left_width)
    }
}

pub(crate) fn remap_typed_expr_for_join_swap(
    expr: TypedExpr,
    remap: JoinSwapOrdinalRemap,
) -> TypedExpr {
    map_ordinals(expr, |ordinal| {
        swap_join_ordinal(ordinal, remap.left_width, remap.right_width)
    })
}

pub(crate) fn remap_projection_expr_for_join_swap(
    projection: ProjectionExpr,
    remap: JoinSwapOrdinalRemap,
) -> ProjectionExpr {
    ProjectionExpr {
        field: projection.field,
        expr: remap_typed_expr_for_join_swap(projection.expr, remap),
    }
}

pub(crate) fn remap_sort_expr_for_join_swap(
    sort: SortExpr,
    remap: JoinSwapOrdinalRemap,
) -> SortExpr {
    SortExpr {
        expr: remap_typed_expr_for_join_swap(sort.expr, remap),
        descending: sort.descending,
        nulls_first: sort.nulls_first,
    }
}

fn plan_is_direct_hybrid_source(plan: &PhysicalPlan) -> bool {
    match plan {
        PhysicalPlan::HybridFunctionScan { args, .. } => {
            !args.iter().any(typed_expr_contains_outer_refs)
        }
        _ => false,
    }
}

// Hybrid-rooted source trees remain safe to reorder for inner joins as long as
// the subtree is passive: projections/aggregations over the hybrid scan that
// do not capture outer references.
fn plan_is_swappable_hybrid_source(plan: &PhysicalPlan) -> bool {
    if plan_is_direct_hybrid_source(plan) {
        return true;
    }

    match plan {
        PhysicalPlan::ProjectSource {
            source,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct: _,
            distinct_on,
        } => {
            plan_is_swappable_hybrid_source(source)
                && !outputs
                    .iter()
                    .any(|projection| typed_expr_contains_outer_refs(&projection.expr))
                && !outputs
                    .iter()
                    .any(|projection| typed_expr_contains_set_returning_functions(&projection.expr))
                && !filter.as_ref().is_some_and(typed_expr_contains_outer_refs)
                && !filter
                    .as_ref()
                    .is_some_and(typed_expr_contains_set_returning_functions)
                && !order_by
                    .iter()
                    .any(|sort| typed_expr_contains_outer_refs(&sort.expr))
                && !order_by
                    .iter()
                    .any(|sort| typed_expr_contains_set_returning_functions(&sort.expr))
                && !limit.as_ref().is_some_and(typed_expr_contains_outer_refs)
                && !limit
                    .as_ref()
                    .is_some_and(typed_expr_contains_set_returning_functions)
                && !offset.as_ref().is_some_and(typed_expr_contains_outer_refs)
                && !offset
                    .as_ref()
                    .is_some_and(typed_expr_contains_set_returning_functions)
                && !distinct_on.iter().any(typed_expr_contains_outer_refs)
                && !distinct_on
                    .iter()
                    .any(typed_expr_contains_set_returning_functions)
        }
        PhysicalPlan::AggregateSource {
            source,
            group_by,
            aggregates,
            having,
            filter,
            order_by,
            limit,
            offset,
            distinct: _,
            distinct_on,
            ..
        } => {
            plan_is_swappable_hybrid_source(source)
                && !group_by.iter().any(typed_expr_contains_outer_refs)
                && !group_by
                    .iter()
                    .any(typed_expr_contains_set_returning_functions)
                && !aggregates
                    .iter()
                    .any(|projection| typed_expr_contains_outer_refs(&projection.expr))
                && !aggregates
                    .iter()
                    .any(|projection| typed_expr_contains_set_returning_functions(&projection.expr))
                && !having.as_ref().is_some_and(typed_expr_contains_outer_refs)
                && !having
                    .as_ref()
                    .is_some_and(typed_expr_contains_set_returning_functions)
                && !filter.as_ref().is_some_and(typed_expr_contains_outer_refs)
                && !filter
                    .as_ref()
                    .is_some_and(typed_expr_contains_set_returning_functions)
                && !order_by
                    .iter()
                    .any(|sort| typed_expr_contains_outer_refs(&sort.expr))
                && !order_by
                    .iter()
                    .any(|sort| typed_expr_contains_set_returning_functions(&sort.expr))
                && !limit.as_ref().is_some_and(typed_expr_contains_outer_refs)
                && !limit
                    .as_ref()
                    .is_some_and(typed_expr_contains_set_returning_functions)
                && !offset.as_ref().is_some_and(typed_expr_contains_outer_refs)
                && !offset
                    .as_ref()
                    .is_some_and(typed_expr_contains_set_returning_functions)
                && !distinct_on.iter().any(typed_expr_contains_outer_refs)
                && !distinct_on
                    .iter()
                    .any(typed_expr_contains_set_returning_functions)
        }
        _ => false,
    }
}

/// Check if a physical plan contains any outer column references.
/// Plans with outer refs are part of correlated subqueries and must
/// not have their join sides swapped.
pub(super) fn plan_has_outer_refs(plan: &PhysicalPlan) -> bool {
    match plan {
        PhysicalPlan::HybridFunctionScan { args, .. } => {
            args.iter().any(typed_expr_contains_outer_refs)
        }
        PhysicalPlan::ProjectSource {
            source,
            outputs,
            filter,
            ..
        } => {
            plan_has_outer_refs(source)
                || outputs
                    .iter()
                    .any(|p| typed_expr_contains_outer_refs(&p.expr))
                || filter.as_ref().is_some_and(typed_expr_contains_outer_refs)
        }
        PhysicalPlan::ProjectTable { filter, .. } => {
            filter.as_ref().is_some_and(typed_expr_contains_outer_refs)
        }
        _ => false,
    }
}

fn typed_expr_contains_outer_refs(expr: &TypedExpr) -> bool {
    match &expr.kind {
        TypedExprKind::OuterColumnRef { .. } => true,
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
            typed_expr_contains_outer_refs(left) || typed_expr_contains_outer_refs(right)
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => typed_expr_contains_outer_refs(expr),
        TypedExprKind::Like { expr, pattern, .. } => {
            typed_expr_contains_outer_refs(expr) || typed_expr_contains_outer_refs(pattern)
        }
        TypedExprKind::InList { expr, list, .. } => {
            typed_expr_contains_outer_refs(expr) || list.iter().any(typed_expr_contains_outer_refs)
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            typed_expr_contains_outer_refs(expr)
                || typed_expr_contains_outer_refs(low)
                || typed_expr_contains_outer_refs(high)
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions.iter().any(typed_expr_contains_outer_refs)
                || results.iter().any(typed_expr_contains_outer_refs)
                || else_result
                    .as_ref()
                    .is_some_and(|expr| typed_expr_contains_outer_refs(expr))
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args }
        | TypedExprKind::UserFunction { args, .. } => {
            args.iter().any(typed_expr_contains_outer_refs)
        }
        TypedExprKind::AggCount { expr, filter, .. } => {
            expr.as_ref()
                .is_some_and(|expr| typed_expr_contains_outer_refs(expr))
                || filter
                    .as_ref()
                    .is_some_and(|expr| typed_expr_contains_outer_refs(expr))
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
        | TypedExprKind::AggVarSamp { expr, filter } => {
            typed_expr_contains_outer_refs(expr)
                || filter
                    .as_ref()
                    .is_some_and(|expr| typed_expr_contains_outer_refs(expr))
        }
        TypedExprKind::AggStringAgg {
            expr,
            delimiter,
            filter,
            ..
        } => {
            typed_expr_contains_outer_refs(expr)
                || typed_expr_contains_outer_refs(delimiter)
                || filter
                    .as_ref()
                    .is_some_and(|expr| typed_expr_contains_outer_refs(expr))
        }
        TypedExprKind::AggArrayAgg { expr, filter, .. } => {
            typed_expr_contains_outer_refs(expr)
                || filter
                    .as_ref()
                    .is_some_and(|expr| typed_expr_contains_outer_refs(expr))
        }
        TypedExprKind::WindowFunction {
            args,
            partition_by,
            order_by,
            ..
        } => {
            args.iter().any(typed_expr_contains_outer_refs)
                || partition_by.iter().any(typed_expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| typed_expr_contains_outer_refs(&sort.expr))
        }
        TypedExprKind::ScalarSubquery { .. }
        | TypedExprKind::ArraySubquery { .. }
        | TypedExprKind::ExistsSubquery { .. }
        | TypedExprKind::InSubquery { .. } => true,
        _ => false,
    }
}

fn typed_expr_contains_set_returning_functions(expr: &TypedExpr) -> bool {
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
            typed_expr_contains_set_returning_functions(left)
                || typed_expr_contains_set_returning_functions(right)
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => typed_expr_contains_set_returning_functions(expr),
        TypedExprKind::Like { expr, pattern, .. } => {
            typed_expr_contains_set_returning_functions(expr)
                || typed_expr_contains_set_returning_functions(pattern)
        }
        TypedExprKind::InList { expr, list, .. } => {
            typed_expr_contains_set_returning_functions(expr)
                || list.iter().any(typed_expr_contains_set_returning_functions)
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            typed_expr_contains_set_returning_functions(expr)
                || typed_expr_contains_set_returning_functions(low)
                || typed_expr_contains_set_returning_functions(high)
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions
                .iter()
                .any(typed_expr_contains_set_returning_functions)
                || results
                    .iter()
                    .any(typed_expr_contains_set_returning_functions)
                || else_result
                    .as_ref()
                    .is_some_and(|expr| typed_expr_contains_set_returning_functions(expr))
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ArrayConstruct { elements: args }
        | TypedExprKind::UserFunction { args, .. } => {
            args.iter().any(typed_expr_contains_set_returning_functions)
        }
        TypedExprKind::ScalarFunction { func, args } => {
            is_set_returning_function(func)
                || args.iter().any(typed_expr_contains_set_returning_functions)
        }
        TypedExprKind::AggCount { expr, filter, .. } => {
            expr.as_ref()
                .is_some_and(|expr| typed_expr_contains_set_returning_functions(expr))
                || filter
                    .as_ref()
                    .is_some_and(|expr| typed_expr_contains_set_returning_functions(expr))
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
        | TypedExprKind::AggVarSamp { expr, filter } => {
            typed_expr_contains_set_returning_functions(expr)
                || filter
                    .as_ref()
                    .is_some_and(|expr| typed_expr_contains_set_returning_functions(expr))
        }
        TypedExprKind::AggStringAgg {
            expr,
            delimiter,
            filter,
            ..
        } => {
            typed_expr_contains_set_returning_functions(expr)
                || typed_expr_contains_set_returning_functions(delimiter)
                || filter
                    .as_ref()
                    .is_some_and(|expr| typed_expr_contains_set_returning_functions(expr))
        }
        TypedExprKind::AggArrayAgg { expr, filter, .. } => {
            typed_expr_contains_set_returning_functions(expr)
                || filter
                    .as_ref()
                    .is_some_and(|expr| typed_expr_contains_set_returning_functions(expr))
        }
        TypedExprKind::WindowFunction {
            args,
            partition_by,
            order_by,
            ..
        } => {
            args.iter().any(typed_expr_contains_set_returning_functions)
                || partition_by
                    .iter()
                    .any(typed_expr_contains_set_returning_functions)
                || order_by
                    .iter()
                    .any(|sort| typed_expr_contains_set_returning_functions(&sort.expr))
        }
        _ => false,
    }
}
