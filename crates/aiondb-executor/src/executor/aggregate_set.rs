use super::aggregate_helpers::*;
use super::dml_plans::best_eq_lookup_index;
use super::join_plans::{build_hash_join_key, JoinFxBuildHasher, JoinHashKey};
use super::*;

// Aggregate / GROUP BY fast-path executors, continuation of `impl Executor`
// below. Helper types/fns in this module are visible to the submodule as
// its descendant.
mod fast_paths;

#[inline]
fn evaluate_post_aggregate_sort_expr_fast(
    executor: &Executor,
    expr: &TypedExpr,
    agg_row: &Row,
    all_projections: &[AggregateExprRef<'_>],
    context: &ExecutionContext,
) -> DbResult<Value> {
    executor.evaluate_having_expr_extended(expr, agg_row, all_projections, context)
}

fn single_aggregate_row_result(
    columns: Vec<aiondb_plan::ResultField>,
    row: Row,
    context: &ExecutionContext,
) -> DbResult<ExecutionResult> {
    let _result_bytes = ensure_result_bytes_fit_and_track_query_row(context, &row, 0)?;
    Ok(ExecutionResult::Query {
        columns,
        rows: vec![row],
    })
}

fn can_use_visible_count_fast_path(
    group_by: &[TypedExpr],
    grouping_sets: &[Vec<usize>],
    aggregates: &[ProjectionExpr],
    having: Option<&TypedExpr>,
    filter: Option<&TypedExpr>,
    order_by: &[SortExpr],
    distinct: bool,
    access_path: &ScanAccessPath,
) -> bool {
    group_by.is_empty()
        && grouping_sets.is_empty()
        && having.is_none()
        && filter.is_none()
        && order_by.is_empty()
        && !distinct
        && matches!(access_path, ScanAccessPath::SeqScan)
        && !aggregates.is_empty()
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

fn aggregate_stream_group_scan_supported(
    executor: &Executor,
    context: &ExecutionContext,
    table_id: RelationId,
    access_path: &ScanAccessPath,
    group_ordinal: usize,
) -> DbResult<bool> {
    let (index_id, equality_prefix_len) = match access_path {
        ScanAccessPath::IndexEq { index_id, .. } => (*index_id, 1usize),
        ScanAccessPath::IndexEqComposite { index_id, values } => (*index_id, values.len()),
        ScanAccessPath::IndexEqRangeComposite {
            index_id,
            eq_values,
            ..
        } => (*index_id, eq_values.len()),
        ScanAccessPath::IndexRange { index_id, .. } => (*index_id, 0usize),
        _ => return Ok(false),
    };
    let Some(table) = executor
        .catalog_reader
        .get_table_by_id(context.txn_id, table_id)?
    else {
        return Ok(false);
    };
    let Some(group_column) = table.columns.get(group_ordinal) else {
        return Ok(false);
    };
    let Some(index) = executor
        .catalog_reader
        .get_index(context.txn_id, index_id)?
    else {
        return Ok(false);
    };
    if index.kind != aiondb_catalog::IndexKind::BTree {
        return Ok(false);
    }
    let Some(key_column) = index.key_columns.get(equality_prefix_len) else {
        return Ok(false);
    };
    Ok(key_column.column_id == group_column.column_id)
}

#[derive(Clone, Copy)]
enum SimpleGroupFilterOp {
    Eq,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Clone)]
pub(in crate::executor) struct SimpleGroupFilter {
    column_ordinal: usize,
    op: SimpleGroupFilterOp,
    literal: Value,
}

#[derive(Clone, Copy)]
pub(in crate::executor) enum SimpleGroupOutput {
    GroupKey { group_index: usize },
    CountStar,
    CountDistinct { projected_pos: usize },
    Sum { projected_pos: usize },
    Avg { projected_pos: usize },
    Min { projected_pos: usize },
    Max { projected_pos: usize },
}

struct SimpleGroupState {
    group_values: Vec<Value>,
    counts: Vec<i64>,
    /// Reused for SUM/AVG accumulators AND for MIN/MAX running
    /// extremum value. The semantic is determined by the matching
    /// `SimpleGroupOutput` variant at the same index.
    sums: Vec<Option<Value>>,
    distincts: Vec<Option<std::collections::HashSet<ValueHashKey>>>,
}

impl SimpleGroupState {
    fn new(group_values: Vec<Value>, output_count: usize) -> Self {
        Self {
            group_values,
            counts: vec![0; output_count],
            sums: vec![None; output_count],
            distincts: vec![None; output_count],
        }
    }
}

fn simple_column_ordinal(expr: &TypedExpr) -> Option<usize> {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => Some(*ordinal),
        TypedExprKind::Cast { expr, .. } => simple_column_ordinal(expr),
        _ => None,
    }
}

fn simple_literal(expr: &TypedExpr) -> Option<Value> {
    match &expr.kind {
        TypedExprKind::Literal(value) => Some(value.clone()),
        TypedExprKind::Cast { expr, .. } => simple_literal(expr),
        _ => None,
    }
}

fn invert_simple_group_filter_op(op: SimpleGroupFilterOp) -> SimpleGroupFilterOp {
    match op {
        SimpleGroupFilterOp::Eq => SimpleGroupFilterOp::Eq,
        SimpleGroupFilterOp::Gt => SimpleGroupFilterOp::Lt,
        SimpleGroupFilterOp::Ge => SimpleGroupFilterOp::Le,
        SimpleGroupFilterOp::Lt => SimpleGroupFilterOp::Gt,
        SimpleGroupFilterOp::Le => SimpleGroupFilterOp::Ge,
    }
}

#[allow(clippy::option_option)]
fn extract_simple_group_filter(filter: Option<&TypedExpr>) -> Option<Option<SimpleGroupFilter>> {
    let Some(filter) = filter else {
        return Some(None);
    };
    let (left, right, op) = match &filter.kind {
        TypedExprKind::BinaryEq { left, right } => {
            (left.as_ref(), right.as_ref(), SimpleGroupFilterOp::Eq)
        }
        TypedExprKind::BinaryGt { left, right } => {
            (left.as_ref(), right.as_ref(), SimpleGroupFilterOp::Gt)
        }
        TypedExprKind::BinaryGe { left, right } => {
            (left.as_ref(), right.as_ref(), SimpleGroupFilterOp::Ge)
        }
        TypedExprKind::BinaryLt { left, right } => {
            (left.as_ref(), right.as_ref(), SimpleGroupFilterOp::Lt)
        }
        TypedExprKind::BinaryLe { left, right } => {
            (left.as_ref(), right.as_ref(), SimpleGroupFilterOp::Le)
        }
        TypedExprKind::Cast { expr, .. } => return extract_simple_group_filter(Some(expr)),
        _ => return None,
    };

    if let (Some(column_ordinal), Some(literal)) =
        (simple_column_ordinal(left), simple_literal(right))
    {
        return Some(Some(SimpleGroupFilter {
            column_ordinal,
            op,
            literal,
        }));
    }
    if let (Some(literal), Some(column_ordinal)) =
        (simple_literal(left), simple_column_ordinal(right))
    {
        return Some(Some(SimpleGroupFilter {
            column_ordinal,
            op: invert_simple_group_filter_op(op),
            literal,
        }));
    }
    None
}

fn simple_group_filter_matches(value: &Value, filter: &SimpleGroupFilter) -> DbResult<bool> {
    if matches!(value, Value::Null) || matches!(filter.literal, Value::Null) {
        return Ok(false);
    }
    let Some(ordering) = compare_runtime_values(value, &filter.literal)? else {
        return Ok(false);
    };
    Ok(match filter.op {
        SimpleGroupFilterOp::Eq => ordering == Ordering::Equal,
        SimpleGroupFilterOp::Gt => ordering == Ordering::Greater,
        SimpleGroupFilterOp::Ge => ordering != Ordering::Less,
        SimpleGroupFilterOp::Lt => ordering == Ordering::Less,
        SimpleGroupFilterOp::Le => ordering != Ordering::Greater,
    })
}

fn projected_position(required_ordinals: &[usize], ordinal: usize) -> Option<usize> {
    required_ordinals
        .iter()
        .position(|candidate| *candidate == ordinal)
}

fn simple_group_order_column_indices(
    aggregates: &[ProjectionExpr],
    order_by: &[SortExpr],
) -> Option<Vec<Option<usize>>> {
    order_by
        .iter()
        .map(|sort| {
            // Always resolve via structural equality against the
            // projected aggregates list. The previous shortcut
            // `ColumnRef { ordinal } if ordinal < aggregates.len()`
            // wrongly treated the source-table-relative ordinal of
            // the ORDER BY column as an output position — e.g.
            // `SELECT dept, count(*), … ORDER BY dept` binds `dept`
            // with the source ordinal 2 (id, name, dept, …), but
            // the projection slot is 0. Falling back to the
            // structural search is what produces the correct sort
            // index for the GROUP BY key column.
            aggregates
                .iter()
                .position(|projection| exprs_structurally_equal(&projection.expr, &sort.expr))
                .map(Some)
        })
        .collect()
}

fn project_table_outputs_are_identity(outputs: &[ProjectionExpr]) -> bool {
    !outputs.is_empty()
        && outputs
            .iter()
            .enumerate()
            .all(|(index, output)| simple_column_ordinal(&output.expr) == Some(index))
}

fn count_star_outputs(aggregates: &[ProjectionExpr]) -> bool {
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

fn simple_scan_output_column(
    plan: &PhysicalPlan,
    output_ordinal: usize,
) -> Option<(RelationId, usize)> {
    match plan {
        PhysicalPlan::SeqScan { table_id } => Some((*table_id, output_ordinal)),
        PhysicalPlan::ProjectTable {
            table_id,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
            access_path,
        }
        | PhysicalPlan::LockingProjectTable {
            table_id,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
            access_path,
            ..
        } => {
            if filter.is_some()
                || !order_by.is_empty()
                || limit.is_some()
                || offset.is_some()
                || *distinct
                || !distinct_on.is_empty()
                || !matches!(access_path, ScanAccessPath::SeqScan)
            {
                return None;
            }
            if outputs.is_empty() {
                return Some((*table_id, output_ordinal));
            }
            let projection = outputs.get(output_ordinal)?;
            let TypedExprKind::ColumnRef { ordinal, .. } = &projection.expr.kind else {
                return None;
            };
            Some((*table_id, *ordinal))
        }
        _ => None,
    }
}

fn group_projection_output_ordinal(
    aggregates: &[ProjectionExpr],
    group_source_ordinal: usize,
    raw_group_ordinal: usize,
) -> Option<usize> {
    aggregates.iter().position(|projection| {
        matches!(
            &projection.expr.kind,
            TypedExprKind::ColumnRef { ordinal, .. }
                if *ordinal == group_source_ordinal || *ordinal == raw_group_ordinal
        )
    })
}

struct AggregateSimpleEqLiteralFilter {
    column_ordinal: usize,
    literal: Value,
}

struct AggregateEqAndRangeLiteralFilter {
    eq_column_ordinal: usize,
    eq_literal: Value,
    range_column_ordinal: usize,
    low_literal: Value,
    high_literal: Value,
}

fn strip_aggregate_cast_wrappers(expr: &TypedExpr) -> &TypedExpr {
    let mut current = expr;
    while let TypedExprKind::Cast { expr, .. } = &current.kind {
        current = expr;
    }
    current
}

fn extract_aggregate_simple_eq_literal_filter(
    filter: &TypedExpr,
) -> Option<AggregateSimpleEqLiteralFilter> {
    let filter = strip_aggregate_cast_wrappers(filter);
    let TypedExprKind::BinaryEq { left, right } = &filter.kind else {
        return None;
    };
    let left = strip_aggregate_cast_wrappers(left);
    let right = strip_aggregate_cast_wrappers(right);
    match (&left.kind, &right.kind) {
        (TypedExprKind::ColumnRef { ordinal, .. }, TypedExprKind::Literal(literal))
        | (TypedExprKind::Literal(literal), TypedExprKind::ColumnRef { ordinal, .. }) => {
            Some(AggregateSimpleEqLiteralFilter {
                column_ordinal: *ordinal,
                literal: literal.clone(),
            })
        }
        _ => None,
    }
}

/// Detect a `col IN (lit1, lit2, ..., litN)` predicate where every
/// list element is a literal. Returns the column ordinal and the list
/// values so the count fast-path can sum per-value index counts.
fn extract_aggregate_in_literal_filter(filter: &TypedExpr) -> Option<(usize, Vec<Value>)> {
    let filter = strip_aggregate_cast_wrappers(filter);
    let TypedExprKind::InList {
        expr,
        list,
        negated: false,
    } = &filter.kind
    else {
        return None;
    };
    let expr = strip_aggregate_cast_wrappers(expr);
    let TypedExprKind::ColumnRef { ordinal, .. } = &expr.kind else {
        return None;
    };
    let mut literals = Vec::with_capacity(list.len());
    for element in list {
        let element = strip_aggregate_cast_wrappers(element);
        let TypedExprKind::Literal(literal) = &element.kind else {
            return None;
        };
        literals.push(literal.clone());
    }
    if literals.is_empty() {
        return None;
    }
    Some((*ordinal, literals))
}

fn extract_aggregate_between_literal_filter(filter: &TypedExpr) -> Option<(usize, Value, Value)> {
    let filter = strip_aggregate_cast_wrappers(filter);
    let TypedExprKind::Between {
        expr,
        low,
        high,
        negated: false,
    } = &filter.kind
    else {
        return None;
    };
    let expr = strip_aggregate_cast_wrappers(expr);
    let low = strip_aggregate_cast_wrappers(low);
    let high = strip_aggregate_cast_wrappers(high);
    match (&expr.kind, &low.kind, &high.kind) {
        (
            TypedExprKind::ColumnRef { ordinal, .. },
            TypedExprKind::Literal(low),
            TypedExprKind::Literal(high),
        ) => Some((*ordinal, low.clone(), high.clone())),
        _ => None,
    }
}

/// Detect a `col CMP literal [AND col CMP literal2 ...]` predicate as a
/// single column-vs-literal range, returned as standard `Bound` pair so
/// the storage range pushdown can apply the comparison once per row.
/// Mirrors the projection-side `extract_simple_range_literal_filter`
/// but routed through the aggregate path's cast-stripping helper.
fn aggregate_simple_range_literal_filter(
    filter: &TypedExpr,
) -> Option<(usize, std::ops::Bound<Value>, std::ops::Bound<Value>)> {
    fn add(
        filter: &TypedExpr,
        column_ordinal: &mut Option<usize>,
        lower: &mut Option<std::ops::Bound<Value>>,
        upper: &mut Option<std::ops::Bound<Value>>,
    ) -> Option<()> {
        let filter = strip_aggregate_cast_wrappers(filter);
        if let TypedExprKind::LogicalAnd { left, right } = &filter.kind {
            add(left, column_ordinal, lower, upper)?;
            add(right, column_ordinal, lower, upper)?;
            return Some(());
        }
        if let TypedExprKind::Between {
            expr,
            low,
            high,
            negated: false,
        } = &filter.kind
        {
            let expr = strip_aggregate_cast_wrappers(expr);
            let low = strip_aggregate_cast_wrappers(low);
            let high = strip_aggregate_cast_wrappers(high);
            let TypedExprKind::ColumnRef { ordinal, .. } = &expr.kind else {
                return None;
            };
            let TypedExprKind::Literal(low_v) = &low.kind else {
                return None;
            };
            let TypedExprKind::Literal(high_v) = &high.kind else {
                return None;
            };
            if matches!(low_v, Value::Null) || matches!(high_v, Value::Null) {
                return None;
            }
            if let Some(existing) = *column_ordinal {
                if existing != *ordinal {
                    return None;
                }
            } else {
                *column_ordinal = Some(*ordinal);
            }
            if lower.is_some() || upper.is_some() {
                return None;
            }
            *lower = Some(std::ops::Bound::Included(low_v.clone()));
            *upper = Some(std::ops::Bound::Included(high_v.clone()));
            return Some(());
        }
        let (col_expr, lit_expr, col_on_left, inclusive, is_lower_when_col_left) =
            match &filter.kind {
                TypedExprKind::BinaryGe { left, right } => (left, right, true, true, true),
                TypedExprKind::BinaryGt { left, right } => (left, right, true, false, true),
                TypedExprKind::BinaryLe { left, right } => (left, right, true, true, false),
                TypedExprKind::BinaryLt { left, right } => (left, right, true, false, false),
                _ => return None,
            };
        let _ = col_on_left;
        let col_expr = strip_aggregate_cast_wrappers(col_expr);
        let lit_expr = strip_aggregate_cast_wrappers(lit_expr);
        let (col_ord, literal, is_lower) = match (&col_expr.kind, &lit_expr.kind) {
            (TypedExprKind::ColumnRef { ordinal, .. }, TypedExprKind::Literal(value)) => {
                (*ordinal, value.clone(), is_lower_when_col_left)
            }
            (TypedExprKind::Literal(value), TypedExprKind::ColumnRef { ordinal, .. }) => {
                // Column on the right: invert the comparison side.
                (*ordinal, value.clone(), !is_lower_when_col_left)
            }
            _ => return None,
        };
        if matches!(literal, Value::Null) {
            return None;
        }
        if let Some(existing) = *column_ordinal {
            if existing != col_ord {
                return None;
            }
        } else {
            *column_ordinal = Some(col_ord);
        }
        let bound = if inclusive {
            std::ops::Bound::Included(literal)
        } else {
            std::ops::Bound::Excluded(literal)
        };
        if is_lower {
            if lower.is_some() {
                return None;
            }
            *lower = Some(bound);
        } else {
            if upper.is_some() {
                return None;
            }
            *upper = Some(bound);
        }
        Some(())
    }

    let mut column_ordinal = None;
    let mut lower = None;
    let mut upper = None;
    add(filter, &mut column_ordinal, &mut lower, &mut upper)?;
    let column_ordinal = column_ordinal?;
    let lower = lower.unwrap_or(std::ops::Bound::Unbounded);
    let upper = upper.unwrap_or(std::ops::Bound::Unbounded);
    if matches!(lower, std::ops::Bound::Unbounded) && matches!(upper, std::ops::Bound::Unbounded) {
        return None;
    }
    Some((column_ordinal, lower, upper))
}

/// AND-of-ranges over distinct columns — aggregate path version. See
/// `extract_multi_range_literal_filter` in projection_plans for the
/// projection-side counterpart.
fn aggregate_multi_range_literal_filter(
    filter: &TypedExpr,
) -> Option<Vec<(usize, std::ops::Bound<Value>, std::ops::Bound<Value>)>> {
    use std::collections::HashMap;
    let mut by_col: HashMap<usize, (std::ops::Bound<Value>, std::ops::Bound<Value>)> =
        HashMap::new();
    fn walk(
        filter: &TypedExpr,
        by_col: &mut HashMap<usize, (std::ops::Bound<Value>, std::ops::Bound<Value>)>,
    ) -> Option<()> {
        let filter = strip_aggregate_cast_wrappers(filter);
        if let TypedExprKind::LogicalAnd { left, right } = &filter.kind {
            walk(left, by_col)?;
            walk(right, by_col)?;
            return Some(());
        }
        if let TypedExprKind::BinaryEq { left, right } = &filter.kind {
            let l = strip_aggregate_cast_wrappers(left);
            let r = strip_aggregate_cast_wrappers(right);
            let (ord, lit) = match (&l.kind, &r.kind) {
                (TypedExprKind::ColumnRef { ordinal, .. }, TypedExprKind::Literal(v))
                | (TypedExprKind::Literal(v), TypedExprKind::ColumnRef { ordinal, .. }) => {
                    (*ordinal, v.clone())
                }
                _ => return None,
            };
            if matches!(lit, Value::Null) {
                return None;
            }
            let entry = by_col
                .entry(ord)
                .or_insert((std::ops::Bound::Unbounded, std::ops::Bound::Unbounded));
            if !matches!(entry.0, std::ops::Bound::Unbounded)
                || !matches!(entry.1, std::ops::Bound::Unbounded)
            {
                return None;
            }
            entry.0 = std::ops::Bound::Included(lit.clone());
            entry.1 = std::ops::Bound::Included(lit);
            return Some(());
        }
        let leaf = aggregate_simple_range_literal_filter(filter)?;
        let (ord, lo, hi) = leaf;
        let entry = by_col
            .entry(ord)
            .or_insert((std::ops::Bound::Unbounded, std::ops::Bound::Unbounded));
        if !matches!(lo, std::ops::Bound::Unbounded) {
            entry.0 = match (&entry.0, &lo) {
                (std::ops::Bound::Unbounded, _) => lo,
                _ => return None,
            };
        }
        if !matches!(hi, std::ops::Bound::Unbounded) {
            entry.1 = match (&entry.1, &hi) {
                (std::ops::Bound::Unbounded, _) => hi,
                _ => return None,
            };
        }
        Some(())
    }
    walk(filter, &mut by_col)?;
    if by_col.is_empty() {
        return None;
    }
    let mut filters: Vec<_> = by_col
        .into_iter()
        .map(|(ord, (lo, hi))| (ord, lo, hi))
        .collect();
    filters.sort_by_key(|(ord, _, _)| *ord);
    Some(filters)
}

/// Same storage-safety guard as the projection-side range pushdown.
/// Mirrors `cmp_value_for_range` in the storage layer.
fn aggregate_range_bound_storage_safe(
    lower: &std::ops::Bound<Value>,
    upper: &std::ops::Bound<Value>,
) -> bool {
    fn bound_safe(bound: &std::ops::Bound<Value>) -> bool {
        match bound {
            std::ops::Bound::Unbounded => true,
            std::ops::Bound::Included(value) | std::ops::Bound::Excluded(value) => matches!(
                value,
                Value::Int(_)
                    | Value::BigInt(_)
                    | Value::Real(_)
                    | Value::Double(_)
                    | Value::Numeric(_)
                    | Value::Money(_)
                    | Value::Text(_)
                    | Value::Blob(_)
                    | Value::Boolean(_)
                    | Value::Date(_)
                    | Value::LargeDate(_)
                    | Value::Time(_)
                    | Value::Timestamp(_)
                    | Value::TimestampTz(_)
                    | Value::Uuid(_)
            ),
        }
    }
    bound_safe(lower) && bound_safe(upper)
}

fn collect_aggregate_and_conjuncts<'a>(expr: &'a TypedExpr, out: &mut Vec<&'a TypedExpr>) {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        let expr = strip_aggregate_cast_wrappers(expr);
        if let TypedExprKind::LogicalAnd { left, right } = &expr.kind {
            stack.push(right);
            stack.push(left);
        } else {
            out.push(expr);
        }
    }
}

fn extract_aggregate_inclusive_range_comparison(
    filter: &TypedExpr,
) -> Option<(usize, Option<Value>, Option<Value>)> {
    let filter = strip_aggregate_cast_wrappers(filter);
    let (left, right, column_on_left, is_lower) = match &filter.kind {
        TypedExprKind::BinaryGe { left, right } => (left, right, true, true),
        TypedExprKind::BinaryLe { left, right } => (left, right, true, false),
        _ => return None,
    };
    let left = strip_aggregate_cast_wrappers(left);
    let right = strip_aggregate_cast_wrappers(right);
    match (&left.kind, &right.kind, column_on_left, is_lower) {
        (TypedExprKind::ColumnRef { ordinal, .. }, TypedExprKind::Literal(literal), true, true) => {
            Some((*ordinal, Some(literal.clone()), None))
        }
        (
            TypedExprKind::ColumnRef { ordinal, .. },
            TypedExprKind::Literal(literal),
            true,
            false,
        ) => Some((*ordinal, None, Some(literal.clone()))),
        _ => None,
    }
}

fn extract_aggregate_eq_and_range_literal_filter(
    filter: &TypedExpr,
) -> Option<AggregateEqAndRangeLiteralFilter> {
    let mut conjuncts = Vec::new();
    collect_aggregate_and_conjuncts(filter, &mut conjuncts);

    let mut eq = None;
    let mut range = None;
    let mut range_column = None;
    let mut lower = None;
    let mut upper = None;

    for conjunct in conjuncts {
        if eq.is_none() {
            eq = extract_aggregate_simple_eq_literal_filter(conjunct);
            if eq.is_some() {
                continue;
            }
        }
        if range.is_none() {
            range = extract_aggregate_between_literal_filter(conjunct);
            if range.is_some() {
                continue;
            }
        }
        if let Some((ordinal, maybe_lower, maybe_upper)) =
            extract_aggregate_inclusive_range_comparison(conjunct)
        {
            if range_column.map_or(true, |existing| existing == ordinal) {
                range_column = Some(ordinal);
                lower = lower.or(maybe_lower);
                upper = upper.or(maybe_upper);
            }
        }
    }

    let eq = eq?;
    let range = range.or_else(|| Some((range_column?, lower?, upper?)))?;

    Some(AggregateEqAndRangeLiteralFilter {
        eq_column_ordinal: eq.column_ordinal,
        eq_literal: eq.literal,
        range_column_ordinal: range.0,
        low_literal: range.1,
        high_literal: range.2,
    })
}

fn row_matches_aggregate_simple_eq_literal_filter(
    row: &Row,
    projected_filter_ordinal: usize,
    literal: &Value,
) -> DbResult<bool> {
    if matches!(literal, Value::Null) {
        return Ok(false);
    }
    let Some(value) = row.values.get(projected_filter_ordinal) else {
        return Ok(false);
    };
    Ok(compare_runtime_values(value, literal)? == Some(Ordering::Equal))
}

fn row_matches_aggregate_between_literal_filter(
    row: &Row,
    projected_range_ordinal: usize,
    low: &Value,
    high: &Value,
) -> DbResult<bool> {
    if matches!(low, Value::Null) || matches!(high, Value::Null) {
        return Ok(false);
    }
    let Some(value) = row.values.get(projected_range_ordinal) else {
        return Ok(false);
    };
    let ge_low =
        compare_runtime_values(value, low)?.is_some_and(|ordering| ordering != Ordering::Less);
    let le_high =
        compare_runtime_values(value, high)?.is_some_and(|ordering| ordering != Ordering::Greater);
    Ok(ge_low && le_high)
}

impl Executor {
    pub(super) fn execute_aggregate_or_set_plan(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        match plan {
            PhysicalPlan::Aggregate {
                table_id,
                group_by,
                grouping_sets,
                aggregates,
                having,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                access_path,
                distinct_on: _,
            } => {
                let plan_limit = limit
                    .as_ref()
                    .map(|e| eval_limit_offset_expr(&self.evaluator, e, "LIMIT"))
                    .transpose()?;
                let effective_limit =
                    effective_collect_limit(plan_limit, context.collect_row_limit);
                if context.has_execution_interrupts() {
                    context.check_deadline()?;
                }
                if matches!(effective_limit, Some(0)) {
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows: Vec::new(),
                    });
                }
                let aggregate_cache_key =
                    self.storage_dml
                        .cache_generation()
                        .map(|_| AggregateRowsCacheKey {
                            table_id: *table_id,
                            group_by_key: format!("{group_by:?}"),
                            grouping_sets_key: format!("{grouping_sets:?}"),
                            aggregates_key: format!("{aggregates:?}"),
                            having_key: having.as_ref().map(|expr| format!("{expr:?}")),
                            filter_key: filter.as_ref().map(|expr| format!("{expr:?}")),
                            order_key: format!("{order_by:?}"),
                            limit_key: limit.as_ref().map(|expr| format!("{expr:?}")),
                            offset_key: offset.as_ref().map(|expr| format!("{expr:?}")),
                            distinct: *distinct,
                            access_path_key: format!("{access_path:?}"),
                        });
                if let (Some(cache_key), Some(generation)) =
                    (&aggregate_cache_key, self.storage_dml.cache_generation())
                {
                    if let Some((cached_generation, cached_rows)) = self
                        .aggregate_rows_cache
                        .read()
                        .map_err(|error| {
                            DbError::internal(format!("aggregate rows cache poisoned: {error}"))
                        })?
                        .get(cache_key)
                        .cloned()
                    {
                        if cached_generation == generation {
                            let mut result_bytes = 0u64;
                            for row in cached_rows.as_slice() {
                                result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                    context,
                                    row,
                                    result_bytes,
                                )?;
                            }
                            return Ok(ExecutionResult::Query {
                                columns: plan.output_fields(),
                                rows: cached_rows.as_ref().clone(),
                            });
                        }
                    }
                }

                if let Some(mut rows) = self.try_group_by_count_via_index_counts(
                    context,
                    *table_id,
                    group_by,
                    aggregates,
                    grouping_sets,
                    having.as_ref(),
                    filter.as_ref(),
                    order_by,
                    *distinct,
                    access_path,
                )? {
                    let offset_val = offset
                        .as_ref()
                        .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
                        .transpose()?
                        .unwrap_or(0);
                    if offset_val > 0 {
                        let skip = clamp_u64_to_usize(offset_val, rows.len());
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

                if can_use_visible_count_fast_path(
                    group_by,
                    grouping_sets,
                    aggregates,
                    having.as_ref(),
                    filter.as_ref(),
                    order_by,
                    *distinct,
                    access_path,
                ) {
                    let offset_val = offset
                        .as_ref()
                        .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
                        .transpose()?
                        .unwrap_or(0);
                    if offset_val > 0 {
                        return Ok(ExecutionResult::Query {
                            columns: plan.output_fields(),
                            rows: Vec::new(),
                        });
                    }
                    if let Ok(count) = self.storage_dml.visible_row_count(
                        context.txn_id,
                        &context.snapshot,
                        *table_id,
                    ) {
                        let count = i64::try_from(count).unwrap_or(i64::MAX);
                        return single_aggregate_row_result(
                            plan.output_fields(),
                            Row::new(vec![Value::BigInt(count); aggregates.len()]),
                            context,
                        );
                    }
                }

                if group_by.is_empty()
                    && grouping_sets.is_empty()
                    && having.is_none()
                    && order_by.is_empty()
                    && !distinct
                    && !aggregates.is_empty()
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
                {
                    if let Some(filter) = filter.as_ref() {
                        let offset_val = offset
                            .as_ref()
                            .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
                            .transpose()?
                            .unwrap_or(0);
                        if offset_val > 0 {
                            return Ok(ExecutionResult::Query {
                                columns: plan.output_fields(),
                                rows: Vec::new(),
                            });
                        }
                        if let Some(count) = self.try_count_eq_and_range_filter(
                            context,
                            *table_id,
                            access_path,
                            filter,
                        )? {
                            let count = i64::try_from(count).unwrap_or(i64::MAX);
                            return single_aggregate_row_result(
                                plan.output_fields(),
                                Row::new(vec![Value::BigInt(count); aggregates.len()]),
                                context,
                            );
                        }
                        if let Some(count) =
                            self.try_count_simple_eq_filter(context, *table_id, filter)?
                        {
                            let count = i64::try_from(count).unwrap_or(i64::MAX);
                            return single_aggregate_row_result(
                                plan.output_fields(),
                                Row::new(vec![Value::BigInt(count); aggregates.len()]),
                                context,
                            );
                        }
                        if let Some(count) =
                            self.try_count_index_range_filter(context, access_path, filter)?
                        {
                            let count = i64::try_from(count).unwrap_or(i64::MAX);
                            return single_aggregate_row_result(
                                plan.output_fields(),
                                Row::new(vec![Value::BigInt(count); aggregates.len()]),
                                context,
                            );
                        }
                        // Only run the SeqScan-based range pushdown when no
                        // index already covers the range — the index path
                        // (`visible_index_row_count` etc.) is much cheaper
                        // for indexed columns. Multi-column AND-of-ranges
                        // is tried first, single-range second; both push
                        // the comparison into `qpqual`-style storage loops.
                        if matches!(access_path, ScanAccessPath::SeqScan) {
                            if let Some(count) =
                                self.try_count_multi_range_filter(context, *table_id, filter)?
                            {
                                let count = i64::try_from(count).unwrap_or(i64::MAX);
                                return single_aggregate_row_result(
                                    plan.output_fields(),
                                    Row::new(vec![Value::BigInt(count); aggregates.len()]),
                                    context,
                                );
                            }
                            if let Some(count) =
                                self.try_count_simple_range_filter(context, *table_id, filter)?
                            {
                                let count = i64::try_from(count).unwrap_or(i64::MAX);
                                return single_aggregate_row_result(
                                    plan.output_fields(),
                                    Row::new(vec![Value::BigInt(count); aggregates.len()]),
                                    context,
                                );
                            }
                        }
                        if let Some(count) =
                            self.try_count_in_literal_filter(context, *table_id, filter)?
                        {
                            let count = i64::try_from(count).unwrap_or(i64::MAX);
                            return single_aggregate_row_result(
                                plan.output_fields(),
                                Row::new(vec![Value::BigInt(count); aggregates.len()]),
                                context,
                            );
                        }
                    }
                }

                if let Some(result) = self.try_simple_group_aggregate(
                    plan,
                    context,
                    *table_id,
                    group_by,
                    grouping_sets,
                    aggregates,
                    having.as_ref(),
                    filter.as_ref(),
                    order_by,
                    limit.as_ref(),
                    offset.as_ref(),
                    *distinct,
                    access_path,
                )? {
                    if let (Some(cache_key), Some(generation)) =
                        (aggregate_cache_key, self.storage_dml.cache_generation())
                    {
                        if let ExecutionResult::Query { rows, .. } = &result {
                            let mut cache = self.aggregate_rows_cache.write().map_err(|error| {
                                DbError::internal(format!("aggregate rows cache poisoned: {error}"))
                            })?;
                            if cache.len() >= 256 {
                                cache.clear();
                            }
                            cache.insert(cache_key, (generation, Arc::new(rows.clone())));
                        }
                    }
                    return Ok(result);
                }

                // `SELECT MIN(col) FROM t` / `SELECT MAX(col) FROM t`
                // fast path. Single aggregate over a column ref, no
                // GROUP BY / HAVING / DISTINCT / WHERE / ORDER BY:
                // the indexed column's first / last leaf entry is the
                // answer. O(log N) instead of full scan + accumulator.
                if group_by.is_empty()
                    && grouping_sets.is_empty()
                    && having.is_none()
                    && order_by.is_empty()
                    && !distinct
                    && filter.is_none()
                    && aggregates.len() == 1
                {
                    let agg = &aggregates[0];
                    let min_max_target = match &agg.expr.kind {
                        TypedExprKind::AggMin { expr, filter: None } => {
                            Some((expr.as_ref(), false))
                        }
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
                                return single_aggregate_row_result(
                                    plan.output_fields(),
                                    Row::new(vec![value]),
                                    context,
                                );
                            }
                        }
                    }
                }

                // Virtual tables (e.g. pg_catalog) are normally rewritten into
                // ProjectValues by the virtual_scan_rewriter before execution;
                // if one still reaches here, treat the absent physical storage
                // as an empty stream. Real tables must propagate scan errors.
                let mut stream =
                    match self.resolve_scan_stream(context, *table_id, access_path, None) {
                        Ok(stream) => stream,
                        Err(error) => {
                            if aiondb_planner::is_virtual_synthetic_relation(table_id.get()) {
                                Box::new(VecTupleStream::new(Vec::new()))
                            } else {
                                return Err(error);
                            }
                        }
                    };

                let mut agg_templates: Vec<AggTemplate> = aggregates
                    .iter()
                    .map(|proj| classify_agg_expr(&proj.expr))
                    .collect();

                // Extract additional aggregate sub-expressions from HAVING and
                // ORDER BY that are not present in the output projections.
                // These need their own accumulators so that `evaluate_having_expr`
                // can resolve them against the finalized row.
                let num_output_aggs = agg_templates.len();
                let mut extra_agg_exprs: Vec<AggregateExprRef<'_>> = Vec::with_capacity(
                    order_by.len().saturating_add(usize::from(having.is_some())),
                );
                {
                    // Use a HashSet of Debug-formatted expression keys for O(1) dedup
                    // instead of O(n) linear scans per candidate expression.
                    let mut seen_agg_keys: std::collections::HashSet<String> = aggregates
                        .iter()
                        .map(|proj| format!("{:?}", proj.expr))
                        .collect();

                    if let Some(having_expr) = having {
                        for agg_expr in find_aggregate_subexprs(having_expr) {
                            let key = format!("{agg_expr:?}");
                            if !seen_agg_keys.insert(key) {
                                continue;
                            }
                            let template = classify_agg_expr(agg_expr);
                            agg_templates.push(template);
                            extra_agg_exprs.push(AggregateExprRef::borrowed("", agg_expr));
                        }
                    }
                    for sort in order_by {
                        for agg_expr in find_aggregate_subexprs(&sort.expr) {
                            let key = format!("{agg_expr:?}");
                            if !seen_agg_keys.insert(key) {
                                continue;
                            }
                            let template = classify_agg_expr(agg_expr);
                            agg_templates.push(template);
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

                // ── Grouping sets path ──
                // When grouping_sets is non-empty, we must run multiple
                // aggregation passes over the same input data, once per
                // grouping set.  For each pass only the columns listed in
                // that set participate in the group key; the remaining
                // group-by columns are NULL in the output.
                if !grouping_sets.is_empty() {
                    // 1. Collect all filtered input rows and their
                    //    pre-evaluated group-by column values.
                    let mut input_rows: Vec<(Row, Vec<Value>)> = Vec::new();
                    if !can_skip_scalar_group_input_scan(
                        group_by,
                        aggregates,
                        having.as_ref(),
                        order_by,
                    ) {
                        while let Some(record) = stream.next()? {
                            context.check_deadline()?;
                            let compat_row =
                                self.compat_scan_row_for_table_id(context, *table_id, &record)?;
                            if !predicate_matches(
                                filter
                                    .as_ref()
                                    .map(|f| self.evaluate_expr_with_row(f, &compat_row, context)),
                            )? {
                                continue;
                            }
                            let gb_vals: Vec<Value> = group_by
                                .iter()
                                .map(|gb| self.evaluate_expr_with_row(gb, &compat_row, context))
                                .collect::<DbResult<Vec<_>>>()?;
                            context
                                .track_memory(estimate_row_bytes(&compat_row).saturating_add(64))?;
                            input_rows.push((compat_row, gb_vals));
                        }
                    }

                    // Identify output columns that are grouping() calls.
                    let grouping_projs = find_grouping_projections(aggregates, group_by);
                    let grouping_output_plan = build_grouping_output_plan(aggregates, group_by);

                    let has_ordering = !order_by.is_empty();
                    let offset_val = offset
                        .as_ref()
                        .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
                        .transpose()?
                        .unwrap_or(0);
                    let _has_offset = offset_val > 0;
                    let mut result_rows: Vec<SortedQueryRow> = Vec::new();
                    let mut result_bytes = 0u64;
                    let mut all_projections: Vec<AggregateExprRef<'_>> = Vec::with_capacity(
                        aggregates.len() + extra_agg_exprs.len() + hidden_group_exprs.len(),
                    );
                    all_projections
                        .extend(aggregates.iter().map(AggregateExprRef::from_projection));
                    all_projections.extend(extra_agg_exprs.iter().cloned());
                    all_projections.extend(hidden_group_exprs.iter().cloned());

                    // 2. For each grouping set, aggregate independently.
                    for active_set in grouping_sets {
                        let mut groups: std::collections::HashMap<Vec<ValueHashKey>, usize> =
                            std::collections::HashMap::new();
                        let mut ordered_groups: Vec<Vec<AggAccumulator>> = Vec::new();
                        // Track actual group-by values in insertion order so we can
                        // reconstruct output rows with NULLs for inactive columns.
                        let mut group_active_values: Vec<Vec<Value>> = Vec::new();
                        let active_positions =
                            build_active_group_positions(active_set, group_by.len());

                        for (row, gb_vals) in &input_rows {
                            context.check_deadline()?;
                            // Build partial key: only active columns.
                            let mut partial_key: Vec<ValueHashKey> =
                                Vec::with_capacity(active_set.len());
                            for &idx in active_set {
                                partial_key.push(build_hash_key(&gb_vals[idx])?);
                            }

                            let group_idx = match groups.entry(partial_key) {
                                std::collections::hash_map::Entry::Occupied(o) => *o.get(),
                                std::collections::hash_map::Entry::Vacant(v) => {
                                    context
                                        .track_memory(estimate_row_bytes(row).saturating_add(64))?;
                                    // Store actual values for active columns.
                                    let mut vals: Vec<Value> = Vec::with_capacity(active_set.len());
                                    for &idx in active_set {
                                        vals.push(gb_vals[idx].clone());
                                    }
                                    group_active_values.push(vals);
                                    let group_idx = ordered_groups.len();
                                    ordered_groups.push(
                                        agg_templates
                                            .iter()
                                            .map(AggAccumulator::from_template)
                                            .collect(),
                                    );
                                    v.insert(group_idx);
                                    group_idx
                                }
                            };
                            let accumulators =
                                ordered_groups.get_mut(group_idx).ok_or_else(|| {
                                    DbError::internal(
                                        "missing accumulator group during aggregate evaluation",
                                    )
                                })?;

                            for (acc, template) in accumulators.iter_mut().zip(agg_templates.iter())
                            {
                                if let Some(ref filter_expr) = template.filter {
                                    let filter_val =
                                        self.evaluate_expr_with_row(filter_expr, row, context)?;
                                    if !matches!(filter_val, Value::Boolean(true)) {
                                        continue;
                                    }
                                }
                                self.accumulate_value(acc, template, row, context)?;
                            }
                        }

                        // If no input rows and this set includes the empty
                        // grouping set (), produce a grand-total row.
                        if ordered_groups.is_empty() && active_set.is_empty() {
                            group_active_values.push(Vec::new());
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
                            for (acc, template) in accumulators.iter().zip(agg_templates.iter()) {
                                finalized_values.push(finalize_accumulator(
                                    acc,
                                    template,
                                    &self.evaluator,
                                    context,
                                )?);
                            }

                            // Patch group-by column values: active columns get
                            // their real value, inactive columns get NULL.
                            let active_vals = group_active_values.get(group_idx);
                            for out_idx in 0..aggregates.len() {
                                if let Some(gb_idx) =
                                    grouping_output_plan.output_group_by_match[out_idx]
                                {
                                    if let Some(active_pos) =
                                        active_positions.get(gb_idx).copied().flatten()
                                    {
                                        if let Some(v) =
                                            active_vals.and_then(|vals| vals.get(active_pos))
                                        {
                                            if out_idx < finalized_values.len() {
                                                finalized_values[out_idx] = v.clone();
                                            }
                                        }
                                    } else if out_idx < finalized_values.len() {
                                        finalized_values[out_idx] = Value::Null;
                                    }
                                } else if !grouping_output_plan.output_has_aggregate[out_idx] {
                                    let references_inactive = grouping_output_plan
                                        .output_referenced_group_by[out_idx]
                                        .iter()
                                        .any(|&gb_idx| {
                                            active_positions
                                                .get(gb_idx)
                                                .copied()
                                                .flatten()
                                                .is_none()
                                        });
                                    if references_inactive && out_idx < finalized_values.len() {
                                        finalized_values[out_idx] = Value::Null;
                                    }
                                }
                            }

                            // Patch grouping() function values.
                            for (out_idx, ref col_indices) in &grouping_projs {
                                context.check_deadline()?;
                                if *out_idx < finalized_values.len() {
                                    finalized_values[*out_idx] = Value::Int(
                                        compute_grouping_bitmask(col_indices, active_set),
                                    );
                                }
                            }

                            let agg_row = Row::new(finalized_values);

                            if let Some(having_expr) = having {
                                let having_val = self.evaluate_having_expr_extended(
                                    having_expr,
                                    &agg_row,
                                    &all_projections,
                                    context,
                                )?;
                                match having_val {
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

                            let sort_keys: Vec<Value> = if has_ordering {
                                order_by
                                    .iter()
                                    .map(|sort| {
                                        evaluate_post_aggregate_sort_expr_fast(
                                            self,
                                            &sort.expr,
                                            &agg_row,
                                            &all_projections,
                                            context,
                                        )
                                    })
                                    .collect::<DbResult<Vec<_>>>()?
                            } else {
                                // Build default sort keys for natural grouping sets ordering:
                                // sort by group-by column values (NULLs last for inactive),
                                // then by grouping level (more specific first).
                                let mut keys = Vec::with_capacity(group_by.len() + 1);
                                for gb_idx in 0..group_by.len() {
                                    if let Some(active_pos) =
                                        active_positions.get(gb_idx).copied().flatten()
                                    {
                                        keys.push(
                                            active_vals
                                                .and_then(|vals| vals.get(active_pos))
                                                .cloned()
                                                .unwrap_or(Value::Null),
                                        );
                                    } else {
                                        keys.push(Value::Null);
                                    }
                                }
                                // Tiebreaker: fewer active columns = higher grouping level = sort later
                                keys.push(Value::Int(neg_len_i32(active_set.len())));
                                keys
                            };
                            let mut output_row = agg_row;
                            if num_output_aggs < output_row.values.len() {
                                output_row.values.truncate(num_output_aggs);
                            }
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
                        if let Some(limit) = effective_limit {
                            let topk = clamp_u64_to_usize(
                                limit.saturating_add(offset_val),
                                result_rows.len(),
                            );
                            sort_query_rows_bounded(&mut result_rows, order_by, topk, context)?;
                        } else {
                            sort_query_rows(&mut result_rows, order_by, context)?;
                        }
                    } else if grouping_sets.len() > 1 {
                        // Apply natural grouping-sets ordering: sort by group-by
                        // column values with NULLs last, then by grouping level.
                        let num_gb = group_by.len();
                        let error: std::cell::RefCell<Option<DbError>> =
                            std::cell::RefCell::new(None);
                        result_rows.sort_by(|a, b| {
                            if error.borrow().is_some() {
                                return std::cmp::Ordering::Equal;
                            }
                            if let Err(e) = context.check_deadline() {
                                *error.borrow_mut() = Some(e);
                                return std::cmp::Ordering::Equal;
                            }
                            for i in 0..num_gb {
                                if i >= a.sort_keys.len() || i >= b.sort_keys.len() {
                                    break;
                                }
                                match compare_sort_values(
                                    &a.sort_keys[i],
                                    &b.sort_keys[i],
                                    false,
                                    Some(false),
                                ) {
                                    Ok(std::cmp::Ordering::Equal) => continue,
                                    Ok(ord) => return ord,
                                    Err(e) => {
                                        *error.borrow_mut() = Some(e);
                                        return std::cmp::Ordering::Equal;
                                    }
                                }
                            }
                            // Tiebreaker: grouping level (stored as negative active_set.len())
                            let a_level = if num_gb < a.sort_keys.len() {
                                &a.sort_keys[num_gb]
                            } else {
                                &Value::Null
                            };
                            let b_level = if num_gb < b.sort_keys.len() {
                                &b.sort_keys[num_gb]
                            } else {
                                &Value::Null
                            };
                            match compare_sort_values(a_level, b_level, false, Some(false)) {
                                Ok(ord) => ord,
                                Err(e) => {
                                    *error.borrow_mut() = Some(e);
                                    std::cmp::Ordering::Equal
                                }
                            }
                        });
                        if let Some(e) = error.into_inner() {
                            return Err(e);
                        }
                    }

                    let mut rows = result_rows
                        .into_iter()
                        .map(|entry| entry.row)
                        .collect::<Vec<_>>();

                    if window_eval::has_window_functions(aggregates) {
                        window_eval::evaluate_post_aggregate_windows(
                            self, aggregates, &mut rows, context,
                        )?;
                    }

                    if *distinct {
                        dedup_rows_by_value_hash(&mut rows, context)?;
                    }

                    if offset_val > 0 {
                        let skip = clamp_u64_to_usize(offset_val, rows.len());
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
                let mut groups: std::collections::HashMap<Vec<ValueHashKey>, usize> =
                    std::collections::HashMap::new();
                let mut ordered_groups: Vec<Vec<AggAccumulator>> = Vec::new();
                let mut presorted_group_output = false;

                if !can_skip_scalar_group_input_scan(
                    group_by,
                    aggregates,
                    having.as_ref(),
                    order_by,
                ) {
                    if group_by.len() == 1 {
                        let gb_expr = &group_by[0];
                        let can_stream_groups = simple_column_ordinal(gb_expr)
                            .map(|group_ordinal| {
                                aggregate_stream_group_scan_supported(
                                    self,
                                    context,
                                    *table_id,
                                    access_path,
                                    group_ordinal,
                                )
                            })
                            .transpose()?
                            .unwrap_or(false);
                        if can_stream_groups {
                            if let Ok(Some(ordered_stream)) = self.resolve_scan_stream_ordered(
                                context,
                                *table_id,
                                access_path,
                                None,
                                false,
                            ) {
                                stream = ordered_stream;
                                presorted_group_output = true;
                                let mut current_group_key: Option<ValueHashKey> = None;
                                let mut current_group_accumulators: Option<Vec<AggAccumulator>> =
                                    None;
                                while let Some(record) = stream.next()? {
                                    context.check_deadline()?;
                                    let compat_row = self.compat_scan_row_for_table_id(
                                        context, *table_id, &record,
                                    )?;

                                    if !predicate_matches(filter.as_ref().map(|f| {
                                        self.evaluate_expr_with_row(f, &compat_row, context)
                                    }))? {
                                        continue;
                                    }

                                    let val =
                                        self.evaluate_expr_with_row(gb_expr, &compat_row, context)?;
                                    let group_key = build_hash_key(&val)?;
                                    let new_group = current_group_key
                                        .as_ref()
                                        .map_or(true, |current| *current != group_key);
                                    if new_group {
                                        if let Some(group_accumulators) =
                                            current_group_accumulators.take()
                                        {
                                            ordered_groups.push(group_accumulators);
                                        }
                                        context.track_memory(
                                            estimate_row_bytes(&compat_row).saturating_add(64),
                                        )?;
                                        current_group_key = Some(group_key);
                                        current_group_accumulators = Some(
                                            agg_templates
                                                .iter()
                                                .map(AggAccumulator::from_template)
                                                .collect(),
                                        );
                                    }
                                    let accumulators = current_group_accumulators
                                        .as_mut()
                                        .ok_or_else(|| {
                                            DbError::internal(
                                                "missing streamed accumulator group during aggregate evaluation",
                                            )
                                        })?;
                                    for (acc, template) in
                                        accumulators.iter_mut().zip(agg_templates.iter())
                                    {
                                        if let Some(ref filter_expr) = template.filter {
                                            let filter_val = self.evaluate_expr_with_row(
                                                filter_expr,
                                                &compat_row,
                                                context,
                                            )?;
                                            if !matches!(filter_val, Value::Boolean(true)) {
                                                continue;
                                            }
                                        }
                                        self.accumulate_value(acc, template, &compat_row, context)?;
                                    }
                                }
                                if let Some(group_accumulators) = current_group_accumulators.take()
                                {
                                    ordered_groups.push(group_accumulators);
                                }
                            } else {
                                let mut groups_single: std::collections::HashMap<
                                    ValueHashKey,
                                    usize,
                                > = std::collections::HashMap::new();
                                while let Some(record) = stream.next()? {
                                    context.check_deadline()?;
                                    let compat_row = self.compat_scan_row_for_table_id(
                                        context, *table_id, &record,
                                    )?;

                                    if !predicate_matches(filter.as_ref().map(|f| {
                                        self.evaluate_expr_with_row(f, &compat_row, context)
                                    }))? {
                                        continue;
                                    }

                                    let val =
                                        self.evaluate_expr_with_row(gb_expr, &compat_row, context)?;
                                    let group_key = build_hash_key(&val)?;
                                    let group_idx =
                                        if let Some(&idx) = groups_single.get(&group_key) {
                                            idx
                                        } else {
                                            context.track_memory(
                                                estimate_row_bytes(&compat_row).saturating_add(64),
                                            )?;
                                            let group_idx = ordered_groups.len();
                                            ordered_groups.push(
                                                agg_templates
                                                    .iter()
                                                    .map(AggAccumulator::from_template)
                                                    .collect(),
                                            );
                                            groups_single.insert(group_key, group_idx);
                                            group_idx
                                        };
                                    let accumulators = ordered_groups.get_mut(group_idx).ok_or_else(
                                        || {
                                            DbError::internal(
                                                "missing accumulator group during aggregate evaluation",
                                            )
                                        },
                                    )?;

                                    for (acc, template) in
                                        accumulators.iter_mut().zip(agg_templates.iter())
                                    {
                                        if let Some(ref filter_expr) = template.filter {
                                            let filter_val = self.evaluate_expr_with_row(
                                                filter_expr,
                                                &compat_row,
                                                context,
                                            )?;
                                            if !matches!(filter_val, Value::Boolean(true)) {
                                                continue;
                                            }
                                        }
                                        self.accumulate_value(acc, template, &compat_row, context)?;
                                    }
                                }
                            }
                        } else {
                            let mut groups_single: std::collections::HashMap<ValueHashKey, usize> =
                                std::collections::HashMap::new();
                            while let Some(record) = stream.next()? {
                                context.check_deadline()?;
                                let compat_row =
                                    self.compat_scan_row_for_table_id(context, *table_id, &record)?;

                                if !predicate_matches(
                                    filter.as_ref().map(|f| {
                                        self.evaluate_expr_with_row(f, &compat_row, context)
                                    }),
                                )? {
                                    continue;
                                }

                                let val =
                                    self.evaluate_expr_with_row(gb_expr, &compat_row, context)?;
                                let group_key = build_hash_key(&val)?;
                                let group_idx = if let Some(&idx) = groups_single.get(&group_key) {
                                    idx
                                } else {
                                    context.track_memory(
                                        estimate_row_bytes(&compat_row).saturating_add(64),
                                    )?;
                                    let group_idx = ordered_groups.len();
                                    ordered_groups.push(
                                        agg_templates
                                            .iter()
                                            .map(AggAccumulator::from_template)
                                            .collect(),
                                    );
                                    groups_single.insert(group_key, group_idx);
                                    group_idx
                                };
                                let accumulators =
                                    ordered_groups.get_mut(group_idx).ok_or_else(|| {
                                        DbError::internal(
                                            "missing accumulator group during aggregate evaluation",
                                        )
                                    })?;

                                for (acc, template) in
                                    accumulators.iter_mut().zip(agg_templates.iter())
                                {
                                    if let Some(ref filter_expr) = template.filter {
                                        let filter_val = self.evaluate_expr_with_row(
                                            filter_expr,
                                            &compat_row,
                                            context,
                                        )?;
                                        if !matches!(filter_val, Value::Boolean(true)) {
                                            continue;
                                        }
                                    }
                                    self.accumulate_value(acc, template, &compat_row, context)?;
                                }
                            }
                        }
                    } else {
                        let mut group_key_scratch: Vec<ValueHashKey> =
                            Vec::with_capacity(group_by.len());
                        while let Some(record) = stream.next()? {
                            context.check_deadline()?;
                            let compat_row =
                                self.compat_scan_row_for_table_id(context, *table_id, &record)?;

                            if !predicate_matches(
                                filter
                                    .as_ref()
                                    .map(|f| self.evaluate_expr_with_row(f, &compat_row, context)),
                            )? {
                                continue;
                            }

                            group_key_scratch.clear();
                            for gb_expr in group_by {
                                let val =
                                    self.evaluate_expr_with_row(gb_expr, &compat_row, context)?;
                                group_key_scratch.push(build_hash_key(&val)?);
                            }

                            // Fast path: look up existing group without allocating
                            // a new key. Only clone the scratch buffer on insert.
                            let group_idx = if let Some(&idx) = groups.get(&group_key_scratch) {
                                idx
                            } else {
                                context.track_memory(
                                    estimate_row_bytes(&compat_row).saturating_add(64),
                                )?;
                                let group_idx = ordered_groups.len();
                                ordered_groups.push(
                                    agg_templates
                                        .iter()
                                        .map(AggAccumulator::from_template)
                                        .collect(),
                                );
                                groups.insert(group_key_scratch.clone(), group_idx);
                                group_idx
                            };
                            let accumulators =
                                ordered_groups.get_mut(group_idx).ok_or_else(|| {
                                    DbError::internal(
                                        "missing accumulator group during aggregate evaluation",
                                    )
                                })?;

                            for (acc, template) in accumulators.iter_mut().zip(agg_templates.iter())
                            {
                                if let Some(ref filter_expr) = template.filter {
                                    let filter_val = self.evaluate_expr_with_row(
                                        filter_expr,
                                        &compat_row,
                                        context,
                                    )?;
                                    if !matches!(filter_val, Value::Boolean(true)) {
                                        continue;
                                    }
                                }
                                self.accumulate_value(acc, template, &compat_row, context)?;
                            }
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

                let has_ordering = !order_by.is_empty();
                let offset_val = offset
                    .as_ref()
                    .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
                    .transpose()?
                    .unwrap_or(0);
                let has_offset = offset_val > 0;
                let mut result_rows: Vec<SortedQueryRow> = Vec::new();
                let mut result_bytes = 0u64;
                let mut all_projections: Vec<AggregateExprRef<'_>> = Vec::with_capacity(
                    aggregates.len() + extra_agg_exprs.len() + hidden_group_exprs.len(),
                );
                all_projections.extend(aggregates.iter().map(AggregateExprRef::from_projection));
                all_projections.extend(extra_agg_exprs.iter().cloned());
                all_projections.extend(hidden_group_exprs.iter().cloned());

                for accumulators in &ordered_groups {
                    context.check_deadline()?;
                    let mut finalized_values = Vec::with_capacity(accumulators.len());
                    for (acc, template) in accumulators.iter().zip(agg_templates.iter()) {
                        finalized_values.push(finalize_accumulator(
                            acc,
                            template,
                            &self.evaluator,
                            context,
                        )?);
                    }
                    let agg_row = Row::new(finalized_values);

                    if let Some(having_expr) = having {
                        let having_val = self.evaluate_having_expr_extended(
                            having_expr,
                            &agg_row,
                            &all_projections,
                            context,
                        )?;
                        match having_val {
                            Value::Boolean(true) => {}
                            Value::Boolean(false) | Value::Null => continue,
                            _ => {
                                return Err(DbError::internal(
                                    "HAVING expression did not evaluate to BOOLEAN",
                                ));
                            }
                        }
                    }

                    if !has_ordering
                        && !has_offset
                        && effective_limit.is_some_and(|lim| usize_to_u64(result_rows.len()) >= lim)
                    {
                        break;
                    }

                    if usize_to_u64(result_rows.len()) >= context.max_result_rows {
                        return Err(DbError::program_limit(
                            "maximum number of result rows reached",
                        ));
                    }

                    let sort_keys: Vec<Value> = order_by
                        .iter()
                        .map(|sort| {
                            evaluate_post_aggregate_sort_expr_fast(
                                self,
                                &sort.expr,
                                &agg_row,
                                &all_projections,
                                context,
                            )
                        })
                        .collect::<DbResult<Vec<_>>>()?;
                    // Strip extra accumulator columns added for HAVING/ORDER BY
                    // before pushing the output row.
                    let mut output_row = agg_row;
                    if num_output_aggs < output_row.values.len() {
                        output_row.values.truncate(num_output_aggs);
                    }
                    push_sorted_query_row(
                        &mut result_rows,
                        context,
                        output_row,
                        sort_keys,
                        &mut result_bytes,
                    )?;
                }

                if has_ordering && !presorted_group_output {
                    if let Some(limit) = effective_limit {
                        let topk =
                            clamp_u64_to_usize(limit.saturating_add(offset_val), result_rows.len());
                        sort_query_rows_bounded(&mut result_rows, order_by, topk, context)?;
                    } else {
                        sort_query_rows(&mut result_rows, order_by, context)?;
                    }
                }

                let mut rows = result_rows
                    .into_iter()
                    .map(|entry| entry.row)
                    .collect::<Vec<_>>();

                // Apply post-aggregate window functions if any output is a
                // window function.
                if window_eval::has_window_functions(aggregates) {
                    window_eval::evaluate_post_aggregate_windows(
                        self, aggregates, &mut rows, context,
                    )?;
                }

                if *distinct {
                    dedup_rows_by_value_hash(&mut rows, context)?;
                }

                if offset_val > 0 {
                    let skip = clamp_u64_to_usize(offset_val, rows.len());
                    rows.drain(..skip);
                }

                if let Some(limit) = effective_limit {
                    rows.truncate(clamp_u64_to_usize(limit, rows.len()));
                }

                Ok(ExecutionResult::Query {
                    columns: plan.output_fields(),
                    rows,
                })
            }
            PhysicalPlan::AggregateSource {
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
            } => {
                // `SELECT MIN(col) FROM t` / `SELECT MAX(col) FROM t`
                // fast path for the AggregateSource shape too: when
                // the source is a SeqScan over a single table and we
                // have just one MIN/MAX aggregate on a column ref,
                // walk the index leaves directly. Conditions match
                // the Aggregate-arm version above.
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
                            TypedExprKind::AggMin { expr, filter: None } => {
                                Some((expr.as_ref(), false))
                            }
                            TypedExprKind::AggMax { expr, filter: None } => {
                                Some((expr.as_ref(), true))
                            }
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
                                    return single_aggregate_row_result(
                                        plan.output_fields(),
                                        Row::new(vec![value]),
                                        context,
                                    );
                                }
                            }
                        }
                    }
                }

                let plan_limit = limit
                    .as_ref()
                    .map(|e| eval_limit_offset_expr(&self.evaluator, e, "LIMIT"))
                    .transpose()?;
                let effective_limit =
                    effective_collect_limit(plan_limit, context.collect_row_limit);
                context.check_deadline()?;
                if matches!(effective_limit, Some(0)) {
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows: Vec::new(),
                    });
                }

                if filter.is_none() {
                    if let PhysicalPlan::ProjectTable {
                        table_id,
                        outputs,
                        filter: source_filter,
                        order_by: source_order_by,
                        limit: source_limit,
                        offset: source_offset,
                        distinct: source_distinct,
                        distinct_on: source_distinct_on,
                        access_path,
                    } = source.as_ref()
                    {
                        let simple_source = source_order_by.is_empty()
                            && source_limit.is_none()
                            && source_offset.is_none()
                            && !*source_distinct
                            && source_distinct_on.is_empty();
                        if simple_source
                            && group_by.is_empty()
                            && grouping_sets.is_empty()
                            && having.is_none()
                            && order_by.is_empty()
                            && !*distinct
                            && count_star_outputs(aggregates)
                        {
                            if let Some(count) = self.try_count_project_table_source(
                                context,
                                *table_id,
                                source_filter.as_ref(),
                                access_path,
                            )? {
                                let offset_val = offset
                                    .as_ref()
                                    .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
                                    .transpose()?
                                    .unwrap_or(0);
                                if offset_val > 0 {
                                    return Ok(ExecutionResult::Query {
                                        columns: plan.output_fields(),
                                        rows: Vec::new(),
                                    });
                                }
                                let count = i64::try_from(count).unwrap_or(i64::MAX);
                                return single_aggregate_row_result(
                                    plan.output_fields(),
                                    Row::new(vec![Value::BigInt(count); aggregates.len()]),
                                    context,
                                );
                            }
                        }
                        if simple_source && project_table_outputs_are_identity(outputs) {
                            if let Some(result) = self.try_simple_group_aggregate(
                                plan,
                                context,
                                *table_id,
                                group_by,
                                grouping_sets,
                                aggregates,
                                having.as_ref(),
                                source_filter.as_ref(),
                                order_by,
                                limit.as_ref(),
                                offset.as_ref(),
                                *distinct,
                                access_path,
                            )? {
                                return Ok(result);
                            }
                        }
                    }
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

                let mut agg_templates: Vec<AggTemplate> = aggregates
                    .iter()
                    .map(|proj| classify_agg_expr(&proj.expr))
                    .collect();

                let num_output_aggs = agg_templates.len();
                let mut extra_agg_exprs: Vec<AggregateExprRef<'_>> = Vec::with_capacity(
                    order_by.len().saturating_add(usize::from(having.is_some())),
                );
                {
                    // Use a HashSet of Debug-formatted expression keys for O(1) dedup
                    // instead of O(n) linear scans per candidate expression.
                    let mut seen_agg_keys: std::collections::HashSet<String> = aggregates
                        .iter()
                        .map(|proj| format!("{:?}", proj.expr))
                        .collect();

                    if let Some(having_expr) = having {
                        for agg_expr in find_aggregate_subexprs(having_expr) {
                            let key = format!("{agg_expr:?}");
                            if !seen_agg_keys.insert(key) {
                                continue;
                            }
                            let template = classify_agg_expr(agg_expr);
                            agg_templates.push(template);
                            extra_agg_exprs.push(AggregateExprRef::borrowed("", agg_expr));
                        }
                    }
                    for sort in order_by {
                        for agg_expr in find_aggregate_subexprs(&sort.expr) {
                            let key = format!("{agg_expr:?}");
                            if !seen_agg_keys.insert(key) {
                                continue;
                            }
                            let template = classify_agg_expr(agg_expr);
                            agg_templates.push(template);
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

                // ── Grouping sets path (AggregateSource) ──
                if !grouping_sets.is_empty() {
                    let mut input_rows: Vec<(Row, Vec<Value>)> = Vec::new();
                    if !can_skip_scalar_group_input_scan(
                        group_by,
                        aggregates,
                        having.as_ref(),
                        order_by,
                    ) {
                        let source_result = self.execute(source, context)?;
                        let ExecutionResult::Query {
                            rows: source_rows, ..
                        } = source_result
                        else {
                            return Err(DbError::internal(
                                "derived aggregate source must produce query rows",
                            ));
                        };
                        for row in source_rows {
                            context.check_deadline()?;
                            if !predicate_matches(
                                filter
                                    .as_ref()
                                    .map(|f| self.evaluate_expr_with_row(f, &row, context)),
                            )? {
                                continue;
                            }
                            let gb_vals: Vec<Value> = group_by
                                .iter()
                                .map(|gb| self.evaluate_expr_with_row(gb, &row, context))
                                .collect::<DbResult<Vec<_>>>()?;
                            context.track_memory(estimate_row_bytes(&row).saturating_add(64))?;
                            input_rows.push((row, gb_vals));
                        }
                    }

                    let grouping_projs = find_grouping_projections(aggregates, group_by);
                    let grouping_output_plan = build_grouping_output_plan(aggregates, group_by);

                    let has_ordering = !order_by.is_empty();
                    let offset_val = offset
                        .as_ref()
                        .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
                        .transpose()?
                        .unwrap_or(0);
                    let _has_offset = offset_val > 0;
                    let mut result_rows: Vec<SortedQueryRow> = Vec::new();
                    let mut result_bytes = 0u64;
                    let mut all_projections: Vec<AggregateExprRef<'_>> = Vec::with_capacity(
                        aggregates.len() + extra_agg_exprs.len() + hidden_group_exprs.len(),
                    );
                    all_projections
                        .extend(aggregates.iter().map(AggregateExprRef::from_projection));
                    all_projections.extend(extra_agg_exprs.iter().cloned());
                    all_projections.extend(hidden_group_exprs.iter().cloned());

                    for active_set in grouping_sets {
                        let mut groups: std::collections::HashMap<Vec<ValueHashKey>, usize> =
                            std::collections::HashMap::new();
                        let mut ordered_groups: Vec<Vec<AggAccumulator>> = Vec::new();
                        let mut group_active_values: Vec<Vec<Value>> = Vec::new();
                        let active_positions =
                            build_active_group_positions(active_set, group_by.len());

                        for (row, gb_vals) in &input_rows {
                            context.check_deadline()?;
                            let mut partial_key: Vec<ValueHashKey> =
                                Vec::with_capacity(active_set.len());
                            for &idx in active_set {
                                partial_key.push(build_hash_key(&gb_vals[idx])?);
                            }

                            let group_idx = match groups.entry(partial_key) {
                                std::collections::hash_map::Entry::Occupied(o) => *o.get(),
                                std::collections::hash_map::Entry::Vacant(v) => {
                                    context
                                        .track_memory(estimate_row_bytes(row).saturating_add(64))?;
                                    let mut vals: Vec<Value> = Vec::with_capacity(active_set.len());
                                    for &idx in active_set {
                                        vals.push(gb_vals[idx].clone());
                                    }
                                    group_active_values.push(vals);
                                    let group_idx = ordered_groups.len();
                                    ordered_groups.push(
                                        agg_templates
                                            .iter()
                                            .map(AggAccumulator::from_template)
                                            .collect(),
                                    );
                                    v.insert(group_idx);
                                    group_idx
                                }
                            };
                            let accumulators =
                                ordered_groups.get_mut(group_idx).ok_or_else(|| {
                                    DbError::internal(
                                        "missing accumulator group during aggregate evaluation",
                                    )
                                })?;

                            for (acc, template) in accumulators.iter_mut().zip(agg_templates.iter())
                            {
                                if let Some(ref filter_expr) = template.filter {
                                    let filter_val =
                                        self.evaluate_expr_with_row(filter_expr, row, context)?;
                                    if !matches!(filter_val, Value::Boolean(true)) {
                                        continue;
                                    }
                                }
                                self.accumulate_value(acc, template, row, context)?;
                            }
                        }

                        if ordered_groups.is_empty() && active_set.is_empty() {
                            ordered_groups.push(
                                agg_templates
                                    .iter()
                                    .map(AggAccumulator::from_template)
                                    .collect(),
                            );
                            group_active_values.push(Vec::new());
                        }

                        for (group_idx, accumulators) in ordered_groups.iter().enumerate() {
                            context.check_deadline()?;
                            let mut finalized_values = Vec::with_capacity(accumulators.len());
                            for (acc, template) in accumulators.iter().zip(agg_templates.iter()) {
                                finalized_values.push(finalize_accumulator(
                                    acc,
                                    template,
                                    &self.evaluator,
                                    context,
                                )?);
                            }

                            let active_vals = group_active_values.get(group_idx);
                            for out_idx in 0..aggregates.len() {
                                if let Some(gb_idx) =
                                    grouping_output_plan.output_group_by_match[out_idx]
                                {
                                    if let Some(active_pos) =
                                        active_positions.get(gb_idx).copied().flatten()
                                    {
                                        if let Some(v) =
                                            active_vals.and_then(|vals| vals.get(active_pos))
                                        {
                                            if out_idx < finalized_values.len() {
                                                finalized_values[out_idx] = v.clone();
                                            }
                                        }
                                    } else if out_idx < finalized_values.len() {
                                        finalized_values[out_idx] = Value::Null;
                                    }
                                } else if !grouping_output_plan.output_has_aggregate[out_idx] {
                                    let references_inactive = grouping_output_plan
                                        .output_referenced_group_by[out_idx]
                                        .iter()
                                        .any(|&gb_idx| {
                                            active_positions
                                                .get(gb_idx)
                                                .copied()
                                                .flatten()
                                                .is_none()
                                        });
                                    if references_inactive && out_idx < finalized_values.len() {
                                        finalized_values[out_idx] = Value::Null;
                                    }
                                }
                            }

                            for (out_idx, ref col_indices) in &grouping_projs {
                                context.check_deadline()?;
                                if *out_idx < finalized_values.len() {
                                    finalized_values[*out_idx] = Value::Int(
                                        compute_grouping_bitmask(col_indices, active_set),
                                    );
                                }
                            }

                            let agg_row = Row::new(finalized_values);

                            if let Some(having_expr) = having {
                                let having_val = self.evaluate_having_expr_extended(
                                    having_expr,
                                    &agg_row,
                                    &all_projections,
                                    context,
                                )?;
                                match having_val {
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

                            let sort_keys: Vec<Value> = if has_ordering {
                                order_by
                                    .iter()
                                    .map(|sort| {
                                        evaluate_post_aggregate_sort_expr_fast(
                                            self,
                                            &sort.expr,
                                            &agg_row,
                                            &all_projections,
                                            context,
                                        )
                                    })
                                    .collect::<DbResult<Vec<_>>>()?
                            } else {
                                // Build default sort keys for natural grouping sets ordering:
                                // sort by group-by column values (NULLs last for inactive),
                                // then by grouping level (more specific first).
                                let mut keys = Vec::with_capacity(group_by.len() + 1);
                                for gb_idx in 0..group_by.len() {
                                    if let Some(active_pos) =
                                        active_positions.get(gb_idx).copied().flatten()
                                    {
                                        keys.push(
                                            active_vals
                                                .and_then(|vals| vals.get(active_pos))
                                                .cloned()
                                                .unwrap_or(Value::Null),
                                        );
                                    } else {
                                        keys.push(Value::Null);
                                    }
                                }
                                // Tiebreaker: fewer active columns = higher grouping level = sort later
                                keys.push(Value::Int(neg_len_i32(active_set.len())));
                                keys
                            };
                            let mut output_row = agg_row;
                            if num_output_aggs < output_row.values.len() {
                                output_row.values.truncate(num_output_aggs);
                            }
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
                        if let Some(limit) = effective_limit {
                            let topk = clamp_u64_to_usize(
                                limit.saturating_add(offset_val),
                                result_rows.len(),
                            );
                            sort_query_rows_bounded(&mut result_rows, order_by, topk, context)?;
                        } else {
                            sort_query_rows(&mut result_rows, order_by, context)?;
                        }
                    } else if grouping_sets.len() > 1 {
                        // Apply natural grouping-sets ordering: sort by group-by
                        // column values with NULLs last, then by grouping level.
                        let num_gb = group_by.len();
                        let error: std::cell::RefCell<Option<DbError>> =
                            std::cell::RefCell::new(None);
                        result_rows.sort_by(|a, b| {
                            if error.borrow().is_some() {
                                return std::cmp::Ordering::Equal;
                            }
                            if let Err(e) = context.check_deadline() {
                                *error.borrow_mut() = Some(e);
                                return std::cmp::Ordering::Equal;
                            }
                            for i in 0..num_gb {
                                if i >= a.sort_keys.len() || i >= b.sort_keys.len() {
                                    break;
                                }
                                match compare_sort_values(
                                    &a.sort_keys[i],
                                    &b.sort_keys[i],
                                    false,
                                    Some(false),
                                ) {
                                    Ok(std::cmp::Ordering::Equal) => continue,
                                    Ok(ord) => return ord,
                                    Err(e) => {
                                        *error.borrow_mut() = Some(e);
                                        return std::cmp::Ordering::Equal;
                                    }
                                }
                            }
                            // Tiebreaker: grouping level (stored as negative active_set.len())
                            let a_level = if num_gb < a.sort_keys.len() {
                                &a.sort_keys[num_gb]
                            } else {
                                &Value::Null
                            };
                            let b_level = if num_gb < b.sort_keys.len() {
                                &b.sort_keys[num_gb]
                            } else {
                                &Value::Null
                            };
                            match compare_sort_values(a_level, b_level, false, Some(false)) {
                                Ok(ord) => ord,
                                Err(e) => {
                                    *error.borrow_mut() = Some(e);
                                    std::cmp::Ordering::Equal
                                }
                            }
                        });
                        if let Some(e) = error.into_inner() {
                            return Err(e);
                        }
                    }

                    let mut rows = result_rows
                        .into_iter()
                        .map(|entry| entry.row)
                        .collect::<Vec<_>>();

                    if window_eval::has_window_functions(aggregates) {
                        window_eval::evaluate_post_aggregate_windows(
                            self, aggregates, &mut rows, context,
                        )?;
                    }

                    if *distinct {
                        dedup_rows_by_value_hash(&mut rows, context)?;
                    }

                    if offset_val > 0 {
                        let skip = clamp_u64_to_usize(offset_val, rows.len());
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

                // ── Standard (non-grouping-sets) path (AggregateSource) ──
                let mut groups: std::collections::HashMap<Vec<ValueHashKey>, usize> =
                    std::collections::HashMap::new();
                let mut ordered_groups: Vec<Vec<AggAccumulator>> = Vec::new();

                if !can_skip_scalar_group_input_scan(
                    group_by,
                    aggregates,
                    having.as_ref(),
                    order_by,
                ) {
                    let mut aggregate_row = |row: Row| -> DbResult<bool> {
                        context.check_deadline()?;

                        if !predicate_matches(
                            filter
                                .as_ref()
                                .map(|f| self.evaluate_expr_with_row(f, &row, context)),
                        )? {
                            return Ok(true);
                        }

                        let mut group_key: Vec<ValueHashKey> = Vec::with_capacity(group_by.len());
                        for gb_expr in group_by {
                            let val = self.evaluate_expr_with_row(gb_expr, &row, context)?;
                            group_key.push(build_hash_key(&val)?);
                        }

                        let group_idx = match groups.entry(group_key) {
                            std::collections::hash_map::Entry::Occupied(o) => *o.get(),
                            std::collections::hash_map::Entry::Vacant(v) => {
                                context
                                    .track_memory(estimate_row_bytes(&row).saturating_add(64))?;
                                let group_idx = ordered_groups.len();
                                ordered_groups.push(
                                    agg_templates
                                        .iter()
                                        .map(AggAccumulator::from_template)
                                        .collect(),
                                );
                                v.insert(group_idx);
                                group_idx
                            }
                        };
                        let accumulators = ordered_groups.get_mut(group_idx).ok_or_else(|| {
                            DbError::internal(
                                "missing accumulator group during aggregate evaluation",
                            )
                        })?;

                        for (acc, template) in accumulators.iter_mut().zip(agg_templates.iter()) {
                            if let Some(ref filter_expr) = template.filter {
                                let filter_val =
                                    self.evaluate_expr_with_row(filter_expr, &row, context)?;
                                if !matches!(filter_val, Value::Boolean(true)) {
                                    continue;
                                }
                            }
                            self.accumulate_value(acc, template, &row, context)?;
                        }
                        Ok(true)
                    };
                    self.for_each_join_child_row(source, context, &mut aggregate_row)?;
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

                let has_ordering = !order_by.is_empty();
                let offset_val = offset
                    .as_ref()
                    .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
                    .transpose()?
                    .unwrap_or(0);
                let has_offset = offset_val > 0;
                let mut result_rows: Vec<SortedQueryRow> = Vec::new();
                let mut result_bytes = 0u64;
                let mut all_projections: Vec<AggregateExprRef<'_>> = Vec::with_capacity(
                    aggregates.len() + extra_agg_exprs.len() + hidden_group_exprs.len(),
                );
                all_projections.extend(aggregates.iter().map(AggregateExprRef::from_projection));
                all_projections.extend(extra_agg_exprs.iter().cloned());
                all_projections.extend(hidden_group_exprs.iter().cloned());

                for accumulators in &ordered_groups {
                    context.check_deadline()?;
                    let mut finalized_values = Vec::with_capacity(accumulators.len());
                    for (acc, template) in accumulators.iter().zip(agg_templates.iter()) {
                        finalized_values.push(finalize_accumulator(
                            acc,
                            template,
                            &self.evaluator,
                            context,
                        )?);
                    }
                    let agg_row = Row::new(finalized_values);

                    if let Some(having_expr) = having {
                        let having_val = self.evaluate_having_expr_extended(
                            having_expr,
                            &agg_row,
                            &all_projections,
                            context,
                        )?;
                        match having_val {
                            Value::Boolean(true) => {}
                            Value::Boolean(false) | Value::Null => continue,
                            _ => {
                                return Err(DbError::internal(
                                    "HAVING expression did not evaluate to BOOLEAN",
                                ));
                            }
                        }
                    }

                    if !has_ordering
                        && !has_offset
                        && effective_limit.is_some_and(|lim| usize_to_u64(result_rows.len()) >= lim)
                    {
                        break;
                    }

                    if usize_to_u64(result_rows.len()) >= context.max_result_rows {
                        return Err(DbError::program_limit(
                            "maximum number of result rows reached",
                        ));
                    }

                    let sort_keys: Vec<Value> = order_by
                        .iter()
                        .map(|sort| {
                            evaluate_post_aggregate_sort_expr_fast(
                                self,
                                &sort.expr,
                                &agg_row,
                                &all_projections,
                                context,
                            )
                        })
                        .collect::<DbResult<Vec<_>>>()?;
                    let mut output_row = agg_row;
                    if num_output_aggs < output_row.values.len() {
                        output_row.values.truncate(num_output_aggs);
                    }
                    push_sorted_query_row(
                        &mut result_rows,
                        context,
                        output_row,
                        sort_keys,
                        &mut result_bytes,
                    )?;
                }

                if has_ordering {
                    if let Some(limit) = effective_limit {
                        let topk =
                            clamp_u64_to_usize(limit.saturating_add(offset_val), result_rows.len());
                        sort_query_rows_bounded(&mut result_rows, order_by, topk, context)?;
                    } else {
                        sort_query_rows(&mut result_rows, order_by, context)?;
                    }
                }

                let mut rows = result_rows
                    .into_iter()
                    .map(|entry| entry.row)
                    .collect::<Vec<_>>();

                if window_eval::has_window_functions(aggregates) {
                    window_eval::evaluate_post_aggregate_windows(
                        self, aggregates, &mut rows, context,
                    )?;
                }

                if *distinct {
                    dedup_rows_by_value_hash(&mut rows, context)?;
                }

                if offset_val > 0 {
                    let skip = clamp_u64_to_usize(offset_val, rows.len());
                    rows.drain(..skip);
                }

                if let Some(limit) = effective_limit {
                    rows.truncate(clamp_u64_to_usize(limit, rows.len()));
                }

                Ok(ExecutionResult::Query {
                    columns: plan.output_fields(),
                    rows,
                })
            }
            PhysicalPlan::SetOperation {
                op,
                all,
                left,
                right,
                output_fields,
                order_by,
                limit,
                offset,
            } => self.execute_set_operation_plan(
                op,
                *all,
                left,
                right,
                output_fields,
                order_by,
                limit.as_ref(),
                offset.as_ref(),
                context,
            ),
            PhysicalPlan::DistributedAppend {
                fragments,
                output_fields,
                order_by,
                limit,
                offset,
            } => self.execute_distributed_append_plan(
                fragments,
                output_fields,
                order_by,
                limit.as_ref(),
                offset.as_ref(),
                context,
            ),
            _ => Err(DbError::internal(
                "non-aggregate/set plan routed to aggregate/set executor",
            )),
        }
    }
}
