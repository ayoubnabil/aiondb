//! Cypher predicate / filter / expression-analysis helpers.
//!
//! Split out of `graph_plans/mod.rs` (see the module docs there). Literal
//! and id extraction, column/range predicate matching, graph-variable
//! reference analysis, hybrid graph-vector filter extraction, and the
//! `GraphFilterConjunct` readiness model. Shared types/core helpers stay
//! in the parent module, reached via `use super::*`.
#![allow(clippy::too_many_lines, clippy::type_complexity)]

use super::*;

pub(in crate::executor) fn literal_value(expr: &TypedExpr) -> Option<Value> {
    match &expr.kind {
        TypedExprKind::Literal(value) => Some(value.clone()),
        _ => None,
    }
}

pub(in crate::executor) fn extract_start_id_literal(
    start: &CypherNodePattern,
    filter: Option<&TypedExpr>,
    start_variable: &str,
) -> Option<Value> {
    let inline_id = match start.properties.as_slice() {
        [] => None,
        [property] if property.key.eq_ignore_ascii_case("id") => literal_value(&property.value),
        _ => return None,
    };
    let filter_id = match filter {
        Some(expr) => Some(extract_exact_id_equality(expr, start_variable)?),
        None => None,
    };
    match (inline_id, filter_id) {
        (Some(inline), Some(filter)) => {
            let mut left = inline;
            let mut right = filter;
            normalize_int_key(&mut left);
            normalize_int_key(&mut right);
            (left == right).then_some(left)
        }
        (Some(inline), None) => Some(inline),
        (None, Some(filter)) => Some(filter),
        (None, None) => None,
    }
}

pub(in crate::executor) fn extract_exact_id_equality(expr: &TypedExpr, variable: &str) -> Option<Value> {
    let TypedExprKind::BinaryEq { left, right } = &expr.kind else {
        return None;
    };
    match (&left.kind, &right.kind) {
        (TypedExprKind::ColumnRef { name, .. }, TypedExprKind::Literal(value))
            if is_graph_id_ref(name, variable) =>
        {
            Some(value.clone())
        }
        (TypedExprKind::Literal(value), TypedExprKind::ColumnRef { name, .. })
            if is_graph_id_ref(name, variable) =>
        {
            Some(value.clone())
        }
        _ => None,
    }
}

pub(in crate::executor) fn is_graph_id_ref(name: &str, variable: &str) -> bool {
    name.eq_ignore_ascii_case(&format!("{variable}.id"))
}

pub(in crate::executor) struct HybridGraphVectorFilter {
    pub(in crate::executor) start_tenant: Value,
    pub(in crate::executor) target_tenant: Value,
    /// L2 query vector stored as `Vec<f32>` so the per-target distance
    /// loop can run through the SIMD `l2_squared_f64` kernel (which takes
    /// two `&[f32]` slices and accumulates in `f64`). Vector embeddings are
    /// f32 in storage, so converting once at filter-extraction time avoids
    /// a scalar zip/map/sum per target row.
    pub(in crate::executor) query_vector: Vec<f32>,
    pub(in crate::executor) distance_threshold: f64,
}

pub(in crate::executor) struct HybridDeepGraphVectorFilter {
    pub(in crate::executor) start_id: Value,
    /// See [`HybridGraphVectorFilter::query_vector`].
    pub(in crate::executor) query_vector: Vec<f32>,
    pub(in crate::executor) distance_threshold: f64,
    pub(in crate::executor) popularity_threshold: Value,
}

fn reconcile_literal_slot(slot: &mut Option<Value>, candidate: Value) -> bool {
    match slot {
        Some(existing) => {
            let mut left = existing.clone();
            let mut right = candidate;
            normalize_int_key(&mut left);
            normalize_int_key(&mut right);
            left == right
        }
        None => {
            *slot = Some(candidate);
            true
        }
    }
}

fn inline_property_literal(properties: &[CypherPropertyExpr], expected_key: &str) -> Option<Value> {
    let property = properties
        .iter()
        .find(|property| property.key.eq_ignore_ascii_case(expected_key))?;
    literal_value(&property.value)
}

fn properties_only_use_keys(properties: &[CypherPropertyExpr], allowed_keys: &[&str]) -> bool {
    properties.iter().all(|property| {
        allowed_keys
            .iter()
            .any(|allowed| property.key.eq_ignore_ascii_case(allowed))
            && literal_value(&property.value).is_some()
    })
}

/// Score a node pattern by inferred selectivity:
///   0 = literal-equality on indexed column (`index_scan` set)
///   1 = at least one literal property OR range pushdown
///        (storage can apply the predicate inline)
///   2 = label-only or no constraint (full SeqScan)
pub(in crate::executor) fn pivot_node_score(node: &CypherNodePattern) -> u8 {
    if node.index_scan.is_some() {
        return 0;
    }
    if !node.properties.is_empty() || !node.range_pushdown.is_empty() {
        return 1;
    }
    2
}

/// Pick the most-selective node in `pattern` to drive the match.
/// Returns `Some(idx)` only when the chosen pivot is BETTER than
/// the leftmost node — same-or-worse pivots leave the original
/// left-to-right walk in place to avoid pointless reordering.
/// Patterns of length 1 always return `None`.
pub(in crate::executor) fn pick_match_pivot_index(pattern: &CypherPattern) -> Option<usize> {
    if pattern.nodes.len() <= 1 {
        return None;
    }
    // Pivoting requires single-hop relationships everywhere along
    // the chain — variable-length expansion needs the original
    // walk so it can chain through `match_variable_length_relationship`.
    if pattern
        .relationships
        .iter()
        .any(|r| r.min_hops.is_some() || r.max_hops.is_some())
    {
        return None;
    }
    let leftmost_node = pattern.nodes.first()?;
    let leftmost_score = pivot_node_score(leftmost_node);
    let (best_idx, best_score) = pattern
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (i, pivot_node_score(n)))
        .min_by_key(|(_, score)| *score)?;
    if best_idx == 0 || best_score >= leftmost_score {
        return None;
    }
    Some(best_idx)
}

/// Reverse a relationship's direction so the matcher walks the
/// adjacency list backwards. `Both` stays `Both` — undirected
/// relationships are symmetric so reversing is a no-op.
pub(in crate::executor) fn flip_relationship_direction(rel: &CypherRelPattern) -> CypherRelPattern {
    let mut flipped = rel.clone();
    flipped.direction = match rel.direction {
        aiondb_plan::graph::CypherRelDirection::Outgoing => {
            aiondb_plan::graph::CypherRelDirection::Incoming
        }
        aiondb_plan::graph::CypherRelDirection::Incoming => {
            aiondb_plan::graph::CypherRelDirection::Outgoing
        }
        aiondb_plan::graph::CypherRelDirection::Both => {
            aiondb_plan::graph::CypherRelDirection::Both
        }
    };
    flipped
}

pub(in crate::executor) fn extract_hybrid_graph_vector_filter(
    filter: &TypedExpr,
    start_properties: &[CypherPropertyExpr],
    target_properties: &[CypherPropertyExpr],
    start_variable: &str,
    target_variable: &str,
) -> Option<HybridGraphVectorFilter> {
    if !properties_only_use_keys(start_properties, &["tenant_id"])
        || !properties_only_use_keys(target_properties, &["tenant_id"])
    {
        return None;
    }

    let mut conjuncts = Vec::new();
    collect_graph_filter_conjuncts(filter, &mut conjuncts);

    let mut start_tenant = inline_property_literal(start_properties, "tenant_id");
    let mut target_tenant = inline_property_literal(target_properties, "tenant_id");
    let mut query_vector = None;
    let mut distance_threshold = None;

    for conjunct in conjuncts {
        if let Some((name, value)) = exact_column_literal_equality(conjunct) {
            if name.eq_ignore_ascii_case(&format!("{start_variable}.tenant_id")) {
                if !reconcile_literal_slot(&mut start_tenant, value) {
                    return None;
                }
                continue;
            }
            if name.eq_ignore_ascii_case(&format!("{target_variable}.tenant_id")) {
                if !reconcile_literal_slot(&mut target_tenant, value) {
                    return None;
                }
                continue;
            }
        }

        if let Some((vector, threshold)) = extract_l2_distance_threshold(conjunct, target_variable)
        {
            query_vector = Some(vector);
            distance_threshold = Some(threshold);
            continue;
        }

        return None;
    }

    Some(HybridGraphVectorFilter {
        start_tenant: start_tenant?,
        target_tenant: target_tenant?,
        // Storage vectors are f32. Converting once here removes the
        // per-target `f64::from(*left)` cast inside the SIMD-replaceable
        // hot loop and lets that loop reach the `l2_squared_f64` kernel.
        query_vector: query_vector?.into_iter().map(|v| v as f32).collect(),
        distance_threshold: distance_threshold?,
    })
}

pub(in crate::executor) fn collect_graph_filter_conjuncts<'a>(
    expr: &'a TypedExpr,
    out: &mut Vec<&'a TypedExpr>,
) {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        if let TypedExprKind::LogicalAnd { left, right } = &expr.kind {
            stack.push(right);
            stack.push(left);
        } else {
            out.push(expr);
        }
    }
}

#[derive(Clone)]
pub(in crate::executor) struct GraphFilterConjunct<'a> {
    pub(in crate::executor) expr: &'a TypedExpr,
    pub(in crate::executor) referenced_vars: Option<HashSet<String>>,
}

impl<'a> GraphFilterConjunct<'a> {
    pub(in crate::executor) fn new(expr: &'a TypedExpr) -> Self {
        Self {
            expr,
            referenced_vars: referenced_graph_variables(expr),
        }
    }

    pub(in crate::executor) fn is_ready(&self, binding: &BindingRow) -> bool {
        let Some(vars) = self.referenced_vars.as_ref() else {
            return false;
        };
        vars.iter()
            .all(|variable| binding.get(variable.as_str()).is_some())
    }

    pub(in crate::executor) fn is_ready_with_names(&self, bound_names: &HashSet<String>) -> bool {
        let Some(vars) = self.referenced_vars.as_ref() else {
            return false;
        };
        vars.iter().all(|variable| bound_names.contains(variable))
    }
}

pub(in crate::executor) fn build_graph_filter_conjuncts(filter: &TypedExpr) -> Vec<GraphFilterConjunct<'_>> {
    let mut conjuncts = Vec::new();
    collect_graph_filter_conjuncts(filter, &mut conjuncts);
    conjuncts
        .into_iter()
        .map(GraphFilterConjunct::new)
        .collect()
}

pub(in crate::executor) fn referenced_graph_variables(expr: &TypedExpr) -> Option<HashSet<String>> {
    let mut vars = HashSet::new();
    if collect_referenced_graph_variables(expr, &mut vars) {
        Some(vars)
    } else {
        None
    }
}

pub(in crate::executor) fn referenced_graph_variables_set(
    expr: &TypedExpr,
) -> Option<HashSet<String>> {
    referenced_graph_variables(expr)
}

/// Graph variables a read-only RETURN / ORDER BY tail will read — but only
/// when the projection is *fully determinable*: every expression must reach
/// its graph data through an explicit `variable.property` path.
///
/// Returns `None` if any expression resolves positionally against the
/// flattened binding row (a bare / aliased column ref, a graph function, …).
/// Callers treat `None` as "every binding variable may be needed" and skip
/// binding pruning entirely; pruning on a partial set would strip the
/// columns positional access depends on and surface spurious NULLs.
pub(in crate::executor) fn cypher_query_output_variables(
    returns: &[ProjectionExpr],
    order_by: &[SortExpr],
) -> Option<HashSet<String>> {
    let mut keep = HashSet::new();
    for item in returns {
        keep.extend(referenced_graph_variables_set(&item.expr)?);
    }
    for sort in order_by {
        keep.extend(referenced_graph_variables_set(&sort.expr)?);
    }
    Some(keep)
}

pub(in crate::executor) fn cypher_query_binding_reduction(
    returns: &[ProjectionExpr],
    distinct: bool,
    order_by: &[SortExpr],
) -> Option<GraphBindingReduction> {
    if distinct || !order_by.is_empty() || returns.len() != 1 {
        return None;
    }
    match &returns[0].expr.kind {
        TypedExprKind::AggCount {
            expr: Some(expr),
            distinct: true,
            filter: None,
        } => Some(GraphBindingReduction::GlobalDistinctExpr((**expr).clone())),
        _ => None,
    }
}

pub(in crate::executor) fn collect_referenced_graph_variables(expr: &TypedExpr, vars: &mut HashSet<String>) -> bool {
    match &expr.kind {
        TypedExprKind::Literal(_) | TypedExprKind::NextValue { .. } => true,
        TypedExprKind::ColumnRef { name, .. } | TypedExprKind::OuterColumnRef { name, .. } => {
            // Only an explicit `variable.property` reference pins down a
            // specific graph binding variable. A bare or positional column
            // ref (e.g. `col1`) is resolved positionally against the
            // *flattened* binding row in `evaluate_cypher_expr_with_binding`,
            // so it can read columns contributed by any variable. Reporting
            // it as a single named variable would let binding pruning drop
            // the rows positional access needs and surface spurious NULLs,
            // so treat it as indeterminate and let callers keep every
            // binding.
            match name.split_once('.') {
                Some((head, _)) if !head.is_empty() => {
                    vars.insert(head.to_owned());
                    true
                }
                _ => false,
            }
        }
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
        | TypedExprKind::Nullif { left, right } => {
            collect_referenced_graph_variables(left, vars)
                && collect_referenced_graph_variables(right, vars)
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => collect_referenced_graph_variables(expr, vars),
        TypedExprKind::IsDistinctFrom { left, right, .. } => {
            collect_referenced_graph_variables(left, vars)
                && collect_referenced_graph_variables(right, vars)
        }
        TypedExprKind::Like { expr, pattern, .. } => {
            collect_referenced_graph_variables(expr, vars)
                && collect_referenced_graph_variables(pattern, vars)
        }
        TypedExprKind::InList { expr, list, .. } => {
            collect_referenced_graph_variables(expr, vars)
                && list
                    .iter()
                    .all(|item| collect_referenced_graph_variables(item, vars))
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            collect_referenced_graph_variables(expr, vars)
                && collect_referenced_graph_variables(low, vars)
                && collect_referenced_graph_variables(high, vars)
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions
                .iter()
                .chain(results.iter())
                .all(|item| collect_referenced_graph_variables(item, vars))
                && else_result
                    .as_deref()
                    .map_or(true, |item| collect_referenced_graph_variables(item, vars))
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ArrayConstruct { elements: args }
        | TypedExprKind::UserFunction { args, .. } => args
            .iter()
            .all(|arg| collect_referenced_graph_variables(arg, vars)),
        TypedExprKind::ScalarFunction { func, args } => {
            if let ScalarFunction::Generic(function_name) = func {
                if function_name.eq_ignore_ascii_case("__cypher_pattern_comprehension") {
                    if let Some(imported) = args.get(1) {
                        return collect_pattern_comprehension_imported_variables(imported, vars);
                    }
                    return false;
                }
            }
            args.iter()
                .all(|arg| collect_referenced_graph_variables(arg, vars))
        }
        TypedExprKind::AggCount { expr, filter, .. } => {
            expr.as_deref().map_or(true, |inner| {
                collect_referenced_graph_variables(inner, vars)
            }) && filter.as_deref().map_or(true, |inner| {
                collect_referenced_graph_variables(inner, vars)
            })
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
            collect_referenced_graph_variables(expr, vars)
                && filter.as_deref().map_or(true, |inner| {
                    collect_referenced_graph_variables(inner, vars)
                })
        }
        TypedExprKind::AggStringAgg {
            expr,
            delimiter,
            filter,
            ..
        } => {
            collect_referenced_graph_variables(expr, vars)
                && collect_referenced_graph_variables(delimiter, vars)
                && filter.as_deref().map_or(true, |inner| {
                    collect_referenced_graph_variables(inner, vars)
                })
        }
        _ => false,
    }
}

pub(in crate::executor) fn collect_pattern_comprehension_imported_variables(
    expr: &TypedExpr,
    vars: &mut HashSet<String>,
) -> bool {
    let TypedExprKind::ArrayConstruct { elements } = &expr.kind else {
        return false;
    };
    for element in elements {
        let TypedExprKind::Literal(Value::Text(name)) = &element.kind else {
            return false;
        };
        vars.insert(name.clone());
    }
    true
}

pub(in crate::executor) fn exact_column_literal_equality(expr: &TypedExpr) -> Option<(&str, Value)> {
    let TypedExprKind::BinaryEq { left, right } = &expr.kind else {
        return None;
    };
    match (&left.kind, &right.kind) {
        (TypedExprKind::ColumnRef { name, .. }, TypedExprKind::Literal(value)) => {
            Some((name.as_str(), value.clone()))
        }
        (TypedExprKind::Literal(value), TypedExprKind::ColumnRef { name, .. }) => {
            Some((name.as_str(), value.clone()))
        }
        _ => None,
    }
}

/// Detect a `column_ref CMP literal` predicate and return it as a
/// `(column_name, lower_bound, upper_bound)` triple. Only matches
/// when the operator is one of `<`, `<=`, `>`, `>=`, or
/// `BETWEEN lo AND hi` over a single `ColumnRef` and one or two
/// literals. Used by `apply_match_filter_index_hints` to drive the
/// per-node range pushdown that `scan_node_candidates` then sends
/// through `scan_table_multi_range_filter`.
pub(in crate::executor) fn extract_column_literal_range(
    expr: &TypedExpr,
) -> Option<(&str, std::ops::Bound<Value>, std::ops::Bound<Value>)> {
    use std::ops::Bound;
    pub(in crate::executor) fn lit(expr: &TypedExpr) -> Option<&Value> {
        match &expr.kind {
            TypedExprKind::Literal(v) => Some(v),
            _ => None,
        }
    }
    pub(in crate::executor) fn col(expr: &TypedExpr) -> Option<&str> {
        match &expr.kind {
            TypedExprKind::ColumnRef { name, .. } => Some(name),
            _ => None,
        }
    }
    match &expr.kind {
        TypedExprKind::BinaryLt { left, right } => {
            if let (Some(c), Some(v)) = (col(left), lit(right)) {
                return Some((c, Bound::Unbounded, Bound::Excluded(v.clone())));
            }
            if let (Some(v), Some(c)) = (lit(left), col(right)) {
                return Some((c, Bound::Excluded(v.clone()), Bound::Unbounded));
            }
            None
        }
        TypedExprKind::BinaryLe { left, right } => {
            if let (Some(c), Some(v)) = (col(left), lit(right)) {
                return Some((c, Bound::Unbounded, Bound::Included(v.clone())));
            }
            if let (Some(v), Some(c)) = (lit(left), col(right)) {
                return Some((c, Bound::Included(v.clone()), Bound::Unbounded));
            }
            None
        }
        TypedExprKind::BinaryGt { left, right } => {
            if let (Some(c), Some(v)) = (col(left), lit(right)) {
                return Some((c, Bound::Excluded(v.clone()), Bound::Unbounded));
            }
            if let (Some(v), Some(c)) = (lit(left), col(right)) {
                return Some((c, Bound::Unbounded, Bound::Excluded(v.clone())));
            }
            None
        }
        TypedExprKind::BinaryGe { left, right } => {
            if let (Some(c), Some(v)) = (col(left), lit(right)) {
                return Some((c, Bound::Included(v.clone()), Bound::Unbounded));
            }
            if let (Some(v), Some(c)) = (lit(left), col(right)) {
                return Some((c, Bound::Unbounded, Bound::Included(v.clone())));
            }
            None
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            let c = col(expr)?;
            let lo = lit(low)?;
            let hi = lit(high)?;
            Some((c, Bound::Included(lo.clone()), Bound::Included(hi.clone())))
        }
        _ => None,
    }
}

pub(in crate::executor) fn exact_variable_column_literal_range(
    expr: &TypedExpr,
    expected_variable: &str,
) -> Option<(String, std::ops::Bound<Value>, std::ops::Bound<Value>)> {
    let (name, lower, upper) = extract_column_literal_range(expr)?;
    let (variable, column) = name.split_once('.')?;
    variable
        .eq_ignore_ascii_case(expected_variable)
        .then_some((column.to_owned(), lower, upper))
}

pub(in crate::executor) fn exact_named_column_literal_equality(expr: &TypedExpr, expected_name: &str) -> Option<Value> {
    let (name, value) = exact_column_literal_equality(expr)?;
    name.eq_ignore_ascii_case(expected_name).then_some(value)
}

pub(in crate::executor) fn exact_variable_column_literal_equality(
    expr: &TypedExpr,
    expected_variable: &str,
) -> Option<(String, Value)> {
    let (name, value) = exact_column_literal_equality(expr)?;
    let (variable, column) = name.split_once('.')?;
    variable
        .eq_ignore_ascii_case(expected_variable)
        .then_some((column.to_owned(), value))
}

pub(in crate::executor) fn exact_variable_column_literal_gt(
    expr: &TypedExpr,
    expected_variable: &str,
) -> Option<(String, Value)> {
    let TypedExprKind::BinaryGt { left, right } = &expr.kind else {
        return None;
    };
    let TypedExprKind::ColumnRef { name, .. } = &left.kind else {
        return None;
    };
    let (variable, column) = name.split_once('.')?;
    if !variable.eq_ignore_ascii_case(expected_variable) {
        return None;
    }
    let TypedExprKind::Literal(value) = &right.kind else {
        return None;
    };
    Some((column.to_owned(), value.clone()))
}

pub(in crate::executor) fn exact_named_column_literal_gt(expr: &TypedExpr, expected_name: &str) -> Option<Value> {
    let TypedExprKind::BinaryGt { left, right } = &expr.kind else {
        return None;
    };
    let TypedExprKind::ColumnRef { name, .. } = &left.kind else {
        return None;
    };
    if !name.eq_ignore_ascii_case(expected_name) {
        return None;
    }
    let TypedExprKind::Literal(value) = &right.kind else {
        return None;
    };
    Some(value.clone())
}

pub(in crate::executor) fn is_column_column_inequality(expr: &TypedExpr, left_name: &str, right_name: &str) -> bool {
    let TypedExprKind::BinaryNe { left, right } = &expr.kind else {
        return false;
    };
    let (TypedExprKind::ColumnRef { name: left, .. }, TypedExprKind::ColumnRef { name: right, .. }) =
        (&left.kind, &right.kind)
    else {
        return false;
    };
    (left.eq_ignore_ascii_case(left_name) && right.eq_ignore_ascii_case(right_name))
        || (left.eq_ignore_ascii_case(right_name) && right.eq_ignore_ascii_case(left_name))
}

pub(in crate::executor) fn graph_filter_node_id_inequality_peers(
    filter_conjuncts: &[GraphFilterConjunct<'_>],
    next_variable: &str,
) -> Vec<String> {
    let mut peers = Vec::new();
    let expected = format!("{next_variable}.id");
    for conjunct in filter_conjuncts {
        let TypedExprKind::BinaryNe { left, right } = &conjunct.expr.kind else {
            continue;
        };
        let (
            TypedExprKind::ColumnRef { name: left, .. },
            TypedExprKind::ColumnRef { name: right, .. },
        ) = (&left.kind, &right.kind)
        else {
            continue;
        };
        let push_peer = |candidate: &str, peers: &mut Vec<String>| {
            let Some((variable, property)) = candidate.split_once('.') else {
                return;
            };
            if property.eq_ignore_ascii_case("id")
                && !variable.eq_ignore_ascii_case(next_variable)
                && !peers
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(variable))
            {
                peers.push(variable.to_owned());
            }
        };
        if left.eq_ignore_ascii_case(&expected) {
            push_peer(right, &mut peers);
        } else if right.eq_ignore_ascii_case(&expected) {
            push_peer(left, &mut peers);
        }
    }
    peers
}

pub(in crate::executor) fn extract_hybrid_deep_graph_vector_filter(
    filter: &TypedExpr,
    start_properties: &[CypherPropertyExpr],
    start_variable: &str,
    friend_variable: &str,
    target_variable: &str,
) -> Option<HybridDeepGraphVectorFilter> {
    if !properties_only_use_keys(start_properties, &["id"]) {
        return None;
    }

    let mut conjuncts = Vec::new();
    collect_graph_filter_conjuncts(filter, &mut conjuncts);

    let mut start_id = inline_property_literal(start_properties, "id");
    let mut query_vector = None;
    let mut distance_threshold = None;
    let mut popularity_threshold = None;

    for conjunct in conjuncts {
        if let Some(value) =
            exact_named_column_literal_equality(conjunct, &format!("{start_variable}.id"))
        {
            if !reconcile_literal_slot(&mut start_id, value) {
                return None;
            }
            continue;
        }

        if let Some(value) =
            exact_named_column_literal_gt(conjunct, &format!("{target_variable}.popularity"))
        {
            popularity_threshold = Some(value);
            continue;
        }

        if let Some((vector, threshold)) = extract_l2_distance_threshold(conjunct, target_variable)
        {
            query_vector = Some(vector);
            distance_threshold = Some(threshold);
            continue;
        }

        if is_column_column_equality(
            conjunct,
            &format!("{target_variable}.tenant_id"),
            &format!("{start_variable}.tenant_id"),
        ) || is_column_column_equality(
            conjunct,
            &format!("{friend_variable}.tenant_id"),
            &format!("{start_variable}.tenant_id"),
        ) {
            continue;
        }

        return None;
    }

    Some(HybridDeepGraphVectorFilter {
        start_id: start_id?,
        // See [`HybridGraphVectorFilter::query_vector`].
        query_vector: query_vector?.into_iter().map(|v| v as f32).collect(),
        distance_threshold: distance_threshold?,
        popularity_threshold: popularity_threshold?,
    })
}

pub(in crate::executor) fn is_column_column_equality(expr: &TypedExpr, left_name: &str, right_name: &str) -> bool {
    let TypedExprKind::BinaryEq { left, right } = &expr.kind else {
        return false;
    };
    let (TypedExprKind::ColumnRef { name: left, .. }, TypedExprKind::ColumnRef { name: right, .. }) =
        (&left.kind, &right.kind)
    else {
        return false;
    };
    (left.eq_ignore_ascii_case(left_name) && right.eq_ignore_ascii_case(right_name))
        || (left.eq_ignore_ascii_case(right_name) && right.eq_ignore_ascii_case(left_name))
}

pub(in crate::executor) fn extract_l2_distance_threshold(
    expr: &TypedExpr,
    target_variable: &str,
) -> Option<(Vec<f64>, f64)> {
    let TypedExprKind::BinaryLt { left, right } = &expr.kind else {
        return None;
    };
    let threshold = literal_f64(right)?;
    let TypedExprKind::ScalarFunction { func, args } = &left.kind else {
        return None;
    };
    if !matches!(func, ScalarFunction::L2Distance)
        && !matches!(func, ScalarFunction::Generic(name) if name.eq_ignore_ascii_case("l2_distance"))
    {
        return None;
    }
    let [left_arg, right_arg] = args.as_slice() else {
        return None;
    };
    let TypedExprKind::ColumnRef { name, .. } = &left_arg.kind else {
        return None;
    };
    if !name.eq_ignore_ascii_case(&format!("{target_variable}.embedding")) {
        return None;
    }
    let TypedExprKind::Literal(value) = &right_arg.kind else {
        return None;
    };
    Some((literal_vector_f64(value)?, threshold))
}

pub(in crate::executor) fn is_l2_distance_expr_or_alias(expr: &TypedExpr, target_variable: &str, alias: &str) -> bool {
    column_ref_name(expr).is_some_and(|name| name.eq_ignore_ascii_case(alias))
        || is_l2_distance_expr_for_variable(expr, target_variable)
}

pub(in crate::executor) fn is_l2_distance_expr_for_variable(expr: &TypedExpr, target_variable: &str) -> bool {
    let TypedExprKind::ScalarFunction { func, args } = &expr.kind else {
        return false;
    };
    if !matches!(func, ScalarFunction::L2Distance)
        && !matches!(func, ScalarFunction::Generic(name) if name.eq_ignore_ascii_case("l2_distance"))
    {
        return false;
    }
    let [left_arg, right_arg] = args.as_slice() else {
        return false;
    };
    let TypedExprKind::ColumnRef { name, .. } = &left_arg.kind else {
        return false;
    };
    if !name.eq_ignore_ascii_case(&format!("{target_variable}.embedding")) {
        return false;
    }
    matches!(right_arg.kind, TypedExprKind::Literal(_))
}

pub(in crate::executor) fn literal_vector_f64(value: &Value) -> Option<Vec<f64>> {
    let vector = match value {
        Value::Vector(vector) => vector
            .values
            .iter()
            .map(|value| f64::from(*value))
            .collect(),
        Value::Text(text) => parse_vector_text_literal(text)?,
        Value::Array(values) => {
            let mut parsed = Vec::with_capacity(values.len());
            for value in values {
                parsed.push(value_to_f64(value)?);
            }
            parsed
        }
        _ => return None,
    };
    Some(vector)
}

pub(in crate::executor) fn parse_vector_text_literal(text: &str) -> Option<Vec<f64>> {
    let trimmed = text.trim();
    let inner = trimmed.strip_prefix('[')?.strip_suffix(']')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    inner
        .split(',')
        .map(|part| part.trim().parse::<f64>().ok())
        .collect()
}

pub(in crate::executor) fn literal_f64(expr: &TypedExpr) -> Option<f64> {
    match &expr.kind {
        TypedExprKind::Literal(value) => value_to_f64(value),
        _ => None,
    }
}

pub(in crate::executor) fn value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int(value) => Some(f64::from(*value)),
        Value::BigInt(value) => Some(i64_to_f64(*value)),
        Value::Real(value) => Some(f64::from(*value)),
        Value::Double(value) => Some(*value),
        _ => None,
    }
}

pub(in crate::executor) fn literal_i64(expr: &TypedExpr) -> Option<i64> {
    match &expr.kind {
        TypedExprKind::Literal(Value::Int(value)) => Some(i64::from(*value)),
        TypedExprKind::Literal(Value::BigInt(value)) => Some(*value),
        _ => None,
    }
}

pub(in crate::executor) fn normalize_int_key(value: &mut Value) {
    if let Value::BigInt(raw) = value {
        if let Ok(int_value) = i32::try_from(*raw) {
            *value = Value::Int(int_value);
        }
    }
}

pub(in crate::executor) fn column_ref_name(expr: &TypedExpr) -> Option<&str> {
    match &expr.kind {
        TypedExprKind::ColumnRef { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

pub(in crate::executor) fn node_has_filter_constraints(node: &CypherNodePattern) -> bool {
    !node.properties.is_empty() || node.index_scan.is_some() || !node.range_pushdown.is_empty()
}

pub(in crate::executor) fn ascending_order_by_matches_column(order_by: &[SortExpr], expected: &str) -> bool {
    order_by
        .iter()
        .all(|sort| !sort.descending && column_ref_name(&sort.expr) == Some(expected))
}

pub(in crate::executor) fn count_return_variable(expr: &TypedExpr) -> Option<&str> {
    let TypedExprKind::AggCount {
        expr: Some(expr),
        distinct: false,
        filter: None,
    } = &expr.kind
    else {
        return None;
    };
    column_ref_name(expr)
}

pub(in crate::executor) fn is_count_star(expr: &TypedExpr) -> bool {
    matches!(
        &expr.kind,
        TypedExprKind::AggCount {
            expr: None,
            distinct: false,
            filter: None,
        }
    )
}

pub(in crate::executor) fn count_distinct_id_return_variable(expr: &TypedExpr) -> Option<&str> {
    let TypedExprKind::AggCount {
        expr: Some(expr),
        distinct: true,
        filter: None,
    } = &expr.kind
    else {
        return None;
    };
    let name = column_ref_name(expr)?;
    let (variable, property) = name.rsplit_once('.')?;
    property.eq_ignore_ascii_case("id").then_some(variable)
}
