use super::*;

fn extract_filter(filter: Option<&TypedExpr>) -> Option<SharedExpr> {
    filter.cloned().map(Arc::new)
}

/// Check if an expression is a top-level aggregate function call.
fn is_top_level_aggregate(expr: &TypedExpr) -> bool {
    matches!(
        &expr.kind,
        TypedExprKind::AggCount { .. }
            | TypedExprKind::AggSum { .. }
            | TypedExprKind::AggAvg { .. }
            | TypedExprKind::AggAnyValue { .. }
            | TypedExprKind::AggMin { .. }
            | TypedExprKind::AggMax { .. }
            | TypedExprKind::AggStringAgg { .. }
            | TypedExprKind::AggArrayAgg { .. }
            | TypedExprKind::AggBoolAnd { .. }
            | TypedExprKind::AggBoolOr { .. }
            | TypedExprKind::AggStddevPop { .. }
            | TypedExprKind::AggStddevSamp { .. }
            | TypedExprKind::AggVarPop { .. }
            | TypedExprKind::AggVarSamp { .. }
    )
}

/// Recursively check if an expression contains any aggregate sub-expression.
pub(crate) fn expr_contains_aggregate(expr: &TypedExpr) -> bool {
    if is_top_level_aggregate(expr) {
        return true;
    }
    match &expr.kind {
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::BinaryLe { left, right }
        | TypedExprKind::LogicalAnd { left, right }
        | TypedExprKind::LogicalOr { left, right }
        | TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right }
        | TypedExprKind::Concat { left, right }
        | TypedExprKind::Nullif { left, right }
        | TypedExprKind::IsDistinctFrom { left, right, .. }
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
        | TypedExprKind::ArrayOverlap { left, right } => {
            expr_contains_aggregate(left) || expr_contains_aggregate(right)
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => expr_contains_aggregate(expr),
        TypedExprKind::Like { expr, pattern, .. } => {
            expr_contains_aggregate(expr) || expr_contains_aggregate(pattern)
        }
        TypedExprKind::InList { expr, list, .. } => {
            expr_contains_aggregate(expr) || list.iter().any(expr_contains_aggregate)
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            expr_contains_aggregate(expr)
                || expr_contains_aggregate(low)
                || expr_contains_aggregate(high)
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions.iter().any(expr_contains_aggregate)
                || results.iter().any(expr_contains_aggregate)
                || else_result
                    .as_ref()
                    .is_some_and(|e| expr_contains_aggregate(e))
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args } => {
            args.iter().any(expr_contains_aggregate)
        }
        TypedExprKind::UserFunction { args, .. } => args.iter().any(expr_contains_aggregate),
        _ => false,
    }
}

/// Collect all unique aggregate sub-expressions from an expression tree.
/// Does not descend into aggregate arguments (aggregates are treated as leaf nodes).
fn collect_aggregate_subexprs(expr: &TypedExpr) -> Vec<&TypedExpr> {
    let mut result = Vec::new();
    collect_aggregate_subexprs_inner(expr, &mut result);
    result
}

fn collect_aggregate_subexpr_keys(expr: &TypedExpr) -> Vec<(SharedExpr, AggTemplate)> {
    let mut result = Vec::new();
    collect_aggregate_subexpr_keys_inner(expr, &mut result);
    result
        .into_iter()
        .map(|(sub_expr, template)| (Arc::new(sub_expr.clone()), template))
        .collect()
}

/// Public helper to collect all top-level aggregate sub-expressions from an
/// expression tree. Used by the aggregate executor to discover aggregates
/// hidden inside HAVING / ORDER BY clauses.
pub(crate) fn find_aggregate_subexprs(expr: &TypedExpr) -> Vec<&TypedExpr> {
    collect_aggregate_subexprs(expr)
}

pub(crate) fn build_hidden_group_projections<'a>(
    group_by: &'a [TypedExpr],
    aggregates: &[ProjectionExpr],
    extra_agg_exprs: &[AggregateExprRef<'a>],
) -> Vec<AggregateExprRef<'a>> {
    let mut hidden_group_exprs: Vec<AggregateExprRef<'a>> = Vec::new();
    for group_expr in group_by {
        let already_visible = aggregates
            .iter()
            .any(|projection| exprs_structurally_equal(&projection.expr, group_expr));
        let already_hidden_agg = extra_agg_exprs
            .iter()
            .any(|projection| exprs_structurally_equal(projection.expr, group_expr));
        let already_hidden_group = hidden_group_exprs
            .iter()
            .any(|projection| exprs_structurally_equal(projection.expr, group_expr));
        if already_visible || already_hidden_agg || already_hidden_group {
            continue;
        }
        hidden_group_exprs.push(AggregateExprRef::owned(
            hidden_group_projection_name(group_expr),
            group_expr,
        ));
    }
    hidden_group_exprs
}

fn hidden_group_projection_name(expr: &TypedExpr) -> String {
    match &expr.kind {
        TypedExprKind::ColumnRef { name, .. } | TypedExprKind::OuterColumnRef { name, .. } => {
            name.rsplit('\0').next().unwrap_or(name).to_owned()
        }
        _ => String::new(),
    }
}

fn collect_aggregate_subexprs_inner<'a>(expr: &'a TypedExpr, out: &mut Vec<&'a TypedExpr>) {
    if is_top_level_aggregate(expr) {
        if !out
            .iter()
            .any(|existing| exprs_structurally_equal(existing, expr))
        {
            out.push(expr);
        }
        return;
    }
    match &expr.kind {
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::BinaryLe { left, right }
        | TypedExprKind::LogicalAnd { left, right }
        | TypedExprKind::LogicalOr { left, right }
        | TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right }
        | TypedExprKind::Concat { left, right }
        | TypedExprKind::Nullif { left, right }
        | TypedExprKind::IsDistinctFrom { left, right, .. }
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
        | TypedExprKind::ArrayOverlap { left, right } => {
            collect_aggregate_subexprs_inner(left, out);
            collect_aggregate_subexprs_inner(right, out);
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => {
            collect_aggregate_subexprs_inner(expr, out);
        }
        TypedExprKind::Like { expr, pattern, .. } => {
            collect_aggregate_subexprs_inner(expr, out);
            collect_aggregate_subexprs_inner(pattern, out);
        }
        TypedExprKind::InList { expr, list, .. } => {
            collect_aggregate_subexprs_inner(expr, out);
            for item in list {
                collect_aggregate_subexprs_inner(item, out);
            }
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            collect_aggregate_subexprs_inner(expr, out);
            collect_aggregate_subexprs_inner(low, out);
            collect_aggregate_subexprs_inner(high, out);
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            for c in conditions {
                collect_aggregate_subexprs_inner(c, out);
            }
            for r in results {
                collect_aggregate_subexprs_inner(r, out);
            }
            if let Some(e) = else_result {
                collect_aggregate_subexprs_inner(e, out);
            }
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args } => {
            for a in args {
                collect_aggregate_subexprs_inner(a, out);
            }
        }
        TypedExprKind::UserFunction { args, .. } => {
            for a in args {
                collect_aggregate_subexprs_inner(a, out);
            }
        }
        _ => {}
    }
}

fn collect_aggregate_subexpr_keys_inner<'a>(
    expr: &'a TypedExpr,
    out: &mut Vec<(&'a TypedExpr, AggTemplate)>,
) {
    if is_top_level_aggregate(expr) {
        if !out
            .iter()
            .any(|(existing, _)| exprs_structurally_equal(existing, expr))
        {
            let template = classify_single_agg_expr(expr);
            out.push((expr, template));
        }
        return;
    }
    match &expr.kind {
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::BinaryLe { left, right }
        | TypedExprKind::LogicalAnd { left, right }
        | TypedExprKind::LogicalOr { left, right }
        | TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right }
        | TypedExprKind::Concat { left, right }
        | TypedExprKind::Nullif { left, right }
        | TypedExprKind::IsDistinctFrom { left, right, .. }
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
        | TypedExprKind::ArrayOverlap { left, right } => {
            collect_aggregate_subexpr_keys_inner(left, out);
            collect_aggregate_subexpr_keys_inner(right, out);
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => {
            collect_aggregate_subexpr_keys_inner(expr, out);
        }
        TypedExprKind::Like { expr, pattern, .. } => {
            collect_aggregate_subexpr_keys_inner(expr, out);
            collect_aggregate_subexpr_keys_inner(pattern, out);
        }
        TypedExprKind::InList { expr, list, .. } => {
            collect_aggregate_subexpr_keys_inner(expr, out);
            for item in list {
                collect_aggregate_subexpr_keys_inner(item, out);
            }
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            collect_aggregate_subexpr_keys_inner(expr, out);
            collect_aggregate_subexpr_keys_inner(low, out);
            collect_aggregate_subexpr_keys_inner(high, out);
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            for c in conditions {
                collect_aggregate_subexpr_keys_inner(c, out);
            }
            for r in results {
                collect_aggregate_subexpr_keys_inner(r, out);
            }
            if let Some(e) = else_result {
                collect_aggregate_subexpr_keys_inner(e, out);
            }
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args } => {
            for a in args {
                collect_aggregate_subexpr_keys_inner(a, out);
            }
        }
        TypedExprKind::UserFunction { args, .. } => {
            for a in args {
                collect_aggregate_subexpr_keys_inner(a, out);
            }
        }
        _ => {}
    }
}

/// Classify a known top-level aggregate expression into its template.
/// Non-aggregate callers should use `classify_agg_expr`, which also
/// handles pass-through and composite expressions.
fn classify_single_agg_expr(expr: &TypedExpr) -> AggTemplate {
    match &expr.kind {
        TypedExprKind::AggCount {
            expr: None,
            distinct,
            filter,
        } => AggTemplate {
            kind: AggKind::CountStar,
            distinct: *distinct,
            filter: extract_filter(filter.as_deref()),
        },
        TypedExprKind::AggCount {
            expr: Some(inner),
            distinct,
            filter,
        } => AggTemplate {
            kind: AggKind::CountExpr(Arc::new(inner.as_ref().clone())),
            distinct: *distinct,
            filter: extract_filter(filter.as_deref()),
        },
        TypedExprKind::AggSum {
            expr: inner,
            distinct,
            filter,
        } => AggTemplate {
            kind: AggKind::Sum(Arc::new(inner.as_ref().clone())),
            distinct: *distinct,
            filter: extract_filter(filter.as_deref()),
        },
        TypedExprKind::AggAvg {
            expr: inner,
            distinct,
            filter,
        } => AggTemplate {
            kind: AggKind::Avg(Arc::new(inner.as_ref().clone())),
            distinct: *distinct,
            filter: extract_filter(filter.as_deref()),
        },
        TypedExprKind::AggAnyValue {
            expr: inner,
            filter,
        } => AggTemplate {
            kind: AggKind::AnyValue(Arc::new(inner.as_ref().clone())),
            distinct: false,
            filter: extract_filter(filter.as_deref()),
        },
        TypedExprKind::AggMin {
            expr: inner,
            filter,
        } => AggTemplate {
            kind: AggKind::Min(Arc::new(inner.as_ref().clone())),
            distinct: false,
            filter: extract_filter(filter.as_deref()),
        },
        TypedExprKind::AggMax {
            expr: inner,
            filter,
        } => AggTemplate {
            kind: AggKind::Max(Arc::new(inner.as_ref().clone())),
            distinct: false,
            filter: extract_filter(filter.as_deref()),
        },
        TypedExprKind::AggStringAgg {
            expr: inner,
            delimiter,
            distinct,
            filter,
        } => AggTemplate {
            kind: AggKind::StringAgg(
                Arc::new(inner.as_ref().clone()),
                Arc::new(delimiter.as_ref().clone()),
            ),
            distinct: *distinct,
            filter: extract_filter(filter.as_deref()),
        },
        TypedExprKind::AggArrayAgg {
            expr: inner,
            distinct,
            filter,
            order_descending,
        } => AggTemplate {
            kind: AggKind::ArrayAgg(Arc::new(inner.as_ref().clone()), *order_descending),
            distinct: *distinct,
            filter: extract_filter(filter.as_deref()),
        },
        TypedExprKind::AggBoolAnd {
            expr: inner,
            filter,
        } => AggTemplate {
            kind: AggKind::BoolAnd(Arc::new(inner.as_ref().clone())),
            distinct: false,
            filter: extract_filter(filter.as_deref()),
        },
        TypedExprKind::AggBoolOr {
            expr: inner,
            filter,
        } => AggTemplate {
            kind: AggKind::BoolOr(Arc::new(inner.as_ref().clone())),
            distinct: false,
            filter: extract_filter(filter.as_deref()),
        },
        TypedExprKind::AggStddevPop {
            expr: inner,
            filter,
        } => AggTemplate {
            kind: AggKind::StddevPop(Arc::new(inner.as_ref().clone())),
            distinct: false,
            filter: extract_filter(filter.as_deref()),
        },
        TypedExprKind::AggStddevSamp {
            expr: inner,
            filter,
        } => AggTemplate {
            kind: AggKind::StddevSamp(Arc::new(inner.as_ref().clone())),
            distinct: false,
            filter: extract_filter(filter.as_deref()),
        },
        TypedExprKind::AggVarPop {
            expr: inner,
            filter,
        } => AggTemplate {
            kind: AggKind::VarPop(Arc::new(inner.as_ref().clone())),
            distinct: false,
            filter: extract_filter(filter.as_deref()),
        },
        TypedExprKind::AggVarSamp {
            expr: inner,
            filter,
        } => AggTemplate {
            kind: AggKind::VarSamp(Arc::new(inner.as_ref().clone())),
            distinct: false,
            filter: extract_filter(filter.as_deref()),
        },
        _ => AggTemplate {
            kind: AggKind::PassThrough(Arc::new(expr.clone())),
            distinct: false,
            filter: None,
        },
    }
}

pub(crate) fn classify_agg_expr(expr: &TypedExpr) -> AggTemplate {
    if is_top_level_aggregate(expr) {
        return classify_single_agg_expr(expr);
    }

    if expr_contains_aggregate(expr) {
        let original = Arc::new(expr.clone());
        let sub_aggs = collect_aggregate_subexpr_keys(original.as_ref());
        if !sub_aggs.is_empty() {
            return AggTemplate {
                kind: AggKind::CompositeAgg { original, sub_aggs },
                distinct: false,
                filter: None,
            };
        }
    }
    AggTemplate {
        kind: AggKind::PassThrough(Arc::new(expr.clone())),
        distinct: false,
        filter: None,
    }
}
